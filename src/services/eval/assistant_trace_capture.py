"""Non-blocking Eval trace capture for Agent Runtime SSE turns."""

from __future__ import annotations

import asyncio
import contextlib
import json
import time
import uuid
from collections.abc import AsyncIterator, Callable
from datetime import datetime, timezone
from typing import Any

from .trace_capture import (
    redact_preview,
    schedule_gateway_trace_ingest,
    span_id_for,
)

# A browser may close its SSE observer while the Runtime turn continues. Keep
# the terminal replay task alive until the durable Runtime cursor reaches a
# terminal event; the task never starts a second turn.
_terminal_replay_tasks: set[asyncio.Task[None]] = set()


async def enrich_runtime_trace_usage(repository: Any, trace: dict[str, Any]) -> None:
    """Attach measured receipts in the background ingest, outside the SSE path."""
    run = await repository.fetchrow(
        """SELECT harness_thread_id FROM assistant_runs
           WHERE run_id = $1::uuid AND tenant_id = $2 AND user_id = $3
             AND session_id = $4 AND engine = 'agent_runtime'
             AND status IN ('completed', 'succeeded', 'failed', 'cancelled')""",
        trace["run_id"], trace["tenant_id"], trace["user_id"], trace["session_id"],
    )
    if not run or not run["harness_thread_id"]:
        return
    receipt = await repository.get_assistant_runtime_trace_model_usage(
        run_id=trace["run_id"], tenant_id=trace["tenant_id"], user_id=trace["user_id"],
        session_id=trace["session_id"], runtime_thread_id=str(run["harness_thread_id"]),
    )
    dispatched = int(receipt.get("dispatched_calls") or 0)
    measured = int(receipt.get("measured_calls") or 0)
    usage = {"dispatched_calls": dispatched, "measured_calls": measured,
             "tokens_complete": dispatched > 0 and measured == dispatched}
    trace["metadata"]["runtime_model_usage"] = usage
    if usage["tokens_complete"]:
        incoming, outgoing = int(receipt["input_tokens"]), int(receipt["output_tokens"])
        trace["metrics"].update(input_tokens=incoming, output_tokens=outgoing,
                                total_tokens=incoming + outgoing)
    if dispatched > 0 and int(receipt.get("cost_measured_calls") or 0) == dispatched:
        usage["cost_microusd"] = int(receipt["cost_microusd"])
        trace["metrics"]["total_cost_cents"] = usage["cost_microusd"] / 10_000


def _runtime_event(frame: bytes | str) -> tuple[str, dict[str, Any]]:
    text = frame.decode("utf-8", errors="ignore") if isinstance(frame, bytes) else frame
    event_type = next(
        (line[6:].strip() for line in text.splitlines() if line.startswith("event:")),
        "",
    )
    raw = next(
        (line[5:].strip() for line in text.splitlines() if line.startswith("data:")),
        "",
    )
    try:
        envelope = json.loads(raw)
    except (TypeError, json.JSONDecodeError):
        return event_type, {}
    data = envelope.get("data") if isinstance(envelope, dict) else None
    return event_type, dict(data) if isinstance(data, dict) else {}


def _terminal_status(event_type: str, data: dict[str, Any]) -> str | None:
    if event_type not in {"run_finished", "run_error", "cancelled"}:
        return None
    raw = str(data.get("status") or "").lower()
    if event_type == "cancelled" or raw == "cancelled":
        return "cancelled"
    if event_type == "run_finished" and raw in {"completed", "succeeded"}:
        return "succeeded"
    return "failed"


def _metric_int(value: Any) -> int:
    if isinstance(value, bool):
        return 0
    try:
        return max(0, int(value or 0))
    except (TypeError, ValueError, OverflowError):
        return 0


