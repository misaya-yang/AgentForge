"""Bounded provider line decoding without buffering for a minimum chunk size."""

from __future__ import annotations

from collections.abc import AsyncIterator

import httpx

from .authorization import AgentModelPlaneError

MAX_PROVIDER_LINE_BYTES = 2 * 1024 * 1024


async def provider_lines(response: httpx.Response) -> AsyncIterator[str]:
    pending = bytearray()
    async for chunk in response.aiter_bytes():
        # A transport may coalesce many valid frames. Bound the unfinished
        # line, not the transport chunk, and never wait for 64 KiB to arrive.
        for offset in range(0, len(chunk), 64 * 1024):
            pending.extend(chunk[offset:offset + 64 * 1024])
            lines = pending.splitlines(keepends=True)
            pending.clear()
            for line in lines:
                if len(line) > MAX_PROVIDER_LINE_BYTES:
                    raise AgentModelPlaneError("RUNTIME_PROVIDER_FRAME_LIMIT", status_code=502)
                if line.endswith((b"\r", b"\n")):
                    try:
                        yield line.rstrip(b"\r\n").decode("utf-8")
                    except UnicodeDecodeError:
                        raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502) from None
                else:
                    pending.extend(line)
    if pending:
        # Unterminated SSE data is incomplete, even if it happens to be JSON.
        raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INCOMPLETE", status_code=502)
