"""Behavior of the proxy-layer SSE heartbeat wrapper."""

from __future__ import annotations

import asyncio

import pytest
from ai_gateway_core.proxy.sse_heartbeat import (
    DEFAULT_HEARTBEAT_INTERVAL_S,
    with_sse_heartbeat,
)

# ----- Helpers --------------------------------------------------------------


async def _drain(it):
    out = []
    async for v in it:
        out.append(v)
    return out


# ----- Tests ----------------------------------------------------------------


async def test_default_interval_is_15s():
    assert DEFAULT_HEARTBEAT_INTERVAL_S == 15.0


async def test_passthrough_when_producer_is_fast_yields_no_heartbeats():
    async def fast_gen():
        for i in range(3):
            yield f"data: {i}\n\n".encode()

    out = await _drain(with_sse_heartbeat(fast_gen(), interval_seconds=1.0))
    assert out == [b"data: 0\n\n", b"data: 1\n\n", b"data: 2\n\n"]


async def test_idle_producer_emits_heartbeat_lines():
    async def slow_gen():
        # No production for ~3 heartbeat intervals, then one chunk.
        await asyncio.sleep(0.18)
        yield b"data: ok\n\n"

    out = await _drain(with_sse_heartbeat(slow_gen(), interval_seconds=0.05))
    heartbeats = [c for c in out if c == b": heartbeat\n\n"]
    payload = [c for c in out if c == b"data: ok\n\n"]
    assert len(heartbeats) >= 2
    assert len(payload) == 1


async def test_heartbeat_waits_for_sse_frame_boundary():
    async def partial_gen():
        yield b"event: update\n"
        await asyncio.sleep(0.14)
        yield b'data: {"ok": true}\n\n'

    out = await _drain(with_sse_heartbeat(partial_gen(), interval_seconds=0.04))
    assert out == [b"event: update\n", b'data: {"ok": true}\n\n']


async def test_heartbeat_resumes_after_complete_sse_frame():
    async def boundary_gen():
        yield b"data: first\n\n"
        await asyncio.sleep(0.12)
        yield b"data: second\n\n"

    out = await _drain(with_sse_heartbeat(boundary_gen(), interval_seconds=0.04))
    assert out[0] == b"data: first\n\n"
    assert b": heartbeat\n\n" in out[1:-1]
    assert out[-1] == b"data: second\n\n"


async def test_heartbeat_payload_matches_upstream_string_type_when_opted_in():
    async def str_gen():
        await asyncio.sleep(0.1)
        yield "data: hello\n\n"

    out = await _drain(
        with_sse_heartbeat(str_gen(), interval_seconds=0.04, as_str=True)
    )
    # Every yielded item is a str — no bytes mixed in.
    assert all(isinstance(c, str) for c in out), out
    assert ": heartbeat\n\n" in out


async def test_heartbeat_default_is_bytes():
    async def empty_gen():
        await asyncio.sleep(0.1)
        # produce nothing else
        return
        yield  # pragma: no cover  (unreachable; makes this an async generator)

    out = await _drain(with_sse_heartbeat(empty_gen(), interval_seconds=0.04))
    assert out, "expected at least one heartbeat before generator finished"
    assert all(isinstance(c, bytes) for c in out)
    assert b": heartbeat\n\n" in out


async def test_producer_exception_propagates():
    async def broken_gen():
        yield b"data: a\n\n"
        raise RuntimeError("upstream fell over")

    pieces = []
    with pytest.raises(RuntimeError, match="upstream fell over"):
        async for chunk in with_sse_heartbeat(broken_gen(), interval_seconds=1.0):
            pieces.append(chunk)
    assert pieces == [b"data: a\n\n"]


async def test_close_failure_does_not_mask_upstream_failure():
    class BrokenIterator:
        def __aiter__(self):
            return self

        async def __anext__(self):
            raise ValueError("primary provider failure")

        async def aclose(self):
            raise OSError("secondary close failure")

    with pytest.raises(ValueError, match="primary provider failure"):
        await _drain(with_sse_heartbeat(BrokenIterator(), interval_seconds=1.0))


async def test_consumer_disconnect_closes_upstream_cleanly_once():
    closed = asyncio.Event()
    close_count = 0

    async def long_gen():
        nonlocal close_count
        try:
            for _ in range(100):
                await asyncio.sleep(0.02)
                yield b"data: x\n\n"
        finally:
            close_count += 1
            closed.set()

    it = with_sse_heartbeat(long_gen(), interval_seconds=10.0)
    # Pull the first event then close the iterator (simulates client disconnect).
    first = await it.__anext__()
    assert first == b"data: x\n\n"
    await it.aclose()
    # Allow the producer task to settle.
    await asyncio.sleep(0.05)
    assert closed.is_set()
    assert close_count == 1


async def test_slow_consumer_never_allows_upstream_to_run_more_than_one_chunk_ahead():
    produced = 0
    consumed = 0
    maximum_ahead = 0

    async def large_gen():
        nonlocal produced, maximum_ahead
        for index in range(5_000):
            produced += 1
            maximum_ahead = max(maximum_ahead, produced - consumed)
            yield f"data: {index}\n\n".encode()

    iterator = with_sse_heartbeat(large_gen(), interval_seconds=10.0)
    try:
        for _ in range(5):
            await iterator.__anext__()
            consumed += 1
            await asyncio.sleep(0.01)
        assert maximum_ahead <= 1
        assert produced == consumed
    finally:
        await iterator.aclose()
