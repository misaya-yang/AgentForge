"""
Chat module — streaming and non-streaming chat completions.

Usage::

    async with AssistantClient("my-key") as client:
        # Non-streaming
        resp = await client.chat.send("Hello!")

        # Streaming
        async for event in client.chat.stream("Tell me a story"):
            if event.is_text():
                print(event.text, end="", flush=True)
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from ai_assistant.exceptions import AssistantError, UnknownOutcomeError
from ai_assistant.models.request import ChatRequest
from ai_assistant.models.response import ChatResponse

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

    from ai_assistant.models.events import StreamEvent
    from ai_assistant.transport.http import HTTPTransport


class ChatModule:
    """High-level chat operations backed by ``/api/v1/assistant/chat``."""

    _CHAT_PATH = "/api/v1/assistant/chat"
    _STREAM_PATH = "/api/v1/assistant/chat/stream"
    _CANCEL_PATH = "/api/v1/assistant/tasks/{task_id}/cancel"

    def __init__(self, transport: HTTPTransport) -> None:
        self._transport = transport

    async def send(
        self,
        message: str,
        *,
        session_id: str | None = None,
        model_id: str | None = None,
        temperature: float = 0.7,
        max_tokens: int | None = None,
        system_prompt: str | None = None,
        kb_dataset_ids: list[str] | None = None,
        web_search_enabled: bool = False,
        **kwargs: Any,
    ) -> ChatResponse:
        """Send a non-streaming chat request and return the full response.

        All keyword arguments are forwarded to ``ChatRequest``.
        """
        req = ChatRequest(
            message=message,
            session_id=session_id,
            model_id=model_id,
            temperature=temperature,
            max_tokens=max_tokens,
            system_prompt=system_prompt,
            kb_dataset_ids=kb_dataset_ids,
            web_search_enabled=web_search_enabled,
            stream=False,
            **kwargs,
        )
        resp = await self._transport.request("POST", self._CHAT_PATH, json=req.to_dict())
        return ChatResponse.from_dict(resp.json())

    async def stream(
        self,
        message: str,
        *,
        session_id: str | None = None,
        model_id: str | None = None,
        temperature: float = 0.7,
        max_tokens: int | None = None,
        system_prompt: str | None = None,
        kb_dataset_ids: list[str] | None = None,
        web_search_enabled: bool = False,
        **kwargs: Any,
    ) -> AsyncIterator[StreamEvent]:
        """Start a streaming chat and yield ``StreamEvent`` objects.

        The returned async iterator stays open until the server sends a
        ``done`` event or closes the connection.
        """
        req = ChatRequest(
            message=message,
            session_id=session_id,
            model_id=model_id,
            temperature=temperature,
            max_tokens=max_tokens,
            system_prompt=system_prompt,
            kb_dataset_ids=kb_dataset_ids,
            web_search_enabled=web_search_enabled,
            stream=True,
            **kwargs,
        )
        events = self._transport.stream_sse(self._STREAM_PATH, req.to_dict())
        body_failed = False
        try:
            async for event in events:
                yield event
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

    async def cancel(self, task_id: str, *, reason: str | None = None) -> dict[str, Any]:
        """Request cancellation once, then inspect the run to learn its final state.

        ``task_id`` is the managed event task ID or genuine SDK assistant_run_id.
        A positive acknowledgement does not prove remote effects stopped or usage
        settled. Unknown outcomes raise ``UnknownOutcomeError`` without retrying.
        """
        if (
            not isinstance(task_id, str) or not task_id or len(task_id) > 160
            or task_id in {".", ".."}
            or any(char.isspace() or ord(char) < 32 for char in task_id)
            or any(char in task_id for char in "/\\?#%")
        ):
            raise ValueError("Invalid cancellation task ID")
        path = self._CANCEL_PATH.format(task_id=task_id)
        body: dict[str, Any] = {}
        if reason:
            body["reason"] = reason
        try:
            resp = await self._transport.request("POST", path, json=body)
        except AssistantError as exc:
            status = exc.status_code
            if status is not None and 400 <= status < 500 and status != 408:
                raise
            raise UnknownOutcomeError(
                "Cancellation outcome unknown; read the run before deciding to retry",
                status_code=status, request_id=exc.request_id,
            ) from exc
        try:
            value = resp.json()
            if (
                not isinstance(value, dict) or value.get("task_id") != task_id
                or type(value.get("cancelled")) is not bool
            ):
                raise ValueError("Invalid cancellation response")
        except ValueError as exc:
            raise UnknownOutcomeError(
                "Cancellation outcome unknown; read the run before deciding to retry"
            ) from exc
        # Preserve the existing response shape, including the Gateway's message.
        return value
