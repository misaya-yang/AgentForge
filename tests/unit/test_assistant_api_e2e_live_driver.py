from __future__ import annotations

import json
from collections.abc import Iterator
from typing import Any

from tests.integration import test_assistant_api_e2e_live as live_e2e


class _DecisionResponse:
    status_code = 200
    text = ""

    def __init__(self, approval_id: str, *, status: str) -> None:
        self._payload = {
            "approval": {
                "approval_id": approval_id,
                "approved": True,
                "status": status,
            }
        }

    def json(self) -> dict[str, Any]:
        return self._payload


class _RuntimeStream:
    status_code = 200
    text = ""

    def __init__(self, client: _LiveDriverClient, events: list[dict[str, Any]]) -> None:
        self._client = client
        self._events = events

    def __enter__(self) -> _RuntimeStream:
        return self

    def __exit__(self, *_args: object) -> None:
        return None

    def iter_lines(self) -> Iterator[str]:
        for event in self._events:
            yield f"data: {json.dumps(event)}"
            if event.get("event_type") == "approval_required":
                approval_id = str((event.get("data") or {}).get("approval_id") or "")
                assert approval_id in self._client.decided_approval_ids, (
                    "the driver tried to consume the paused Runtime stream before deciding approval"
                )


class _LiveDriverClient:
    def __init__(self, events: list[dict[str, Any]]) -> None:
        self._events = events
        self.stream_requests: list[dict[str, Any]] = []
        self.decision_requests: list[dict[str, Any]] = []
        self.decided_approval_ids: set[str] = set()

    def stream(self, method: str, url: str, **kwargs: Any) -> _RuntimeStream:
        self.stream_requests.append({"method": method, "url": url, **kwargs})
        assert len(self.stream_requests) == 1, "approval must not start a resume stream"
        return _RuntimeStream(self, self._events)

    def post(self, url: str, **kwargs: Any) -> _DecisionResponse:
        approval_id = url.rstrip("/").split("/")[-2 if url.endswith("/decision") else -1]
        self.decision_requests.append({"url": url, **kwargs})
        self.decided_approval_ids.add(approval_id)
        status = "consumed" if url.endswith("/decision") else "approved"
        return _DecisionResponse(approval_id, status=status)


def _runtime_events(*, include_thread_id: bool) -> list[dict[str, Any]]:
    approval_data = {
        "run_id": "run-1",
        "approval_id": "approval-1",
        "tool_name": "execute_python_code",
    }
    if include_thread_id:
        approval_data["thread_id"] = "thread-1"
    return [
        {"event_type": "run_started", "data": {"run_id": "run-1"}},
        {"event_type": "approval_required", "data": approval_data},
        {
            "event_type": "tool_call_result",
            "data": {"run_id": "run-1", "tool_name": "execute_python_code"},
        },
        {
            "event_type": "run_finished",
            "data": {"run_id": "run-1", "status": "succeeded"},
        },
    ]


def test_live_driver_decides_v2_approval_while_consuming_the_same_stream() -> None:
    client = _LiveDriverClient(_runtime_events(include_thread_id=True))

    events = live_e2e._stream_chat_until_success(
        client,  # type: ignore[arg-type]
        token="token",
        session_id="session-1",
        message="use the real tool",
        max_attempts=1,
    )

    assert [event["event_type"] for event in events][-2:] == [
        "tool_call_result",
        "run_finished",
    ]
    assert len(client.stream_requests) == 1
    assert "resume_run_id" not in client.stream_requests[0]["json"]
    assert "resume_approval_id" not in client.stream_requests[0]["json"]
    assert client.decision_requests == [
        {
            "url": (
                f"{live_e2e.API_BASE_URL}/api/v2/agent/threads/thread-1/"
                "approvals/approval-1/decision"
            ),
            "headers": {"Authorization": "Bearer token"},
            "json": {
                "approved": True,
                "reason": "result-level live capability check",
            },
        }
    ]


def test_live_driver_uses_v1_decision_seam_for_legacy_approval_event() -> None:
    client = _LiveDriverClient(_runtime_events(include_thread_id=False))

    events = live_e2e._stream_chat(
        client,  # type: ignore[arg-type]
        token="token",
        session_id="session-1",
        message="use the real tool",
    )

    assert events[-1]["event_type"] == "run_finished"
    assert len(client.stream_requests) == 1
    assert client.decision_requests[0]["url"] == (
        f"{live_e2e.API_PREFIX}/assistant/approvals/approval-1"
    )
