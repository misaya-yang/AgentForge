import asyncio
import json
import uuid
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import httpx
import pytest

from src.core.gateway.admission import CapacityRejected
from src.services.agent_runtime.control import capacity, turn_start
from src.services.agent_runtime.control_plane import AgentRuntimeControlPlane
from src.services.agent_runtime.model import chat_completions
from src.services.agent_runtime.model_plane import AgentModelPlane


TERMINAL_CASES = json.loads(
    (Path(__file__).resolve().parents[2] / "fixtures/agent-runtime/terminal-status-v1.json").read_text()
)["statuses"]


def state():
    turn = SimpleNamespace(run_id=str(uuid.uuid4()), runtime_thread_id=str(uuid.uuid4()), after_sequence=1200)
    lease = SimpleNamespace(bind_owner=MagicMock(), release=AsyncMock())
    scope = {"tenant_id": "t", "user_id": "u", "session_id": "s"}
    plane = SimpleNamespace(
        database=SimpleNamespace(fetchrow=AsyncMock(return_value={"status": "running"}), execute=AsyncMock()),
        interrupt_turn=AsyncMock(), lease_ttl_seconds=30,
        _run_capacity_tasks={}, _run_capacity_leases={turn.run_id: lease},
        _run_capacity_contexts={turn.run_id: (turn, scope)},
    )
    return plane, turn, lease, scope


@pytest.mark.asyncio
@pytest.mark.parametrize("status", [case["input"] for case in TERMINAL_CASES if case["terminal_in_ledger"]])
async def test_watcher_already_terminal_releases_without_stream_or_interrupt(monkeypatch, status):
    plane, turn, lease, scope = state()
    plane.database.fetchrow.return_value = {"status": status}
    stream = MagicMock(side_effect=AssertionError("terminal run must not reopen SSE"))
    monkeypatch.setattr(capacity.event_stream, "stream_thread_events", stream)
    await capacity.watch_run(plane, turn, lease, **scope)
    lease.release.assert_awaited_once()
    plane.interrupt_turn.assert_not_awaited()
    plane.database.execute.assert_not_awaited()
    stream.assert_not_called()


@pytest.mark.asyncio
async def test_eof_retries_until_original_ttl_then_quarantines_until_real_terminal(monkeypatch):
    plane, turn, lease, scope = state()
    plane.lease_ttl_seconds = 0.03
    plane.interrupt_turn.side_effect = httpx.ReadTimeout("unconfirmed interrupt")

    async def empty(*_args, **kwargs):
        assert kwargs["after_sequence"] == 1200
        if False:
            yield

    monkeypatch.setattr(capacity.event_stream, "stream_thread_events", empty)
    await capacity.watch_run(plane, turn, lease, **scope)
    lease.release.assert_not_awaited()
    assert turn.run_id in plane._run_capacity_leases
    plane.database.execute.assert_awaited_once()  # model authorization revoked
    plane.database.fetchrow.return_value = {"status": "cancelled"}
    await capacity.watch_run(plane, turn, lease, **scope)
    lease.release.assert_awaited_once()


@pytest.mark.asyncio
@pytest.mark.parametrize("transient", [httpx.ReadError("controlled transport loss"), capacity.AgentRuntimeControlError("fixture_unavailable", status_code=503)])
async def test_runtime_restart_reconnects_original_cursor_without_revoking_capacity(monkeypatch, transient):
    plane, turn, lease, scope = state()
    plane.lease_ttl_seconds = 3
    cursors = []

    async def events(*_args, **kwargs):
        assert {key: kwargs[key] for key in scope} == scope
        assert kwargs["runtime_thread_id"] == turn.runtime_thread_id
        assert kwargs["turn_id"] == turn.run_id
        cursors.append(kwargs["after_sequence"])
        lease.release.assert_not_awaited()
        plane.database.execute.assert_not_awaited()
        if len(cursors) == 1:
            yield {"sequence": 1201, "event_type": "text_delta", "data": {"run_id": turn.run_id}}
        elif len(cursors) == 2:
            raise transient
        else:
            yield {"sequence": 1202, "event_type": "run_finished", "data": {"run_id": turn.run_id, "status": "succeeded"}}

    monkeypatch.setattr(capacity.event_stream, "stream_thread_events", events)
    await capacity.watch_run(plane, turn, lease, **scope)
    assert cursors == [1200, 1201, 1201]
    plane.interrupt_turn.assert_not_awaited()
    plane.database.execute.assert_not_awaited()
    lease.release.assert_awaited_once()


@pytest.mark.asyncio
async def test_permission_denied_does_not_retry_runtime_subscription(monkeypatch):
    plane, turn, lease, scope = state()
    subscriptions = []

    async def events(*_args, **_kwargs):
        subscriptions.append(True)
        raise capacity.AgentRuntimeControlError("fixture_forbidden", status_code=403)
        yield

    monkeypatch.setattr(capacity.event_stream, "stream_thread_events", events)
    await capacity.watch_run(plane, turn, lease, **scope)
    assert len(subscriptions) == 1
    plane.interrupt_turn.assert_awaited_once()


