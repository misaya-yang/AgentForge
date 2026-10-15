from __future__ import annotations

import httpx
import pytest

from src.services.agent_runtime.model.authorization import AgentModelPlaneError
from src.services.agent_runtime.model.stream_limits import MAX_PROVIDER_LINE_BYTES, provider_lines


async def test_provider_lines_allow_large_coalesced_stream_without_unbounded_line() -> None:
    content = b'data: {"text":"ok"}\r\n\r\n' * 100000
    response = httpx.Response(200, content=content)
    lines = 0
    async for line in provider_lines(response):
        if line:
            assert line == 'data: {"text":"ok"}'
            lines += 1
    assert lines == 100000


async def test_provider_lines_reject_single_oversized_frame() -> None:
    response = httpx.Response(200, content=b"data: " + b"x" * MAX_PROVIDER_LINE_BYTES + b"\n")
    with pytest.raises(AgentModelPlaneError, match="RUNTIME_PROVIDER_FRAME_LIMIT"):
        _ = [line async for line in provider_lines(response)]


async def test_provider_lines_reject_unterminated_frame() -> None:
    response = httpx.Response(200, content=b'data: {"text":"partial"}')
    with pytest.raises(AgentModelPlaneError, match="RUNTIME_PROVIDER_STREAM_INCOMPLETE"):
        _ = [line async for line in provider_lines(response)]
