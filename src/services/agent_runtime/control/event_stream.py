"""Runtime event streaming and child-run projection.

ARC-02 split of ``control_plane.py``: SSE frame iteration for turn and thread
event streams, plus the subagent-lifecycle projection for child runs.  The
functions take the control plane as their first argument and are bound onto
``AgentRuntimeControlPlane`` by the facade.
"""

from __future__ import annotations

import contextlib
import json
import logging
import uuid
from collections.abc import AsyncIterator, Callable
from typing import TYPE_CHECKING, Any

from .http_headers import runtime_headers
from .types import AgentRuntimeControlError, AgentTurn

if TYPE_CHECKING:
    from ..control_plane import AgentRuntimeControlPlane

logger = logging.getLogger(__name__)


def project_child_runtime_event(
    envelope: dict[str, Any], parent_turn_id: str
) -> dict[str, Any] | None:
    """Project real child runs into the stable Assistant subagent vocabulary."""

    data = envelope.get("data")
    if not isinstance(data, dict):
        return None
    run_id = str(data.get("run_id") or "")
    if not run_id:
        return None
    if run_id == parent_turn_id:
        return envelope
    agent_id = str(data.get("thread_id") or "")
    if not agent_id:
        return None
    event_type = str(envelope.get("event_type") or "")
    common = {
        "agent_id": agent_id,
        "agent_type": "task",
        "call_id": run_id,
        "parent_task_id": parent_turn_id,
        "task_id": run_id,
        "session_id": data.get("session_id"),
        "thread_id": agent_id,
    }
    if event_type == "run_started":
        return {
            **envelope,
            "event_type": "subagent_started",
            "data": {
                **common,
                "description": "Delegated child task",
                "status": "running",
            },
        }
    if event_type in {"run_finished", "run_error", "cancelled"}:
        raw_status = str(data.get("status") or "failed").lower()
        status = (
            "completed"
            if event_type == "run_finished" and raw_status in {"completed", "succeeded"}
            else "cancelled"
            if raw_status == "cancelled" or event_type == "cancelled"
            else "failed"
        )
        return {
            **envelope,
            "event_type": "subagent_finished",
            "data": {**common, "status": status, "result": data.get("exit")},
        }
    if event_type == "text_delta" and isinstance(data.get("content"), str):
        return {
            **envelope,
            "event_type": "subagent_text_delta",
            "data": {**common, "content": data["content"], "status": "running"},
        }
    return None


async def stream_events(
    plane: AgentRuntimeControlPlane,
    *,
    turn: AgentTurn,
    tenant_id: str,
    user_id: str,
    session_id: str,
    _logger: logging.Logger = logger,
) -> AsyncIterator[bytes]:
    url = f"{plane.runtime_url}/internal/v1/threads/{turn.runtime_thread_id}/events"
    headers = runtime_headers(
        plane,
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        run_id=turn.run_id,
        turn_id=turn.run_id,
    )
    terminal_status: str | None = None
    async with plane.http_client.stream(
        "GET",
        url,
        headers=headers,
        params={"after_sequence": turn.after_sequence, "limit": 1000},
    ) as response:
        if response.status_code >= 400:
            # An event observer rejection is not a Runtime terminal fact.
            # Leave execution lifetime to its existing owners; a configured
            # capacity watcher requires durable terminal/cancellation evidence.
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_EVENT_STREAM_FAILED", status_code=503
            )
        frame: list[str] = []
        async for line in response.aiter_lines():
            if line:
                frame.append(line)
                continue
            if not frame:
                continue
            current_frame = frame
            frame = []
            encoded = ("\n".join(current_frame) + "\n\n").encode()
            event_type = next(
                (value[6:].strip() for value in current_frame if value.startswith("event:")),
                "",
            )
            data_raw = next(
                (value[5:].strip() for value in current_frame if value.startswith("data:")),
                "",
            )
            if data_raw:
                with contextlib.suppress(json.JSONDecodeError):
                    event = json.loads(data_raw)
                    event_data = event.get("data") if isinstance(event, dict) else None
                    if (
                        isinstance(event_data, dict)
                        and str(event_data.get("run_id") or "") == turn.run_id
                    ):
                        if event_type == "run_started":
                            event_data.update(
                                {
                                    "requested_reasoning_option": turn.requested_reasoning_option,
                                    "effective_reasoning_option": turn.effective_reasoning_option,
                                    "reasoning_adapter_id": turn.reasoning_adapter_id,
                                    "capability_revision": turn.capability_revision,
                                    "reasoning_fallback_reason": turn.fallback_reason,
                                    "kernel": "agent",
                                }
                            )
                            encoded_data = json.dumps(
                                event,
                                ensure_ascii=False,
                                separators=(",", ":"),
                            )
                            encoded = (
                                "\n".join(
                                    f"data: {encoded_data}"
                                    if value.startswith("data:")
                                    else value
                                    for value in current_frame
                                )
                                + "\n\n"
                            ).encode()
                        if event_type in {"run_finished", "run_error", "cancelled"}:
                            terminal_status = "cancelled" if event_type == "cancelled" else str(event_data.get("status") or "failed")
                    elif event_type in {"run_finished", "run_error", "cancelled"}:
                        _logger.warning(
                            "Terminal %s not matched to turn run_id=%s (event run_id=%r); "
                            "V1 stream will not close on it",
                            event_type,
                            turn.run_id,
                            (event_data or {}).get("run_id")
                            if isinstance(event_data, dict)
                            else None,
                        )
            if terminal_status:
                # Clients can disconnect immediately after terminal delivery.
                # Release the run and model lease before exposing that event.
                await plane._complete_run(uuid.UUID(turn.run_id), terminal_status)
            yield encoded
            if terminal_status:
                break