@pytest.mark.asyncio
async def test_cancelled_watcher_waits_for_interrupt_ack_before_release(monkeypatch):
    plane, turn, lease, scope = state()
    subscribed, interrupt_started, interrupted = asyncio.Event(), asyncio.Event(), asyncio.Event()

    async def events(*_args, **_kwargs):
        subscribed.set()
        await asyncio.Event().wait()
        yield {}

    async def stop(**_kwargs):
        interrupt_started.set()
        await interrupted.wait()

    plane.interrupt_turn = stop
    monkeypatch.setattr(capacity.event_stream, "stream_thread_events", events)
    task = asyncio.create_task(capacity.watch_run(plane, turn, lease, **scope))
    await subscribed.wait()
    task.cancel()
    await interrupt_started.wait()
    lease.release.assert_not_awaited()
    interrupted.set()
    await task
    lease.release.assert_awaited_once()


@pytest.mark.asyncio
async def test_close_handles_watcher_cancelled_before_first_instruction():
    plane, turn, lease, _scope = state()

    async def unstarted():
        raise AssertionError("task was cancelled before it started")

    plane._run_capacity_tasks[turn.run_id] = asyncio.create_task(unstarted())
    await capacity.close(plane)
    plane.interrupt_turn.assert_awaited_once()
    lease.release.assert_awaited_once()


@pytest.mark.asyncio
async def test_provider_admission_rejection_terminalizes_reserved_without_dispatch(monkeypatch):
    call = SimpleNamespace(call_id=uuid.uuid4(), tenant_id="t", user_id="u", provider_id="p")
    plane = object.__new__(AgentModelPlane)
    plane._fail_call = AsyncMock()
    monkeypatch.setattr(capacity, "acquire", AsyncMock(side_effect=CapacityRejected(budget_key="provider")))
    dispatch = MagicMock(side_effect=AssertionError("must not dispatch"))
    monkeypatch.setattr(chat_completions, "stream", dispatch)
    with pytest.raises(CapacityRejected):
        await anext(plane.stream(body={}, turn_metadata={}, authorized_call=call))
    plane._fail_call.assert_awaited_once_with(call.call_id, "capacity_rejected", dispatched=False)
    dispatch.assert_not_called()


@pytest.mark.asyncio
async def test_provider_pre_dispatch_exception_cleans_reservation_before_capacity(monkeypatch):
    call = SimpleNamespace(call_id=uuid.uuid4(), tenant_id="t", user_id="u", provider_id="p")
    plane = object.__new__(AgentModelPlane)
    order = []

    async def settle(query, *_args):
        assert "CASE WHEN status='reserved' THEN 'failed' ELSE 'unknown' END" in query
        order.append("settled")

    async def release():
        assert order == ["producer_closed", "settled"]
        order.append("released")

    async def fail(*_args, **_kwargs):
        try:
            raise RuntimeError("provider lookup failed")
            yield b""
        finally:
            order.append("producer_closed")

    plane.database = SimpleNamespace(execute=settle)
    monkeypatch.setattr(capacity, "acquire", AsyncMock(return_value=SimpleNamespace(release=release)))
    monkeypatch.setattr(chat_completions, "stream", fail)
    with pytest.raises(RuntimeError, match="provider lookup failed"):
        await anext(plane.stream(body={}, turn_metadata={}, authorized_call=call))
    assert order == ["producer_closed", "settled", "released"]


@pytest.mark.asyncio
async def test_start_response_lost_holds_capacity_until_interrupt_confirmed(monkeypatch):
    fake, turn, lease, scope = state()
    plane = object.__new__(AgentRuntimeControlPlane)
    plane.__dict__.update(fake.__dict__)
    plane.interrupt_turn.side_effect = httpx.ReadTimeout("interrupt unconfirmed")

    async def start(*_args, **_kwargs):
        capacity.note_dispatch(runtime_thread_id=turn.runtime_thread_id, run_id=turn.run_id, after_sequence=0)
        raise httpx.ReadTimeout("turn accepted but response lost")

    monkeypatch.setattr(capacity, "acquire", AsyncMock(return_value=lease))
    monkeypatch.setattr(turn_start, "start_turn", start)
    with pytest.raises(httpx.ReadTimeout):
        await plane.start_turn(**scope, message="x", model_id="m", reasoning_option=None,
                               legacy_thinking_level=None, max_tokens=None)
    lease.release.assert_not_awaited()
    assert turn.run_id in plane._run_capacity_leases
    assert capacity.PENDING_START.get() is None


@pytest.mark.asyncio
@pytest.mark.parametrize("case", TERMINAL_CASES, ids=lambda case: case["input"] or "empty")
async def test_terminal_ledger_vocabulary_retains_original_scope(case):
    plane, turn, _lease, scope = state()
    plane.database.fetchrow.return_value = {"status": case["input"]}
    assert await capacity._terminal_in_ledger(plane, turn, scope) is case["terminal_in_ledger"]
    call = plane.database.fetchrow.await_args
    assert call.args[0] == (
        "SELECT status FROM assistant_runs WHERE run_id=$1 AND tenant_id=$2 "
        "AND user_id=$3 AND session_id=$4 AND engine='agent_runtime'"
    )
    assert call.args[1:] == (uuid.UUID(turn.run_id), scope["tenant_id"], scope["user_id"], scope["session_id"])


@pytest.mark.asyncio
async def test_legacy_terminal_does_not_confirm_unacknowledged_dispatch():
    plane, turn, _lease, scope = state()
    turn._capacity_unconfirmed_dispatch = True
    plane.database.fetchrow.return_value = {"status": "completed"}
    assert not await capacity._terminal_in_ledger(plane, turn, scope)
    plane.database.fetchrow.assert_not_awaited()
