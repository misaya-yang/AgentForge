"""GW1: security headers must not wrap streams in function-style call_next."""

from __future__ import annotations

import inspect

import pytest

from src.core.middleware._streaming.security_headers import SecurityHeadersMiddleware
from src.core.middleware.streaming import is_streaming_path


def test_security_headers_middleware_does_not_use_call_next() -> None:
    source = inspect.getsource(SecurityHeadersMiddleware.__call__)
    assert "await call_next" not in source
    assert "await self.app(scope, receive, send_with_headers)" in source


def test_assistant_chat_and_responses_are_streaming_paths() -> None:
    assert is_streaming_path("/api/v1/assistant/chat/stream") is True
    assert is_streaming_path("/v1/responses") is True


@pytest.mark.asyncio
async def test_security_headers_emit_first_body_before_handler_returns() -> None:
    sent: list[tuple[dict[str, object], bool]] = []
    handler_finished = False

    async def app(scope, receive, send):  # type: ignore[no-untyped-def]
        del scope, receive
        await send({"type": "http.response.start", "status": 200, "headers": []})
        await send({"type": "http.response.body", "body": b"chunk", "more_body": True})
        nonlocal handler_finished
        handler_finished = True
        await send({"type": "http.response.body", "body": b"", "more_body": False})

    async def send(message: dict[str, object]) -> None:
        sent.append((message, handler_finished))

    async def receive() -> dict[str, object]:
        return {"type": "http.request", "body": b"", "more_body": False}

    await SecurityHeadersMiddleware(app)(
        {"type": "http", "path": "/api/v1/assistant/chat/stream"},
        receive,
        send,
    )

    first_body = next(item for item in sent if item[0]["type"] == "http.response.body")
    assert first_body[1] is False
    start = next(item for item in sent if item[0]["type"] == "http.response.start")
    names = {key.lower() for key, _value in start[0]["headers"]}  # type: ignore[misc]
    assert b"x-content-type-options" in names
    assert b"x-frame-options" in names
