"""Independent run/provider/SSE admission using the Gateway's shared controller."""
from __future__ import annotations

import asyncio
import contextlib
import logging
import uuid
from contextvars import ContextVar
from types import SimpleNamespace
from typing import Any

import httpx

from ....core.gateway.admission import _finish_cleanup
from . import event_stream
from .types import AgentRuntimeControlError

logger = logging.getLogger(__name__)
# Historical recovery completion is terminal, but is not a proven success.
_TERMINAL = {"succeeded", "failed", "cancelled", "completed"}
PENDING_START: ContextVar[dict[str, Any] | None] = ContextVar("runtime_capacity_pending_start", default=None)


def note_dispatch(*, runtime_thread_id: str, run_id: str, after_sequence: int) -> None:
    pending = PENDING_START.get()
    if pending is not None:
        pending["turn"] = SimpleNamespace(
            runtime_thread_id=runtime_thread_id, run_id=run_id, after_sequence=after_sequence,
            _capacity_unconfirmed_dispatch=True,
        )



async def acquire(plane: Any, *, tenant_id: str, user_id: str, kind: str, request_id: str, provider_id: str | None = None):
    if getattr(plane, "admission_controller", None) is None or getattr(plane, "capacity_resolver", None) is None:
        return None
    service_id = f"agent-runtime-{kind}"
    request_class = "stream" if kind in {"provider", "sse"} else "sync"
    budgets = await plane.capacity_resolver.resolve(
        tenant_id=tenant_id, service_id=service_id, request_class=request_class,
        upstream_group=None, provider_id=provider_id, resource_kind=kind,
    )
    return await plane.admission_controller.acquire(
        budgets=budgets, tenant_id=tenant_id, user_id=user_id, service_id=service_id,
        request_class=request_class, request_id=request_id,
    )


async def _terminal_in_ledger(plane: Any, turn: Any, scope: dict[str, str]) -> bool:
    if getattr(turn, "_capacity_unconfirmed_dispatch", False):
        return False
    row = await plane.database.fetchrow(
        "SELECT status FROM assistant_runs WHERE run_id=$1 AND tenant_id=$2 "
        "AND user_id=$3 AND session_id=$4 AND engine='agent_runtime'",
        uuid.UUID(turn.run_id), scope["tenant_id"], scope["user_id"], scope["session_id"],
    )
    return bool(row and row.get("status") in _TERMINAL)


async def stop_run(plane: Any, turn: Any, scope: dict[str, str]) -> bool:
    """Fence future model work; release only after an authoritative terminal/ack."""
    try:
        if await _terminal_in_ledger(plane, turn, scope):
            return True
    except Exception:
        pass
    try:
        await plane.database.execute(
            "UPDATE assistant_runtime_model_leases SET status='revoked', revoked_at=NOW(), "
            "revoked_reason='run_capacity_interrupted', updated_at=NOW() "
            "WHERE run_id=$1 AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND status='active'",
            uuid.UUID(turn.run_id), scope["tenant_id"], scope["user_id"], scope["session_id"],
        )
    except Exception:
        logger.error("run_capacity_lease_revoke_unconfirmed run_id=%s", turn.run_id)
    try:
        # This existing method checks HTTP status and acknowledges only after
        # Runtime TurnAborted; failed dispatch never creates a fake terminal.
        await asyncio.wait_for(plane.interrupt_turn(
            runtime_thread_id=turn.runtime_thread_id, turn_id=turn.run_id,
            reason="run_capacity_interrupted", **scope,
        ), timeout=5.0)
        return True
    except Exception:
        logger.error("run_capacity_interrupt_unconfirmed run_id=%s", turn.run_id)
        return False


async def release_run(plane: Any, run_id: str) -> None:
    lease = getattr(plane, "_run_capacity_leases", {}).get(run_id)
    if lease is not None:
        await lease.release()
        plane._run_capacity_leases.pop(run_id, None)
        getattr(plane, "_run_capacity_contexts", {}).pop(run_id, None)


async def watch_run(plane: Any, turn: Any, lease: Any, *, tenant_id: str, user_id: str, session_id: str) -> None:
    scope = {"tenant_id": tenant_id, "user_id": user_id, "session_id": session_id}
    confirmed = False
    try:
        lease.bind_owner()
        if await _terminal_in_ledger(plane, turn, scope):
            confirmed = True
            return

        async def consume() -> bool:
            cursor = getattr(turn, "after_sequence", 0)
            while True:
                if await _terminal_in_ledger(plane, turn, scope):
                    return True
                events = event_stream.stream_thread_events(
                    plane, runtime_thread_id=turn.runtime_thread_id, **scope,
                    turn_id=turn.run_id, after_sequence=cursor,
                )
                try:
                    async with contextlib.aclosing(events):
                        async for envelope in events:
                            cursor = max(cursor, int(envelope.get("sequence") or cursor))
                            data = envelope.get("data") or {}
                            if (envelope.get("event_type") in {"run_finished", "run_error", "cancelled"}
                                    and str(data.get("run_id") or "") == str(turn.run_id)):
                                return True
                except AgentRuntimeControlError as exc:
                    if exc.status_code != 503:
                        raise
                except httpx.TransportError:
                    pass
                # EOF/transport loss reconnects only the original cursor under
                # the original watchdog TTL. Capacity loss, service shutdown
                # and TTL still cancel this task through the existing stop path.
                await asyncio.sleep(0.5)

        confirmed = await asyncio.wait_for(consume(), timeout=plane.lease_ttl_seconds)
        if not confirmed:
            confirmed = await _terminal_in_ledger(plane, turn, scope)
    except BaseException:
        # Cancellation is drained below before release, including disconnect,
        # lease loss, TTL and service shutdown.
        pass
    finally:
        if not confirmed:
            confirmed = await _finish_cleanup(asyncio.create_task(stop_run(plane, turn, scope)))
        if confirmed:
            await release_run(plane, turn.run_id)
        else:
            # Keep the local slot/quarantine lease until a subsequent real
            # terminal closes it. Never turn an unavailable interrupt into success.
            logger.error("run_capacity_quarantined run_id=%s", turn.run_id)
        plane._run_capacity_tasks.pop(turn.run_id, None)


async def close(plane: Any) -> None:
    tasks = list(getattr(plane, "_run_capacity_tasks", {}).values())
    for task in tasks:
        task.cancel()
    if tasks:
        await asyncio.gather(*tasks, return_exceptions=True)
    getattr(plane, "_run_capacity_tasks", {}).clear()
    # A task cancelled before its first instruction never ran its finally.
    for run_id, (turn, scope) in list(getattr(plane, "_run_capacity_contexts", {}).items()):
        if await stop_run(plane, turn, scope):
            await release_run(plane, run_id)