def build_assistant_runtime_trace(
    *,
    run_id: str,
    request_id: str,
    tenant_id: str,
    user_id: str,
    session_id: str,
    message: str,
    snapshot: dict[str, Any],
    status: str,
    started_at: float,
    ended_at: float,
    first_token_latency_ms: int,
    output: str,
    event_counts: dict[str, int],
    usage: dict[str, Any],
    error_type: str | None,
) -> dict[str, Any]:
    trace_id = str(uuid.UUID(run_id))
    started = datetime.fromtimestamp(started_at, tz=timezone.utc).isoformat()
    ended = datetime.fromtimestamp(ended_at, tz=timezone.utc).isoformat()
    duration_ms = max(0, int((ended_at - started_at) * 1000))
    model = snapshot.get("model") if isinstance(snapshot.get("model"), dict) else {}
    publication = (
        snapshot.get("publication")
        if isinstance(snapshot.get("publication"), dict)
        else {}
    )
    span_id = span_id_for(trace_id, "runtime_turn")
    metrics = {
        "total_latency_ms": duration_ms,
        "first_token_latency_ms": max(0, first_token_latency_ms),
        "input_tokens": _metric_int(usage.get("input_tokens")),
        "output_tokens": _metric_int(usage.get("output_tokens")),
        "total_tokens": _metric_int(usage.get("total_tokens")),
    }
    return {
        "trace_id": trace_id,
        "trace_family": "assistant",
        "workflow_kind": "agent_runtime_turn",
        "tenant_id": tenant_id,
        "user_id": user_id,
        "session_id": session_id,
        "thread_id": session_id,
        "run_id": run_id,
        "request_id": request_id or run_id,
        "model_id": model.get("id"),
        "provider": model.get("provider"),
        "status": status,
        "started_at": started,
        "ended_at": ended,
        "input_preview": redact_preview(message),
        "output_preview": redact_preview(output),
        "metrics": metrics,
        "privacy": {"payloads": "bounded_redacted_preview"},
        "redaction_state": {"input": "bounded", "output": "bounded"},
        "metadata": {
            "agent_id": snapshot.get("agent_id"),
            "agent_version_id": snapshot.get("agent_version_id"),
            "publication_id": publication.get("id"),
            "channel": publication.get("channel"),
            "runtime_trajectory": {
                "exit_reason": status,
                "event_counts": event_counts,
            },
            "error_type": error_type,
        },
        "source_adapter": "gateway.agent_runtime",
        "spans": [
            {
                "span_id": span_id,
                "span_kind": "agent_runtime",
                "name": "agent_runtime_turn",
                "status": status,
                "sequence_no": 0,
                "started_at": started,
                "ended_at": ended,
                "duration_ms": duration_ms,
                "input_preview": redact_preview(message),
                "output_preview": redact_preview(output),
                "attributes": {"run_id": run_id, "session_id": session_id},
                "error_type": error_type,
            }
        ],
        "events": [
            {
                "event_type": "agent_runtime_turn_finished",
                "sequence_no": 1,
                "payload": {"status": status, "event_counts": event_counts},
            }
        ],
    }


