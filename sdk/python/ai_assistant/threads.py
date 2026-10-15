"""Native V2 Thread/Turn/Item SDK module (V1 remains unchanged)."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

    from ai_assistant.models.events import StreamEvent
    from ai_assistant.transport.http import HTTPTransport


def _record(value: Any) -> dict[str, Any] | None:
    return value if isinstance(value, dict) else None


def _item_text(value: Any) -> str:
    if isinstance(value, str):
        return value
    if not isinstance(value, list):
        return ""
    return "".join(
        part.get("text", part.get("content", ""))
        for part in value
        if isinstance(part, dict) and isinstance(part.get("text", part.get("content", "")), str)
    )


def project_v2_event(event: StreamEvent) -> StreamEvent | None:
    """Unwrap a V2 item envelope into the stable stream event contract."""
    from ai_assistant.models.events import StreamEvent

    if event.event_type != "item" or not isinstance(event.data, dict):
        return event
    envelope = _record(event.data.get("event")) or event.data
    raw = _record(envelope.get("payload")) or {}
    event_type = raw.get("event_type")
    data = raw.get("data")
    if isinstance(event_type, str) and event_type not in {"item", "rollout/item"}:
        return StreamEvent(
            event_type=event_type,
            data=data if isinstance(data, dict) else {"value": data},
            timestamp=event.timestamp,
        )

    item = _record(data) or raw
    payload = _record(item.get("payload")) or item
    item_type = str(item.get("type") or payload.get("type") or "")
    text = _item_text(payload.get("content")) or str(
        payload.get("message") or payload.get("text") or ""
    )
    if text and (payload.get("role") == "assistant" or payload.get("type") == "agent_message"):
        return StreamEvent(
            event_type="text_delta", data={"content": text}, timestamp=event.timestamp
        )
    if text and (payload.get("role") == "reasoning" or payload.get("type") == "reasoning"):
        return StreamEvent(
            event_type="thinking_delta", data={"content": text}, timestamp=event.timestamp
        )
    if payload.get("approval_id") or item_type == "approval_request":
        return StreamEvent(event_type="approval_required", data=payload, timestamp=event.timestamp)
    if payload.get("artifact_id") or item_type == "artifact":
        return StreamEvent(event_type="artifact_created", data=payload, timestamp=event.timestamp)
    tool_name = payload.get("name") or payload.get("tool")
    if tool_name or item_type in {
        "function_call",
        "tool_use",
        "command_execution",
        "mcp_tool_call",
    }:
        status = str(item.get("status") or payload.get("status") or "").lower()
        event_type = (
            "tool_call_result"
            if status in {"completed", "succeeded", "failed", "error", "cancelled"}
            else "tool_call_start"
        )
        return StreamEvent(
            event_type=event_type,
            data={
                "tool_call_id": item.get("id") or payload.get("id"),
                "tool_name": tool_name or item_type,
                "arguments": payload.get("arguments", payload.get("input")),
                "result": payload.get("result", payload.get("output")),
                "status": item.get("status", payload.get("status")),
            },
            timestamp=event.timestamp,
        )
    if item_type in {"activity", "event_msg"}:
        return StreamEvent(event_type="activity", data=payload, timestamp=event.timestamp)
    return None


class ThreadModule:
    _BASE = "/api/v2/agent/threads"

    def __init__(self, transport: HTTPTransport) -> None:
        self._transport = transport

    async def create(self, *, session_id: str | None = None) -> dict[str, Any]:
        body = {"session_id": session_id} if session_id else {}
        return (await self._transport.request("POST", self._BASE, json=body)).json()

    async def get(self, thread_id: str) -> dict[str, Any]:
        return (await self._transport.request("GET", f"{self._BASE}/{thread_id}")).json()

    async def turn(
        self,
        thread_id: str,
        message: str,
        *,
        model_id: str | None = None,
        reasoning_option: str | None = None,
        max_tokens: int | None = None,
        kb_dataset_ids: list[str] | None = None,
        kb_mode: str = "off",
        kb_top_k: int = 5,
        kb_score_threshold: float = 0.4,
        web_search_enabled: bool = False,
        web_search_max_results: int = 5,
        file_paths: list[str] | None = None,
    ) -> dict[str, Any]:
        body: dict[str, Any] = {"message": message}
        if model_id is not None:
            body["model_id"] = model_id
        if reasoning_option is not None:
            body["reasoning_option"] = reasoning_option
        if max_tokens is not None:
            body["max_tokens"] = max_tokens
        body.update(
            {
                "kb_dataset_ids": kb_dataset_ids or [],
                "kb_mode": kb_mode,
                "kb_top_k": kb_top_k,
                "kb_score_threshold": kb_score_threshold,
                "web_search_enabled": web_search_enabled,
                "web_search_max_results": web_search_max_results,
                "file_paths": file_paths or [],
            }
        )
        response = await self._transport.request(
            "POST", f"{self._BASE}/{thread_id}/turns", json=body
        )
        return response.json()

    async def interrupt(
        self, thread_id: str, turn_id: str, *, reason: str = "client_interrupt"
    ) -> dict[str, Any]:
        response = await self._transport.request(
            "POST",
            f"{self._BASE}/{thread_id}/turns/{turn_id}:interrupt",
            json={"reason": reason},
        )
        return response.json()

    async def get_approval(self, thread_id: str, approval_id: str) -> dict[str, Any]:
        response = await self._transport.request(
            "GET", f"{self._BASE}/{thread_id}/approvals/{approval_id}"
        )
        return response.json()

    async def decide_approval(
        self, thread_id: str, approval_id: str, *, approved: bool, reason: str | None = None
    ) -> dict[str, Any]:
        body: dict[str, Any] = {"approved": approved}
        if reason is not None:
            body["reason"] = reason
        response = await self._transport.request(
            "POST", f"{self._BASE}/{thread_id}/approvals/{approval_id}/decision", json=body
        )
        return response.json()

    async def events(
        self,
        thread_id: str,
        *,
        turn: dict[str, Any] | None = None,
        turn_id: str | None = None,
        after_sequence: int = 0,
        limit: int = 100,
    ) -> AsyncIterator[StreamEvent]:
        cursor = turn.get("turn", turn) if isinstance(turn, dict) else {}
        events_url = cursor.get("events_url") if isinstance(cursor, dict) else None
        path = str(events_url or f"{self._BASE}/{thread_id}/events")
        params = (
            None
            if events_url
            else {
                "after_sequence": after_sequence,
                "limit": limit,
                **({"turn_id": turn_id} if turn_id else {}),
            }
        )
        events = self._transport.stream_sse_get(
            path,
            params=params,
        )
        body_failed = False
        try:
            async for event in events:
                projected = project_v2_event(event)
                if projected is not None:
                    yield projected
        except BaseException as exc:
            body_failed = not isinstance(exc, GeneratorExit)
            raise
        finally:
            try:
                close = getattr(events, "aclose", None)
                if close is not None:
                    await close()
            except BaseException:
                if not body_failed:
                    raise
