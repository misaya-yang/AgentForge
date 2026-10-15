"""PPR-00 additive timing schema: identity, attribution and tolerance tests.

Covers the pre-declared phase-00 gates:

- G1 additive identity (fake-clock exact, real-clock within 5 ms)
- G2 controlled-delay attribution (pre-dispatch delay never leaks into
  provider_wait; post-first-frame delay never leaks into provider_wait)
- G3 client reconciliation tolerance boundary matrix
- G4 is proven by the untouched ``test_model_plane.py`` suite passing.

Everything runs against an injected fake clock and httpx.MockTransport — no
Docker, no provider network, no sleeps (except the one real-clock G1 test).
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
import time
import uuid
from collections.abc import AsyncIterator
from typing import Any

import httpx
import pytest
from ai_gateway_contracts.agent_runtime_lease import RuntimeModelLeaseSigner
from ai_gateway_core.models import get_builtin_model_capabilities

from src.services.agent_runtime.model_plane import AgentModelPlane, _AuthorizedCall
from src.services.agent_runtime.timing import (
    REAL_CLOCK_IDENTITY_TOLERANCE_SECONDS,
    ModelPlaneTiming,
    client_residual_within_tolerance,
)

logger_name = "src.services.agent_runtime.model_plane"


class FakeClock:
    """Deterministic monotonic clock: only moves when the test advances it."""

    def __init__(self, start: float = 1_000.0) -> None:
        self.value = start

    def __call__(self) -> float:
        return self.value

    def advance(self, seconds: float) -> None:
        self.value += seconds


# --------------------------------------------------------------------------
# G1 — pure schema unit tests (single clock domain, exact arithmetic)
# --------------------------------------------------------------------------


def test_components_are_additive_in_one_clock_domain() -> None:
    clock = FakeClock(1000.0)
    timing = ModelPlaneTiming.start(clock)
    clock.advance(0.25)
    timing.note_dispatch()
    clock.advance(1.5)
    timing.note_first_frame()
    clock.advance(0.08)
    timing.note_first_visible()

    assert timing.local_pre_provider_seconds == pytest.approx(0.25, abs=1e-9)
    assert timing.provider_wait_seconds == pytest.approx(1.5, abs=1e-9)
    assert timing.local_projection_seconds == pytest.approx(0.08, abs=1e-9)
    assert timing.local_overhead_seconds == pytest.approx(0.33, abs=1e-9)
    assert timing.model_plane_ttft_seconds == pytest.approx(1.83, abs=1e-9)
    assert timing.identity_residual() is not None
    assert timing.identity_residual() <= 1e-9


@pytest.mark.asyncio
async def test_real_clock_identity_stays_within_rounding_tolerance() -> None:
    """G1 real-clock clause: the identity is arithmetic, not a sleep race."""
    timing = ModelPlaneTiming.start(time.perf_counter)
    await asyncio.sleep(0.005)
    timing.note_dispatch()
    await asyncio.sleep(0.005)
    timing.note_first_frame()
    timing.note_first_visible()
    residual = timing.identity_residual()
    assert residual is not None
    assert residual <= REAL_CLOCK_IDENTITY_TOLERANCE_SECONDS


def test_partial_records_report_missing_components_as_none_never_zero() -> None:
    clock = FakeClock(10.0)
    timing = ModelPlaneTiming.start(clock)
    clock.advance(0.5)
    timing.note_dispatch()

    assert timing.local_pre_provider_seconds == pytest.approx(0.5)
    assert timing.provider_wait_seconds is None
    assert timing.local_projection_seconds is None
    assert timing.local_overhead_seconds is None
    assert timing.model_plane_ttft_seconds is None
    assert timing.identity_residual() is None


def test_stamps_are_first_write_wins() -> None:
    clock = FakeClock(0.0)
    timing = ModelPlaneTiming.start(clock)
    clock.advance(1.0)
    timing.note_dispatch()
    clock.advance(5.0)
    timing.note_dispatch()  # a later provider frame must not move the boundary
    assert timing.local_pre_provider_seconds == pytest.approx(1.0)


def test_components_round_to_six_digits() -> None:
    clock = FakeClock(0.0)
    timing = ModelPlaneTiming.start(clock)
    clock.advance(0.123456789)
    timing.note_dispatch()
    assert timing.components()["local_pre_provider_seconds"] == 0.123457


# --------------------------------------------------------------------------
# G3 — client reconciliation tolerance matrix (pre-declared bounds)
# --------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("server_ttft", "client_ttft", "expected"),
    [
        (None, 3.0, False),
        (3.0, None, False),
        (3.0, 3.05, True),  # typical localhost residual
        (3.0, 3.19, True),  # inside the 0.200 s absolute budget
        (3.0, 3.30, False),  # residual beyond tolerance fails
        (3.0, 2.99, True),  # exactly at the 10 ms skew floor (inclusive)
        (3.0, 2.98, False),  # client more than 10 ms below server: impossible
        (3.0, 2.90, False),  # server window is a sub-interval of the client's
        (4.5, 4.70, True),  # 5 % of client (0.235) beats the 0.200 abs floor
        (4.5, 4.80, False),
    ],
)
def test_client_residual_tolerance_boundaries(
    server_ttft: float | None,
    client_ttft: float | None,
    expected: bool,
) -> None:
    assert client_residual_within_tolerance(server_ttft, client_ttft) is expected


# --------------------------------------------------------------------------
# Shared integration plumbing (mirrors test_model_plane.py fixture shapes)
# --------------------------------------------------------------------------


class _FakeDatabase:
    """Fake asyncpg surface; every execute advances the fake clock by latency."""

    def __init__(self, clock: FakeClock, *, latency: float = 0.04) -> None:
        self.clock = clock
        self.latency = latency
        self.operations: list[tuple[str, tuple[Any, ...]]] = []

    async def execute(self, query: str, *args: Any) -> str:
        self.clock.advance(self.latency)
        self.operations.append(("execute", (query, args)))
        return "UPDATE 1"

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any]:
        self.operations.append(("fetchrow", (query, args)))
        return {"ok": True}


class _FakeProviderService:
    async def get_runtime_provider_config(
        self,
        tenant_id: str,
        provider_id: str,
    ) -> dict[str, Any]:
        assert tenant_id == "tenant-a"
        assert provider_id == "dashscope"
        return {
            "updated_at": "provider-revision-1",
            "api_key": "provider-secret",
            "runtime_base_url": "https://dashscope.example/compatible-mode/v1",
        }


def _profile() -> dict[str, Any]:
    profile = get_builtin_model_capabilities("dashscope", "qwen3.7-plus")
    assert profile is not None
    return profile


def _make_call(wire_protocol: str) -> _AuthorizedCall:
    return _AuthorizedCall(
        call_id=uuid.uuid4(),
        lease_id=uuid.uuid4(),
        run_id=uuid.uuid4(),
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        model_id="qwen3.7-plus",
        provider_id="dashscope",
        provider_revision="provider-revision-1",
        snapshot={
            "model": {"wire_protocol": wire_protocol},
            "capabilities": _profile(),
            "reasoning": {"effective_option": "minimal"},
            "pricing": {"input_price_per_1k": 0.001, "output_price_per_1k": 0.002},
        },
        estimated_input_tokens=4,
        reserved_output_tokens=256,
    )


def _make_plane(clock: FakeClock, content: AsyncIterator[bytes]) -> AgentModelPlane:
    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            request=request,
            headers={"content-type": "text/event-stream"},
            content=content,
        )

    return AgentModelPlane(
        database=_FakeDatabase(clock),
        provider_service=_FakeProviderService(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        http_client=httpx.AsyncClient(transport=httpx.MockTransport(handler)),
        clock=clock,
    )


async def _drain(plane: AgentModelPlane, call: _AuthorizedCall, *, tools: list[dict] | None = None) -> list[bytes]:
    try:
        return [
            chunk
            async for chunk in plane.stream(
                body={"input": [{"role": "user", "content": "你好"}], "tools": tools or []},
                turn_metadata={},
                authorized_call=call,
            )
        ]
    finally:
        await plane.close()


_TIMING_PAIR_RE = re.compile(r"(\w+_seconds)=(\S+)")


def _parse_timing_line(message: str) -> dict[str, float | None]:
    parsed: dict[str, float | None] = {}
    for key, value in _TIMING_PAIR_RE.findall(message):
        parsed[key] = None if value == "None" else float(value)
    return parsed


def _logged_timings(caplog: pytest.LogCaptureFixture) -> list[dict[str, float | None]]:
    return [
        _parse_timing_line(record.getMessage())
        for record in caplog.records
        if "Agent model-plane timing schema=ppr-timing/v1" in record.getMessage()
    ]


def _only_logged_timing(caplog: pytest.LogCaptureFixture) -> dict[str, float | None]:
    logged = _logged_timings(caplog)
    assert len(logged) == 1, logged
    return logged[0]


def _chat_line(payload: dict[str, Any]) -> bytes:
    return f"data: {json.dumps(payload, separators=(',', ':'))}\n\n".encode()


# --------------------------------------------------------------------------
# G1/G2 — chat_completions path (DashScope default wire protocol)
# --------------------------------------------------------------------------


async def _chat_stream(
    clock: FakeClock,
    *,
    provider_stall: float,
    projection_stall: float,
) -> AsyncIterator[bytes]:
    # The advance before the first yield models the provider stalling on TTFB;
    # the one before the content frame models provider pacing *after* the first
    # upstream frame (attributed to local projection by the declared caveat).
    clock.advance(provider_stall)
    yield _chat_line({"choices": [{"delta": {"role": "assistant"}}]})
    clock.advance(projection_stall)
    yield _chat_line({"choices": [{"delta": {"content": "你"}}]})
    yield _chat_line(
        {
            "choices": [{"delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 2},
        }
    )
    yield b"data: [DONE]\n\n"


@pytest.mark.asyncio
async def test_chat_completions_delays_are_attributed_to_the_right_component(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """G2 with exact fake-clock arithmetic.

    The only pre-dispatch DB round trip is the dispatched UPDATE, so
    local_pre_provider_seconds is exactly the fake latency; provider_wait ends
    at the first parsed frame; projection is frame -> first visible chunk.
    """
    clock = FakeClock(1000.0)
    plane = _make_plane(clock, _chat_stream(clock, provider_stall=2.0, projection_stall=0.5))
    with caplog.at_level(logging.INFO, logger=logger_name):
        await _drain(plane, _make_call("chat_completions"))

    logged = _only_logged_timing(caplog)
    assert logged["local_pre_provider_seconds"] == pytest.approx(0.04, abs=1e-9)
    assert logged["provider_wait_seconds"] == pytest.approx(2.0, abs=1e-9)
    assert logged["local_projection_seconds"] == pytest.approx(0.5, abs=1e-9)
    assert logged["local_overhead_seconds"] == pytest.approx(0.54, abs=1e-9)
    assert logged["model_plane_ttft_seconds"] == pytest.approx(2.54, abs=1e-9)
    assert "wire=chat_completions" in caplog.text


@pytest.mark.asyncio
async def test_chat_completions_pre_dispatch_delay_never_leaks_into_provider_wait(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """G2 isolation: stretching local pre-dispatch work moves only local time.

    The slow-plane adds 0.9 s to the pre-dispatch DB round trip; provider frames
    are paced identically, so provider_wait and projection must not move at all
    while local_pre_provider and the end-to-end server TTFT move by exactly Δ.
    """
    delta = 0.9
    clock = FakeClock(1000.0)
    base_plane = _make_plane(clock, _chat_stream(clock, provider_stall=1.0, projection_stall=0.3))
    with caplog.at_level(logging.INFO, logger=logger_name):
        await _drain(base_plane, _make_call("chat_completions"))
    base = _only_logged_timing(caplog)

    slow_clock = FakeClock(1000.0)
    slow_plane = _make_plane(
        slow_clock, _chat_stream(slow_clock, provider_stall=1.0, projection_stall=0.3)
    )
    slow_plane.database = _FakeDatabase(slow_clock, latency=0.04 + delta)
    caplog.clear()
    with caplog.at_level(logging.INFO, logger=logger_name):
        await _drain(slow_plane, _make_call("chat_completions"))
    slow = _only_logged_timing(caplog)

    assert slow["local_pre_provider_seconds"] - base["local_pre_provider_seconds"] == pytest.approx(
        delta, abs=1e-9
    )
    assert slow["provider_wait_seconds"] == pytest.approx(base["provider_wait_seconds"], abs=1e-9)
    assert slow["local_projection_seconds"] == pytest.approx(
        base["local_projection_seconds"], abs=1e-9
    )
    assert slow["model_plane_ttft_seconds"] - base["model_plane_ttft_seconds"] == pytest.approx(
        delta, abs=1e-9
    )


@pytest.mark.asyncio
async def test_chat_completions_post_first_frame_delay_never_leaks_into_provider_wait(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """G2 second clause: pacing after the first frame moves projection only."""
    clock = FakeClock(1000.0)
    base_plane = _make_plane(clock, _chat_stream(clock, provider_stall=1.5, projection_stall=0.1))
    with caplog.at_level(logging.INFO, logger=logger_name):
        await _drain(base_plane, _make_call("chat_completions"))
    base = _only_logged_timing(caplog)

    slow_clock = FakeClock(1000.0)
    slow_plane = _make_plane(
        slow_clock, _chat_stream(slow_clock, provider_stall=1.5, projection_stall=1.0)
    )
    caplog.clear()
    with caplog.at_level(logging.INFO, logger=logger_name):
        await _drain(slow_plane, _make_call("chat_completions"))
    slow = _only_logged_timing(caplog)

    assert slow["provider_wait_seconds"] == pytest.approx(base["provider_wait_seconds"], abs=1e-9)
    assert slow["local_pre_provider_seconds"] == pytest.approx(
        base["local_pre_provider_seconds"], abs=1e-9
    )
    assert slow["local_projection_seconds"] - base["local_projection_seconds"] == pytest.approx(
        0.9, abs=1e-9
    )
    assert slow["local_overhead_seconds"] - base["local_overhead_seconds"] == pytest.approx(
        0.9, abs=1e-9
    )


@pytest.mark.asyncio
async def test_tool_only_stream_keeps_projection_missing(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """A call with no text/reasoning output has no TTFT — reported as None."""
    clock = FakeClock(1000.0)

    async def stream() -> AsyncIterator[bytes]:
        yield _chat_line(
            {
                "choices": [
                    {
                        "delta": {
                            "tool_calls": [
                                {
                                    "index": 0,
                                    "id": "call-1",
                                    "type": "function",
                                    "function": {"name": "lookup", "arguments": "{}"},
                                }
                            ]
                        }
                    }
                ]
            }
        )
        yield _chat_line(
            {
                "choices": [{"delta": {}, "finish_reason": "tool_calls"}],
                "usage": {"prompt_tokens": 7, "completion_tokens": 2},
            }
        )
        yield b"data: [DONE]\n\n"

    plane = _make_plane(clock, stream())
    with caplog.at_level(logging.INFO, logger=logger_name):
        await _drain(plane, _make_call("chat_completions"), tools=[{
            "type": "function", "name": "lookup", "description": "Lookup.", "parameters": {"type": "object"},
        }])

    logged = _only_logged_timing(caplog)
    assert logged["provider_wait_seconds"] == pytest.approx(0.0, abs=1e-9)
    assert logged["local_projection_seconds"] is None
    assert logged["local_overhead_seconds"] is None
    assert logged["model_plane_ttft_seconds"] is None


# --------------------------------------------------------------------------
# G1/G2 — responses_v1 path
# --------------------------------------------------------------------------


def _native_event(sequence: int, event_type: str, **payload: Any) -> bytes:
    value = {"type": event_type, "sequence_number": sequence, **payload}
    return f"data: {json.dumps(value, separators=(',', ':'))}\n\n".encode()


async def _native_stream(
    clock: FakeClock,
    *,
    provider_stall: float,
    projection_stall: float,
) -> AsyncIterator[bytes]:
    message = {
        "id": "msg_1",
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{"type": "output_text", "text": "你好", "annotations": []}],
    }
    clock.advance(provider_stall)
    yield _native_event(
        0, "response.created", response={"id": "resp_1", "status": "queued", "output": []}
    )
    yield _native_event(
        1,
        "response.in_progress",
        response={"id": "resp_1", "status": "in_progress", "output": []},
    )
    yield _native_event(
        2,
        "response.output_item.added",
        output_index=0,
        item={"id": "msg_1", "type": "message", "status": "in_progress"},
    )
    clock.advance(projection_stall)
    yield _native_event(
        3,
        "response.output_text.delta",
        item_id="msg_1",
        output_index=0,
        content_index=0,
        delta="你好",
    )
    yield _native_event(4, "response.output_item.done", output_index=0, item=message)
    yield _native_event(
        5,
        "response.completed",
        response={
            "id": "resp_1",
            "status": "completed",
            "output": [message],
            "usage": {
                "input_tokens": 11,
                "output_tokens": 3,
                "total_tokens": 14,
                "input_tokens_details": {"cached_tokens": 7},
                "output_tokens_details": {"reasoning_tokens": 1},
            },
        },
    )


@pytest.mark.asyncio
async def test_native_responses_timing_identity_and_attribution(
    caplog: pytest.LogCaptureFixture,
) -> None:
    clock = FakeClock(1000.0)
    plane = _make_plane(clock, _native_stream(clock, provider_stall=1.8, projection_stall=0.2))
    with caplog.at_level(logging.INFO, logger=logger_name):
        await _drain(plane, _make_call("responses_v1"))

    logged = _only_logged_timing(caplog)
    assert logged["local_pre_provider_seconds"] == pytest.approx(0.04, abs=1e-9)
    # provider_wait ends at the first parsed non-terminal frame: response.created.
    assert logged["provider_wait_seconds"] == pytest.approx(1.8, abs=1e-9)
    # created/in_progress/item.added carry no delta payload; the first
    # output_text.delta is the first visible chunk.
    assert logged["local_projection_seconds"] == pytest.approx(0.2, abs=1e-9)
    assert logged["local_overhead_seconds"] == pytest.approx(0.24, abs=1e-9)
    assert logged["model_plane_ttft_seconds"] == pytest.approx(2.04, abs=1e-9)
    assert "wire=responses_v1" in caplog.text


# --------------------------------------------------------------------------
# Wall-clock attribution (methodology review F7): real asyncio.sleep delays,
# real time.perf_counter, real httpx stream parsing — no fake clock.
# --------------------------------------------------------------------------


class _SleepingDatabase(_FakeDatabase):
    """Real-time pre-dispatch latency: execute actually awaits."""

    def __init__(self, *, latency: float) -> None:
        super().__init__(FakeClock(), latency=0.0)
        self.real_latency = latency

    async def execute(self, query: str, *args: Any) -> str:
        await asyncio.sleep(self.real_latency)
        return await super().execute(query, *args)


async def _chat_stream_wallclock(
    *,
    provider_stall: float,
    projection_stall: float,
) -> AsyncIterator[bytes]:
    await asyncio.sleep(provider_stall)
    yield _chat_line({"choices": [{"delta": {"role": "assistant"}}]})
    await asyncio.sleep(projection_stall)
    yield _chat_line({"choices": [{"delta": {"content": "你"}}]})
    yield _chat_line(
        {
            "choices": [{"delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 2},
        }
    )
    yield b"data: [DONE]\n\n"


@pytest.mark.asyncio
async def test_wall_clock_delays_land_in_the_right_components(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """G2 under real wall-clock delays through the real transport.

    The fake-clock tests prove stamp placement exactly; this proves the same
    attribution survives real asyncio scheduling jitter: a 0.3 s pre-dispatch
    DB await, a 0.4 s provider TTFB stall and 0.2 s of post-first-frame
    pacing must stay in their own components within 50 ms of the scheduled
    wall-clock delays, with the identity holding to the real-clock tolerance.
    """
    pre, wait, proj = 0.3, 0.4, 0.2

    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            request=request,
            headers={"content-type": "text/event-stream"},
            content=_chat_stream_wallclock(provider_stall=wait, projection_stall=proj),
        )

    plane = AgentModelPlane(
        database=_SleepingDatabase(latency=pre),
        provider_service=_FakeProviderService(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        http_client=httpx.AsyncClient(transport=httpx.MockTransport(handler)),
        clock=time.perf_counter,
    )
    with caplog.at_level(logging.INFO, logger=logger_name):
        await _drain(plane, _make_call("chat_completions"))

    logged = _only_logged_timing(caplog)
    assert logged["local_pre_provider_seconds"] >= pre
    assert logged["local_pre_provider_seconds"] < pre + 0.05
    assert logged["provider_wait_seconds"] >= wait
    assert logged["provider_wait_seconds"] < wait + 0.05
    assert logged["local_projection_seconds"] >= proj
    assert logged["local_projection_seconds"] < proj + 0.05
    assert logged["model_plane_ttft_seconds"] == pytest.approx(
        logged["local_pre_provider_seconds"]
        + logged["provider_wait_seconds"]
        + logged["local_projection_seconds"],
        abs=REAL_CLOCK_IDENTITY_TOLERANCE_SECONDS,
    )


@pytest.mark.asyncio
async def test_native_function_call_arguments_never_stamp_first_visible(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """Reviewer regression #6: the arguments of a model-generated tool call
    are not visible content, even when they literally contain "reasoning" or
    "output_text". Only the re-encoded event type may stamp t_first_visible."""
    clock = FakeClock(1000.0)

    async def stream() -> AsyncIterator[bytes]:
        yield _native_event(
            0, "response.created", response={"id": "resp_1", "status": "queued", "output": []}
        )
        yield _native_event(
            1,
            "response.output_item.added",
            output_index=0,
            item={
                "id": "call_1",
                "type": "function_call",
                "status": "in_progress",
                "call_id": "call_1",
                "name": "lookup",
                "arguments": "",
            },
        )
        # Adversarial arguments: contain both substrings the old byte
        # heuristic matched on.
        yield _native_event(
            2,
            "response.function_call_arguments.delta",
            item_id="call_1",
            output_index=0,
            delta='{"query": "reasoning about output_text .delta"}',
        )
        yield _native_event(
            3,
            "response.function_call_arguments.done",
            item_id="call_1",
            output_index=0,
            name="lookup",
            arguments='{"query": "reasoning about output_text .delta"}',
        )
        yield _native_event(
            4,
            "response.output_item.done",
            output_index=0,
            item={
                "id": "call_1",
                "type": "function_call",
                "status": "completed",
                "call_id": "call_1",
                "name": "lookup",
                "arguments": '{"query": "reasoning"}',
            },
        )
        clock.advance(0.2)
        yield _native_event(
            5,
            "response.output_text.delta",
            item_id="msg_1",
            output_index=1,
            content_index=0,
            delta="你好",
        )
        yield _native_event(
            6,
            "response.completed",
            response={
                "id": "resp_1",
                "status": "completed",
                "output": [
                    {
                        "id": "call_1",
                        "type": "function_call",
                        "status": "completed",
                        "call_id": "call_1",
                        "name": "lookup",
                        "arguments": '{"query": "reasoning"}',
                    },
                    {
                        "id": "msg_1",
                        "type": "message",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "你好", "annotations": []}],
                    },
                ],
                "usage": {
                    "input_tokens": 11,
                    "output_tokens": 3,
                    "total_tokens": 14,
                    "input_tokens_details": {"cached_tokens": 0},
                    "output_tokens_details": {"reasoning_tokens": 0},
                },
            },
        )

    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            request=request,
            headers={"content-type": "text/event-stream"},
            content=stream(),
        )

    plane = AgentModelPlane(
        database=_FakeDatabase(clock),
        provider_service=_FakeProviderService(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        http_client=httpx.AsyncClient(transport=httpx.MockTransport(handler)),
        clock=clock,
    )
    call = _make_call("responses_v1")
    with caplog.at_level(logging.INFO, logger=logger_name):
        async for _ in plane.stream(
            body={
                "input": [{"role": "user", "content": "你好"}],
                "tools": [
                    {
                        "type": "function",
                        "name": "lookup",
                        "description": "Look something up.",
                        "parameters": {"type": "object", "properties": {}},
                    }
                ],
            },
            turn_metadata={},
            authorized_call=call,
        ):
            pass
        await plane.close()

    logged = _only_logged_timing(caplog)
    # If the old substring heuristic were still in place, projection would be
    # 0.0 (the arguments delta pre-fires at seq 2). It must instead measure
    # the single 0.2 advance between the tool events and the text delta.
    assert logged["local_projection_seconds"] == pytest.approx(0.2, abs=1e-9)
    assert logged["model_plane_ttft_seconds"] == pytest.approx(0.24, abs=1e-9)


@pytest.mark.asyncio
async def test_cancelled_stream_emits_zero_timing_lines(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """Server review F2 (survivorship, methodology note): timing lines exist
    only for *completed* calls. A consumer that disconnects mid-stream
    (GeneratorExit at a yield) never reaches `_log_model_plane_timing`, so a
    cancelled call can never enter the component distributions — not even as
    a partial record. Every other test here asserts exactly-one; this asserts
    zero."""
    clock = FakeClock(1000.0)

    async def endless() -> AsyncIterator[bytes]:
        while True:
            yield _chat_line({"choices": [{"delta": {"content": "你"}}]})

    plane = _make_plane(clock, endless())
    with caplog.at_level(logging.INFO, logger=logger_name):
        stream = plane.stream(
            body={"input": [{"role": "user", "content": "你好"}]},
            turn_metadata={},
            authorized_call=_make_call("chat_completions"),
        )
        # Advance past the synthesized created() frame and two content-delta
        # frames so t_first_frame/t_first_visible ARE stamped — the strongest
        # cut point: even a fully-measured call must emit nothing until
        # _complete_call, and a cancelled one never gets there.
        for _ in range(3):
            assert await stream.__anext__()
        await stream.aclose()  # client disconnect mid-call
        await plane.close()

    assert _logged_timings(caplog) == []