async def stream_thread_events(
    plane: AgentRuntimeControlPlane,
    *,
    runtime_thread_id: str,
    tenant_id: str,
    user_id: str,
    session_id: str,
    after_sequence: int = 0,
    limit: int = 1000,
    turn_id: str | None = None,
    _projector: Callable[
        [dict[str, Any], str], dict[str, Any] | None
    ] = project_child_runtime_event,
) -> AsyncIterator[dict[str, Any]]:
    """Stream Runtime replay + live broadcast without Gateway DB polling."""
    url = f"{plane.runtime_url}/internal/v1/threads/{runtime_thread_id}/events"
    headers = runtime_headers(
        plane,
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        run_id=turn_id,
        turn_id=turn_id,
    )
    frame: list[str] = []
    terminal_status: str | None = None
    async with plane.http_client.stream(
        "GET",
        url,
        headers=headers,
        params={
            "after_sequence": max(0, int(after_sequence)),
            "limit": max(1, min(int(limit), 1000)),
        },
    ) as response:
        if response.status_code >= 400:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_EVENT_STREAM_FAILED",
                status_code=503 if response.status_code >= 500 else response.status_code,
            )
        async for line in response.aiter_lines():
            if line:
                frame.append(line)
                continue
            if not frame:
                continue
            data_raw = next(
                (value[5:].strip() for value in frame if value.startswith("data:")), ""
            )
            frame = []
            if not data_raw:
                continue
            with contextlib.suppress(json.JSONDecodeError):
                envelope = json.loads(data_raw)
                if not isinstance(envelope, dict):
                    continue
                event_data = envelope.get("data")
                if turn_id:
                    projected = _projector(envelope, turn_id)
                    if projected is None:
                        continue
                    envelope = projected
                    event_data = envelope.get("data")
                event_type = str(envelope.get("event_type") or "")
                # A stream without a turn filter is a durable whole-thread
                # cursor. An older turn reaching terminal state must not hide
                # later turns from backlog replay or close the live stream.
                if event_type in {"run_finished", "run_error", "cancelled"} and isinstance(event_data, dict):
                    event_run_id = str(event_data.get("run_id") or "")
                    try:
                        completed_id = uuid.UUID(event_run_id)
                    except ValueError:
                        completed_id = None
                    scoped = turn_id == event_run_id if turn_id else False
                    if completed_id is not None and not turn_id:
                        scoped = bool(await plane.database.fetchrow(
                            "SELECT run_id FROM assistant_runs WHERE run_id=$1 AND tenant_id=$2 "
                            "AND user_id=$3 AND session_id=$4 AND harness_thread_id=$5 AND engine='agent_runtime'",
                            completed_id, tenant_id, user_id, session_id, uuid.UUID(runtime_thread_id),
                        ))
                    if completed_id is not None and scoped:
                        status = "cancelled" if event_type == "cancelled" else str(event_data.get("status") or "failed")
                        await plane._complete_run(completed_id, status)
                        if turn_id:
                            terminal_status = status
                yield envelope
                if terminal_status:
                    break


__all__ = [
    "project_child_runtime_event",
    "stream_events",
    "stream_thread_events",
]
