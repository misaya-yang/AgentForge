"""Generic SSE heartbeat wrapper for streaming pass-through proxies.

Long upstream tool calls (Gemini image gen 60s+, KB queries 30s+, model
generation under contention) leave the SSE stream silent for stretches
that exceed nginx's default proxy_read_timeout, ALB idle timeout, mobile
NAT keepalive windows. The browser-side EventSource then fires
``ERR_CONNECTION_RESET`` mid-flight and the model's final response never
reaches the user.

The fix is a constant ``: heartbeat\\n\\n`` SSE comment line every N
seconds of producer silence — invisible to spec-compliant EventSource
consumers (lines starting with ``:`` are dropped per the SSE spec) but
keeps every TCP/HTTP intermediary's keepalive timer alive.

This proxy-layer implementation wraps an
``async generator`` of bytes (or strings) coming back from a pass-
through ``client.stream(...)`` so the gateway's SSE proxies for agent-
agent and other LangGraph upstreams get the same protection without
having to edit each upstream's code.

Usage::

    async def upstream_gen():
        async with client.stream(...) as resp:
            async for chunk in resp.aiter_raw():
                yield chunk

    return StreamingResponse(
        with_sse_heartbeat(upstream_gen(), interval_seconds=15.0),
        media_type="text/event-stream",
    )
"""

from __future__ import annotations

import asyncio
import contextlib
import sys
from collections.abc import AsyncIterator
from typing import Any

# 15s is a safe default — beats most CDN / proxy / NAT idle timeouts
# (commonly 30s–60s) by a comfortable margin without flooding the wire.
DEFAULT_HEARTBEAT_INTERVAL_S: float = 15.0

# SSE comment line. ``:`` prefix marks the line as a comment per the SSE
# spec — well-behaved EventSource clients silently discard it. The
# trailing ``\n\n`` closes the SSE event frame so any client buffer that
# accumulates by event boundary still sees activity.
_HEARTBEAT_FRAME_BYTES = b": heartbeat\n\n"
_HEARTBEAT_FRAME_STR = ": heartbeat\n\n"


async def with_sse_heartbeat(
    upstream: AsyncIterator[Any],
    *,
    interval_seconds: float = DEFAULT_HEARTBEAT_INTERVAL_S,
    as_str: bool = False,
) -> AsyncIterator[Any]:
    """Yield from ``upstream`` while injecting periodic heartbeats.

    The wrapped iterator yields:
      * Each item the upstream produces, exactly as produced.
      * A heartbeat frame every ``interval_seconds`` of producer silence.
        The frame type is bytes by default (proxy ``aiter_raw`` is the
        common caller). Pass ``as_str=True`` if your producer yields
        ``str`` chunks so heartbeats match — mixing types would corrupt
        the stream when StreamingResponse encodes.

    Cancellation: when the consumer disconnects or stops iterating, the
    producer task is cancelled cleanly. Exceptions raised inside the
    upstream propagate to the consumer.
    """
    heartbeat_payload: Any = _HEARTBEAT_FRAME_STR if as_str else _HEARTBEAT_FRAME_BYTES
    at_sse_frame_boundary = True
    trailing_bytes = b""
    trailing_str = ""

    def _update_frame_boundary(chunk: Any) -> None:
        nonlocal at_sse_frame_boundary, trailing_bytes, trailing_str

        if isinstance(chunk, bytes):
            trailing_bytes = (trailing_bytes + chunk)[-4:]
            at_sse_frame_boundary = trailing_bytes.endswith(b"\n\n") or trailing_bytes.endswith(
                b"\r\n\r\n"
            )
            return

        if isinstance(chunk, str):
            trailing_str = (trailing_str + chunk)[-4:]
            at_sse_frame_boundary = trailing_str.endswith("\n\n") or trailing_str.endswith(
                "\r\n\r\n"
            )

    # Keep exactly one upstream read in flight. Unlike wait_for(anext(...)),
    # asyncio.wait does not cancel the read when the heartbeat deadline fires.
    # Starting the following read only after the downstream asks for another
    # item also prevents a slow client from accumulating queued chunks.
    next_item: asyncio.Task[Any] | None = asyncio.create_task(anext(upstream))

    try:
        while True:
            assert next_item is not None
            done, _ = await asyncio.wait({next_item}, timeout=interval_seconds)
            if not done:
                if at_sse_frame_boundary:
                    yield heartbeat_payload
                continue

            completed = next_item
            next_item = None
            try:
                item = completed.result()
            except StopAsyncIteration:
                return

            _update_frame_boundary(item)
            yield item
            next_item = asyncio.create_task(anext(upstream))
    finally:
        primary_exception = sys.exc_info()[1]
        if next_item is not None and not next_item.done():
            next_item.cancel()
            with contextlib.suppress(asyncio.CancelledError, BaseException):
                await next_item
        close = getattr(upstream, "aclose", None)
        if callable(close):
            try:
                await close()
            except (asyncio.CancelledError, RuntimeError):
                pass
            except BaseException:
                # Preserve the provider/root failure when cleanup also fails.
                if primary_exception is None:
                    raise


__all__ = [
    "DEFAULT_HEARTBEAT_INTERVAL_S",
    "with_sse_heartbeat",
]
