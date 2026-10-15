"""Recover missing Eval traces from durable, terminal Agent Runtime turns.

This reader never starts, resumes, or replays a model/tool turn. The Runtime
run ledger and its original-turn V1 event log are the only source of facts.
"""

from __future__ import annotations

import asyncio
import json
import logging
from datetime import datetime, timedelta, timezone
from typing import Any

from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository

from .assistant_trace_capture import _terminal_status, build_assistant_runtime_trace

logger = logging.getLogger(__name__)

_TERMINAL_RUN_STATUS = {
    "completed": "succeeded",
    "succeeded": "succeeded",
    "failed": "failed",
    "cancelled": "cancelled",
}


def _json_object(value: Any) -> dict[str, Any]:
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except json.JSONDecodeError:
            return {}
    return value if isinstance(value, dict) else {}


def _scoped_terminal_status(run: dict[str, Any], terminal: dict[str, Any]) -> str | None:
    event_type = str(terminal.get("event_type") or "").removeprefix("compat/v1/")
    data = _json_object(_json_object(terminal.get("payload")).get("data"))
    expected = _TERMINAL_RUN_STATUS.get(str(run.get("status") or ""))
    if (
        str(data.get("run_id") or "") != str(run["run_id"])
        or str(data.get("session_id") or "") != str(run["session_id"])
        or (expected == "succeeded" and event_type != "run_finished")
        or (expected == "failed" and event_type != "run_error")
        or (expected == "cancelled" and event_type not in {"cancelled", "run_error"})
    ):
        return None
    observed = _terminal_status(event_type, data)
    return observed if observed == expected else None


async def _read_output_preview(
    repository: AgentTraceRepository, *, scope: dict[str, str],
) -> str:
    # SQL excludes empty chunks; at most 2,000 one-character rows are read.
    parts: list[str] = []
    size = 0
    sequence = 0
    while size < 2_000:
        page = await repository.read_assistant_runtime_trace_text_page(
            **scope, after_sequence=sequence,
        )
        if not page:
            break
        for item in page:
            sequence = int(item["sequence"])
            content = str(item.get("content") or "")
            part = content[: 2_000 - size]
            parts.append(part)
            size += len(part)
            if size >= 2_000:
                break
        if len(page) < 250:
            break
    return "".join(parts)


async def reconcile_assistant_runtime_trace_page(
    database: Any,
    *,
    retention_days: int = 90,
    limit: int = 100,
    after_finished_at: datetime | None = None,
    after_run_id: str | None = None,
) -> tuple[int, tuple[datetime, str] | None]:
    """Scan one bounded page; return inserted count and the next-page cursor."""
    if not getattr(database, "enabled", False):
        return 0, None
    repository = AgentTraceRepository(database)
    cutoff = datetime.now(timezone.utc) - timedelta(days=max(1, retention_days))
    rows = await repository.list_assistant_runtime_trace_reconciliation_candidates(
        since=cutoff, limit=limit,
        after_finished_at=after_finished_at, after_run_id=after_run_id,
    )
    inserted = 0
    for run in rows:
        try:
            scope = {
                "tenant_id": str(run["tenant_id"]),
                "user_id": str(run["user_id"]),
                "session_id": str(run["session_id"]),
                "runtime_thread_id": str(run["runtime_thread_id"]),
                "run_id": str(run["run_id"]),
            }
            terminal = await repository.get_assistant_runtime_trace_terminal(**scope)
            if terminal is None:
                continue
            status = _scoped_terminal_status(run, terminal)
            if status is None:
                continue
            started_at = run["started_at"]
            ended_at = run["ended_at"]
            if not isinstance(started_at, datetime) or not isinstance(ended_at, datetime):
                continue
            event_counts = await repository.count_assistant_runtime_trace_events(**scope)
            output = await _read_output_preview(repository, scope=scope)
            receipt = await repository.get_assistant_runtime_trace_model_usage(**scope)
            dispatched_calls = int(receipt.get("dispatched_calls") or 0)
            measured_calls = int(receipt.get("measured_calls") or 0)
            usage = _json_object(run.get("usage"))
            if dispatched_calls > 0 and measured_calls == dispatched_calls:
                input_tokens = int(receipt["input_tokens"])
                output_tokens = int(receipt["output_tokens"])
                usage = {
                    "input_tokens": input_tokens, "output_tokens": output_tokens,
                    "total_tokens": input_tokens + output_tokens,
                }
            snapshot = _json_object(run.get("snapshot"))
            model = _json_object(snapshot.get("model"))
            snapshot = {
                **snapshot,
                "model": {**model, "provider": model.get("provider") or model.get("provider_id")},
            }
            trace = build_assistant_runtime_trace(
                run_id=scope["run_id"], request_id=scope["run_id"],
                tenant_id=scope["tenant_id"], user_id=scope["user_id"],
                session_id=scope["session_id"],
                message=str(run.get("request_preview") or ""), snapshot=snapshot,
                status=status, started_at=started_at.timestamp(),
                ended_at=ended_at.timestamp(), first_token_latency_ms=0,
                output=output, event_counts=event_counts,
                usage=usage, error_type=None,
            )
            trace["metadata"]["runtime_model_usage"] = {
                "dispatched_calls": dispatched_calls,
                "measured_calls": measured_calls,
                "tokens_complete": dispatched_calls > 0 and measured_calls == dispatched_calls,
            }
            if (
                dispatched_calls > 0
                and int(receipt.get("cost_measured_calls") or 0) == dispatched_calls
            ):
                trace["metadata"]["runtime_model_usage"]["cost_microusd"] = int(
                    receipt["cost_microusd"]
                )
            trace["events"][0]["payload"]["runtime_sequence"] = int(terminal["sequence"])
            trace["retention_expires_at"] = (
                ended_at + timedelta(days=max(1, retention_days))
            ).isoformat()
            if await repository.insert_reconciled_assistant_runtime_trace(
                run=run, terminal=terminal, trace=trace,
            ):
                inserted += 1
        except Exception:
            logger.exception("Assistant trace reconciliation failed for run_id=%s", run.get("run_id"))
    next_cursor = (
        (rows[-1]["ended_at"], str(rows[-1]["run_id"]))
        if len(rows) == max(1, min(limit, 500)) else None
    )
    return inserted, next_cursor


async def run_assistant_trace_reconciler(
    database: Any, *, retention_days: int = 90,
    page_limit: int = 100, interval_seconds: float = 30,
) -> None:
    """Page the retention window on startup and revisit it for late terminals."""
    cursor: tuple[datetime, str] | None = None
    while True:
        try:
            inserted, cursor = await reconcile_assistant_runtime_trace_page(
                database, retention_days=retention_days, limit=page_limit,
                after_finished_at=cursor[0] if cursor else None,
                after_run_id=cursor[1] if cursor else None,
            )
            if inserted:
                logger.info("Reconciled %s durable assistant Runtime traces", inserted)
        except asyncio.CancelledError:
            raise
        except Exception:
            logger.exception("Assistant trace reconciliation scan failed")
            cursor = None
        await asyncio.sleep(interval_seconds)


__all__ = ["reconcile_assistant_runtime_trace_page", "run_assistant_trace_reconciler"]
