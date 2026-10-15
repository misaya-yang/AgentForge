from __future__ import annotations

import pytest

from ai_assistant.models.events import EventType, StreamEvent
from ai_assistant.threads import ThreadModule


class _Response:
    def __init__(self, payload):
        self._payload = payload

    def json(self):
        return self._payload


class _Transport:
    def __init__(self) -> None:
        self.calls: list[tuple[str, str, dict]] = []

    async def request(self, method, path, **kwargs):
        self.calls.append((method, path, kwargs))
        return _Response({"thread": {"thread_id": "thread-1"}})

    async def stream_sse_get(self, path, *, params):
        self.calls.append(("GET", path, {"params": params}))
        yield StreamEvent(event_type=EventType.RUN_FINISHED, data={})


@pytest.mark.asyncio
async def test_threads_module_uses_v2_paths_and_cursor() -> None:
    transport = _Transport()
    module = ThreadModule(transport)
    created = await module.create(session_id="session-1")
    assert created["thread"]["thread_id"] == "thread-1"
    await module.turn("thread-1", "hello", reasoning_option="auto")
    assert transport.calls[-1] == (
        "POST",
        "/api/v2/agent/threads/thread-1/turns",
        {"json": {
            "message": "hello",
            "reasoning_option": "auto",
            "kb_dataset_ids": [],
            "kb_mode": "off",
            "kb_top_k": 5,
            "kb_score_threshold": 0.4,
            "web_search_enabled": False,
            "web_search_max_results": 5,
            "file_paths": [],
        }},
    )
    await module.interrupt("thread-1", "turn-1")
    events = [event async for event in module.events("thread-1", after_sequence=6, limit=10)]
    assert events[0].is_done()
    assert transport.calls[-1] == (
        "GET",
        "/api/v2/agent/threads/thread-1/events",
        {"params": {"after_sequence": 6, "limit": 10}},
    )

    cursor = {
        "turn": {
            "id": "turn-1",
            "events_url": (
                "/api/v2/agent/threads/thread-1/events"
                "?after_sequence=9&turn_id=turn-1"
            ),
        }
    }
    [event async for event in module.events("thread-1", turn=cursor)]
    assert transport.calls[-1] == (
        "GET",
        cursor["turn"]["events_url"],
        {"params": None},
    )
