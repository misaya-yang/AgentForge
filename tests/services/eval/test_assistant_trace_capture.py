from __future__ import annotations

import asyncio
import json
import uuid

import pytest

from src.services.eval import assistant_trace_capture


def _frame(event_type: str, data: dict) -> bytes:
    envelope = json.dumps({"event_type": event_type, "data": data})
    return f"event: {event_type}\ndata: {envelope}\n\n".encode()


@pytest.mark.asyncio
async def test_runtime_trace_capture_forwards_stream_and_schedules_terminal_trace(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    run_id = str(uuid.uuid4())
    frames = [
        _frame("text_delta", {"run_id": run_id, "content": "hello"}),
        _frame(
            "run_finished",
            {
                "run_id": run_id,
                "status": "completed",
                "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5},
            },
        ),
    ]
    scheduled: list[dict] = []

    def record(_database, **kwargs):
        scheduled.append(kwargs)

    monkeypatch.setattr(assistant_trace_capture, "schedule_gateway_trace_ingest", record)

    async def source():
        for frame in frames:
            yield frame

    forwarded = [
        frame
        async for frame in assistant_trace_capture.capture_assistant_runtime_stream(
            source(),
            database=object(),
            run_id=run_id,
            request_id="request-a",
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            message="hi",
            snapshot={
                "agent_id": str(uuid.uuid4()),
                "agent_version_id": None,
                "publication": {"id": None, "channel": "preview"},
                "model": {"id": "qwen3.7-plus", "provider": "dashscope"},
            },
        )
    ]

    assert forwarded == frames
    assert len(scheduled) == 1
    assert scheduled[0]["enqueue"] is True
    trace = scheduled[0]["trace"]
    assert trace["trace_id"] == run_id
    assert trace["session_id"] == "session-a"
    assert trace["status"] == "succeeded"
    assert trace["output_preview"] == "hello"
    assert trace["metrics"]["total_tokens"] == 5


@pytest.mark.asyncio
async def test_browser_detach_does_not_record_runtime_run_as_cancelled(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    run_id = str(uuid.uuid4())
    scheduled: list[dict] = []
    monkeypatch.setattr(
        assistant_trace_capture,
        "schedule_gateway_trace_ingest",
        lambda _database, **kwargs: scheduled.append(kwargs),
    )

    async def source():
        yield _frame("text_delta", {"run_id": run_id, "content": "partial"})
        yield _frame("run_finished", {"run_id": run_id, "status": "completed"})

    stream = assistant_trace_capture.capture_assistant_runtime_stream(
        source(),
        database=object(),
        run_id=run_id,
        request_id="request-a",
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        message="hi",
        snapshot={"publication": {"id": None, "channel": "preview"}},
    )
    assert await anext(stream) == _frame("text_delta", {"run_id": run_id, "content": "partial"})
    await stream.aclose()

    assert scheduled == []


@pytest.mark.asyncio
async def test_browser_detach_replays_durable_events_to_one_terminal_trace(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    run_id = str(uuid.uuid4())
    scheduled: list[dict] = []
    monkeypatch.setattr(
        assistant_trace_capture,
        "schedule_gateway_trace_ingest",
        lambda _database, **kwargs: scheduled.append(kwargs),
    )

    async def observer():
        yield _frame("text_delta", {"run_id": run_id, "content": "partial"})
        yield _frame("run_finished", {"run_id": run_id, "status": "completed"})

    async def durable_replay():
        yield _frame("text_delta", {"run_id": run_id, "content": "full answer"})
        yield _frame("run_finished", {"run_id": str(uuid.uuid4()), "status": "completed"})
        yield _frame("run_finished", {"run_id": run_id, "status": "completed"})

    stream = assistant_trace_capture.capture_assistant_runtime_stream(
        observer(), database=object(), run_id=run_id, request_id="request-a",
        tenant_id="tenant-a", user_id="user-a", session_id="session-a",
        message="hi", snapshot={"publication": {"channel": "preview"}},
        terminal_replay_source=durable_replay,
    )
    await anext(stream)
    await stream.aclose()
    assert scheduled == []
    await asyncio.gather(*assistant_trace_capture._terminal_replay_tasks)

    assert len(scheduled) == 1
    assert scheduled[0]["trace"]["status"] == "succeeded"
    assert scheduled[0]["trace"]["output_preview"] == "full answer"


@pytest.mark.asyncio
async def test_child_terminal_before_browser_detach_does_not_finish_parent_trace(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    parent_id = str(uuid.uuid4())
    scheduled: list[dict] = []
    monkeypatch.setattr(
        assistant_trace_capture,
        "schedule_gateway_trace_ingest",
        lambda _database, **kwargs: scheduled.append(kwargs),
    )

    async def source():
        yield _frame("run_finished", {"run_id": str(uuid.uuid4()), "status": "completed"})
        yield _frame("run_finished", {"run_id": parent_id, "status": "completed"})

    stream = assistant_trace_capture.capture_assistant_runtime_stream(
        source(), database=object(), run_id=parent_id, request_id="request-a",
        tenant_id="tenant-a", user_id="user-a", session_id="session-a",
        message="hi", snapshot={"publication": {"channel": "preview"}},
    )
    await anext(stream)
    await stream.aclose()

    assert scheduled == []


@pytest.mark.asyncio
async def test_parent_terminal_delivered_then_browser_closes_still_records_terminal_trace(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    run_id = str(uuid.uuid4())
    scheduled: list[dict] = []
    monkeypatch.setattr(
        assistant_trace_capture,
        "schedule_gateway_trace_ingest",
        lambda _database, **kwargs: scheduled.append(kwargs),
    )

    async def source():
        yield _frame("run_finished", {"run_id": run_id, "status": "completed"})
        yield _frame("text_delta", {"run_id": run_id, "content": "late"})

    stream = assistant_trace_capture.capture_assistant_runtime_stream(
        source(), database=object(), run_id=run_id, request_id="request-a",
        tenant_id="tenant-a", user_id="user-a", session_id="session-a",
        message="hi", snapshot={"publication": {"channel": "preview"}},
    )
    await anext(stream)
    await stream.aclose()

    assert len(scheduled) == 1
    assert scheduled[0]["trace"]["status"] == "succeeded"
    assert scheduled[0]["trace"]["ended_at"] is not None
    assert scheduled[0]["trace"]["metadata"]["error_type"] is None