def runtime_model_evidence(
    *, evidence: dict[str, Any] | None, tenant_id: str, run_case_id: str,
    run_id: str, expected_model_ref: dict[str, Any],
) -> tuple[dict[str, Any], dict[str, Any], list[dict[str, Any]]]:
    """Project durable, scoped model receipts; never infer a call from SSE text."""
    if (not evidence or evidence.get("tenant_id") != tenant_id
            or evidence.get("session_id") != run_case_id or str(evidence.get("run_id")) != run_id):
        raise RuntimeError("AGENT_EVAL_MODEL_EVIDENCE_UNAVAILABLE")
    pricing = evidence.get("pricing_snapshot") or {}
    model_ref = {
        "tenant_id": tenant_id, "provider_id": evidence.get("provider_id"),
        "model_id": evidence.get("model_id"),
        "capability_revision": evidence.get("capability_revision"),
        "price_version": pricing.get("version"),
    }
    if any(value is None or value == "" for value in model_ref.values()):
        raise RuntimeError("AGENT_EVAL_MODEL_EVIDENCE_UNAVAILABLE")
    if any(expected_model_ref.get(key) != value for key, value in model_ref.items()):
        raise RuntimeError("AGENT_EVAL_MODEL_REF_MISMATCH")
    fingerprint = {
        "runtime_revision": evidence.get("runtime_revision"),
        "model_id": model_ref["model_id"], "provider": model_ref["provider_id"],
        "model_ref": model_ref, "model_ref_verified": True,
        "sampling": {"temperature": (evidence.get("parameters") or {}).get("temperature"),
                     "max_tokens": (evidence.get("limits") or {}).get("max_output_tokens")},
    }
    frozen_fingerprint = evidence.get("eval_fingerprint") or {}
    for key in ("system_prompt_hash", "tool_schema_hash"):
        value = frozen_fingerprint.get(key)
        if isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value):
            fingerprint[key] = value
    version_keys = ("agent_id", "agent_version_id", "agent_spec_hash", "agent_runtime_snapshot_hash")
    if all(evidence.get(key) for key in version_keys):
        fingerprint.update({key: str(evidence[key]) for key in version_keys})
        fingerprint["agent_spec_hash"] = fingerprint["agent_spec_hash"].removeprefix("sha256:")
        fingerprint["candidate_type"] = "agent_version"
    calls = [call for call in evidence.get("calls") or [] if call.get("dispatched_at")]
    usage: dict[str, Any] = {}
    # Missing measurements stay unknown; reserved budgets are not observed usage.
    for key in ("input_tokens", "output_tokens"):
        if calls and all(isinstance(call.get(key), int) for call in calls):
            usage[key] = sum(call[key] for call in calls)
    if "input_tokens" in usage and "output_tokens" in usage:
        usage["total_tokens"] = usage["input_tokens"] + usage["output_tokens"]
    spans = []
    for index, call in enumerate(calls, 1):
        started, ended = call["dispatched_at"], call.get("completed_at")
        spans.append({
            "span_id": span_id_for(run_id, f"model_call:{call['call_id']}"),
            "span_kind": "model_invocation", "name": "runtime_model_call",
            "sequence_no": index, "status": "succeeded" if call["status"] == "completed" else "failed",
            "started_at": started.isoformat(), "ended_at": ended.isoformat() if ended else None,
            "duration_ms": max(0, int((ended - started).total_seconds() * 1000)) if ended else None,
            "model_id": model_ref["model_id"], "provider": model_ref["provider_id"],
            "attributes": {"call_id": str(call["call_id"]), "ledger_status": call["status"],
                           "input_tokens": call.get("input_tokens"),
                           "output_tokens": call.get("output_tokens"),
                           "cost_microusd": call.get("cost_microusd"), "model_ref": model_ref},
            "error_type": call.get("error_code"),
        })
    for execution in evidence.get("tool_executions") or []:
        if not execution.get("capability_id"):
            continue
        attributes = {key: str(execution[key]) for key in (
            "execution_id", "effect", "approval_status", "quiz_id",
        ) if execution.get(key) is not None}
        spans.append({
            "span_id": span_id_for(run_id, f"capability:{execution['execution_id']}"),
            "span_kind": "tool_execution", "name": execution["capability_id"],
            "sequence_no": len(spans) + 1,
            "status": "succeeded" if execution["status"] == "succeeded" else "failed",
            "started_at": execution["dispatched_at"].isoformat() if execution.get("dispatched_at") else None,
            "ended_at": execution["terminal_at"].isoformat() if execution.get("terminal_at") else None,
            "attributes": attributes, "error_type": execution.get("error_code"),
        })
    return fingerprint, usage, spans


async def capture_assistant_runtime_stream(
    source: AsyncIterator[bytes | str],
    *,
    database: Any,
    run_id: str,
    request_id: str,
    tenant_id: str,
    user_id: str,
    session_id: str,
    message: str,
    snapshot: dict[str, Any],
    terminal_replay_source: Callable[[], AsyncIterator[bytes | str]] | None = None,
    _started_at: float | None = None,
    _first_token_latency_ms: int | None = None,
) -> AsyncIterator[bytes | str]:
    """Forward the stream byte-for-byte and schedule one terminal trace."""

    started_at = _started_at if _started_at is not None else time.time()
    first_token_at: float | None = None
    output: list[str] = []
    output_size = 0
    # The SSE connection is an observer. A closed browser stream does not
    # cancel or fail the independently owned Runtime turn.
    status = "running"
    usage: dict[str, Any] = {}
    event_counts: dict[str, int] = {}
    error_type: str | None = None
    try:
        async for frame in source:
            event_type, data = _runtime_event(frame)
            if event_type:
                event_counts[event_type] = event_counts.get(event_type, 0) + 1
            if event_type == "text_delta" and isinstance(data.get("content"), str):
                if first_token_at is None:
                    first_token_at = time.time()
                remaining = max(0, 2_000 - output_size)
                if remaining:
                    part = data["content"][:remaining]
                    output.append(part)
                    output_size += len(part)
            if isinstance(data.get("usage"), dict):
                usage.update(data["usage"])
            terminal = _terminal_status(event_type, data)
            if terminal is not None and str(data.get("run_id") or "") == run_id:
                status = terminal
            yield frame
    except BaseException as exc:
        if status == "running" or not isinstance(exc, (asyncio.CancelledError, GeneratorExit)):
            error_type = type(exc).__name__
        raise
    finally:
        ended_at = time.time()
        # A detached observer has no terminal fact. Ingesting a provisional
        # trace here could race with and overwrite a terminal trace for this
        # run, because the trace upsert accepts later status writes.
        if status != "running":
            with contextlib.suppress(Exception):
                trace = build_assistant_runtime_trace(
                    run_id=run_id,
                    request_id=request_id,
                    tenant_id=tenant_id,
                    user_id=user_id,
                    session_id=session_id,
                    message=message,
                    snapshot=snapshot,
                    status=status,
                    started_at=started_at,
                    ended_at=ended_at,
                    first_token_latency_ms=(
                        _first_token_latency_ms
                        if _first_token_latency_ms is not None
                        else int((first_token_at - started_at) * 1000) if first_token_at else 0
                    ),
                    output="".join(output),
                    event_counts=event_counts,
                    usage=usage,
                    error_type=error_type,
                )
                schedule_gateway_trace_ingest(
                    database,
                    tenant_id=tenant_id,
                    created_by=user_id,
                    trace=trace,
                    enqueue=True,
                )
        elif terminal_replay_source is not None:
            # The original observer has no terminal fact. A fresh read of the
            # same durable cursor reconstructs the full output and terminal
            # status without mutating or replaying the run. Keep the original
            # start time; a replayed first token has no trustworthy latency.
            with contextlib.suppress(Exception, asyncio.CancelledError):
                await source.aclose()  # type: ignore[attr-defined]

            async def replay_terminal() -> None:
                try:
                    async def drain() -> None:
                        async for _ in capture_assistant_runtime_stream(
                            terminal_replay_source(),
                            database=database,
                            run_id=run_id,
                            request_id=request_id,
                            tenant_id=tenant_id,
                            user_id=user_id,
                            session_id=session_id,
                            message=message,
                            snapshot=snapshot,
                            _started_at=started_at,
                            _first_token_latency_ms=(
                                int((first_token_at - started_at) * 1000)
                                if first_token_at is not None else 0
                            ),
                        ):
                            pass

                    await asyncio.wait_for(drain(), timeout=900)
                except Exception:
                    # No guessed terminal Trace: the run ledger remains the
                    # authority if its event source is unavailable.
                    pass

            task = asyncio.create_task(replay_terminal())
            _terminal_replay_tasks.add(task)
            task.add_done_callback(_terminal_replay_tasks.discard)


__all__ = ["build_assistant_runtime_trace", "capture_assistant_runtime_stream"]
