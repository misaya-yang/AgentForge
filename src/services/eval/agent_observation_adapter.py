"""Conservative projection from recorded Assistant artifacts to Eval observations.

The adapter is intentionally evidence preserving: it only projects fields that
are present in canonical turn events, terminal envelopes, context snapshots, or
durable checkpoints.  Unsupported claims remain ``unknown`` and therefore
cannot satisfy the hard stateful gate.
"""

from __future__ import annotations

import hashlib
import json
import math
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .golden import load_jsonl

PRODUCER_EVIDENCE_SCHEMA_VERSION = "assistant-producer-evidence/v1"
ADAPTER_SCHEMA_VERSION = "assistant-producer-observation/v1"
RUN_BUDGET_SCHEMA_VERSION = "assistant-run-budget/v1"
TURN_CONTRACT_SCHEMA_VERSION = "assistant-turn-contract/v1"

BUDGET_LIMIT_KEYS = frozenset(
    {
        "max_model_turns",
        "max_tool_calls",
        "max_parallel_tool_calls",
        "max_wall_time_seconds",
        "max_tool_result_bytes",
    }
)
BUDGET_USAGE_KEYS = frozenset(
    {"model_turns", "tool_calls", "tool_result_bytes", "elapsed_ms"}
)
BUDGET_REMAINING_KEYS = frozenset(
    {"model_turns", "tool_calls", "tool_result_bytes", "wall_time_ms"}
)
BUDGET_SNAPSHOT_KEYS = frozenset(
    {"schema_version", "limits", "usage", "remaining", "exhausted", "reason"}
)
BUDGET_EXHAUSTION_LIMIT_KEYS = {
    "model_turns": "max_model_turns",
    "tool_calls": "max_tool_calls",
    "parallel_tool_calls": "max_parallel_tool_calls",
    "wall_time": "max_wall_time_seconds",
    "tool_result_bytes": "max_tool_result_bytes",
}

ADAPTER_COMPONENTS = (
    "binding",
    "plan",
    "tool_pairing",
    "budget",
    "hitl",
    "compaction",
    "security",
)
CONTROL_ARGUMENT_KEYS = {
    "_approval_id",
    "_middleware_approval_required",
    "_steer_payload",
}
TERMINAL_TOOL_STATUSES = {
    "budget_rejected",
    "cancelled",
    "completed",
    "deduplicated",
    "denied",
    "error",
    "failed",
    "invalid_arguments",
    "not_executed",
    "side_effect_unknown",
    "succeeded",
    "timeout",
}
SUCCESSFUL_TOOL_STATUSES = {"completed", "deduplicated", "succeeded"}
TERMINAL_EVENT_TYPES = {
    "approval_required",
    "run_error",
    "run_finished",
    "side_effect_unknown",
}
IDENTITY_BOUND_EVENT_TYPES = TERMINAL_EVENT_TYPES | {
    "approval_result",
    "run_budget_exceeded",
    "tool_call_end",
    "tool_call_result",
    "tool_call_start",
}


@dataclass(frozen=True)
class _Event:
    turn_index: int
    event_index: int
    event_type: str
    data: dict[str, Any]

    @property
    def position(self) -> tuple[int, int]:
        return self.turn_index, self.event_index


@dataclass
class _Turn:
    turn_index: int
    events: list[_Event]
    terminal_envelope: dict[str, Any] | None
    context_snapshot: dict[str, Any] | None
    checkpoint: dict[str, Any] | None


def load_producer_artifacts(path: str | Path) -> list[dict[str, Any]]:
    """Load the versioned, recorded producer-artifact JSONL contract."""

    return load_jsonl(path)


def _canonical_json(value: Any) -> str:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        default=str,
    )


def _canonical_arguments_hash(arguments: dict[str, Any]) -> str:
    return hashlib.sha256(_canonical_json(arguments).encode("utf-8")).hexdigest()


def _runtime_arguments_hash(arguments: dict[str, Any]) -> str:
    encoded = json.dumps(
        arguments,
        ensure_ascii=False,
        sort_keys=True,
        default=str,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _nonempty_string(value: Any) -> str | None:
    return value.strip() if isinstance(value, str) and value.strip() else None


def _is_nonnegative_int(value: Any) -> bool:
    return not isinstance(value, bool) and isinstance(value, int) and value >= 0


def _is_positive_int(value: Any) -> bool:
    return not isinstance(value, bool) and isinstance(value, int) and value > 0


def _is_positive_finite_number(value: Any) -> bool:
    if isinstance(value, bool) or not isinstance(value, int | float):
        return False
    try:
        return value > 0 and math.isfinite(value)
    except OverflowError:
        return False


def _is_nonnegative_finite_number(value: Any) -> bool:
    if isinstance(value, bool) or not isinstance(value, int | float):
        return False
    try:
        return value >= 0 and math.isfinite(value)
    except OverflowError:
        return False


def _budget_snapshot_errors(snapshot: dict[str, Any], *, label: str) -> list[str]:
    errors: list[str] = []
    if frozenset(snapshot) != BUDGET_SNAPSHOT_KEYS:
        return [f"{label} has invalid snapshot fields"]
    if snapshot.get("schema_version") != RUN_BUDGET_SCHEMA_VERSION:
        errors.append(f"{label} has an unsupported schema")
    limits = snapshot.get("limits")
    usage = snapshot.get("usage")
    remaining = snapshot.get("remaining")
    if not isinstance(limits, dict) or frozenset(limits) != BUDGET_LIMIT_KEYS:
        errors.append(f"{label} has invalid limit fields")
    if not isinstance(usage, dict) or frozenset(usage) != BUDGET_USAGE_KEYS:
        errors.append(f"{label} has invalid usage fields")
    if not isinstance(remaining, dict) or frozenset(remaining) != BUDGET_REMAINING_KEYS:
        errors.append(f"{label} has invalid remaining fields")
    if errors:
        return errors
    assert isinstance(limits, dict)
    assert isinstance(usage, dict)
    assert isinstance(remaining, dict)
    for key in (
        "max_model_turns",
        "max_tool_calls",
        "max_parallel_tool_calls",
        "max_tool_result_bytes",
    ):
        if not _is_positive_int(limits[key]):
            errors.append(f"{label} limit {key} must be a positive integer")
    if not _is_positive_finite_number(limits["max_wall_time_seconds"]):
        errors.append(f"{label} limit max_wall_time_seconds must be positive and finite")
    for key in BUDGET_USAGE_KEYS:
        if not _is_nonnegative_int(usage[key]):
            errors.append(f"{label} usage {key} must be a non-negative integer")
    for key in BUDGET_REMAINING_KEYS:
        if not _is_nonnegative_int(remaining[key]):
            errors.append(f"{label} remaining {key} must be a non-negative integer")
    exhausted = snapshot.get("exhausted")
    reason = snapshot.get("reason")
    if not isinstance(exhausted, bool):
        errors.append(f"{label} exhausted must be boolean")
    elif exhausted:
        if reason not in {
            f"{dimension}_exhausted" for dimension in BUDGET_EXHAUSTION_LIMIT_KEYS
        }:
            errors.append(f"{label} has an unsupported exhaustion reason")
    elif reason is not None:
        errors.append(f"{label} non-exhausted snapshot must not have a reason")
    if errors:
        return errors

    wall_time_limit_ms = math.ceil(float(limits["max_wall_time_seconds"]) * 1000)
    expected_remaining = {
        "model_turns": max(0, limits["max_model_turns"] - usage["model_turns"]),
        "tool_calls": max(0, limits["max_tool_calls"] - usage["tool_calls"]),
        "tool_result_bytes": max(
            0,
            limits["max_tool_result_bytes"] - usage["tool_result_bytes"],
        ),
        "wall_time_ms": max(0, wall_time_limit_ms - usage["elapsed_ms"]),
    }
    if remaining != expected_remaining:
        errors.append(f"{label} remaining counters are inconsistent")
    for usage_key, limit_key in (
        ("model_turns", "max_model_turns"),
        ("tool_calls", "max_tool_calls"),
        ("tool_result_bytes", "max_tool_result_bytes"),
    ):
        if usage[usage_key] > limits[limit_key]:
            errors.append(f"{label} usage {usage_key} exceeds its limit")
    if exhausted and reason == "model_turns_exhausted" and (
        usage["model_turns"] != limits["max_model_turns"]
    ):
        errors.append(f"{label} model-turn exhaustion is inconsistent")
    if exhausted and reason == "wall_time_exhausted" and (
        usage["elapsed_ms"] < wall_time_limit_ms
    ):
        errors.append(f"{label} wall-time exhaustion is inconsistent")
    if not exhausted and usage["elapsed_ms"] > wall_time_limit_ms:
        errors.append(f"{label} elapsed wall time exceeds its limit")
    return errors


def _budget_exhaustion_event_errors(
    event_data: dict[str, Any],
    snapshot: dict[str, Any],
    *,
    label: str,
) -> list[str]:
    errors: list[str] = []
    dimension = _nonempty_string(event_data.get("dimension"))
    reason = _nonempty_string(event_data.get("reason"))
    if dimension not in BUDGET_EXHAUSTION_LIMIT_KEYS:
        return [f"{label} has an unsupported exhaustion dimension"]
    expected_reason = f"{dimension}_exhausted"
    if reason != expected_reason or snapshot.get("reason") != expected_reason:
        errors.append(f"{label} exhaustion dimension/reason is inconsistent")
    limits = snapshot.get("limits")
    usage = snapshot.get("usage")
    if not isinstance(limits, dict) or not isinstance(usage, dict):
        return [*errors, f"{label} has no valid bound budget snapshot"]
    expected_limit = limits.get(BUDGET_EXHAUSTION_LIMIT_KEYS[dimension])
    limit = event_data.get("limit")
    used = event_data.get("used")
    requested = event_data.get("requested")
    integer_dimension = dimension != "wall_time"
    if integer_dimension:
        if any(
            isinstance(value, bool) or not isinstance(value, int) or value < 0
            for value in (limit, used, requested)
        ):
            errors.append(f"{label} limit/used/requested must be non-negative integers")
            return errors
    elif any(
        not _is_nonnegative_finite_number(value)
        for value in (limit, used, requested)
    ):
        errors.append(f"{label} wall-time receipt must use finite non-negative numbers")
        return errors
    if limit != expected_limit:
        errors.append(f"{label} exhaustion limit does not match the budget snapshot")
    if dimension == "model_turns":
        if used != usage.get("model_turns") or requested != used + 1 or requested <= limit:
            errors.append(f"{label} model-turn exhaustion arithmetic is inconsistent")
    elif dimension == "tool_calls":
        if used != usage.get("tool_calls") or requested < used or requested <= limit:
            errors.append(f"{label} tool-call exhaustion arithmetic is inconsistent")
    elif dimension == "tool_result_bytes":
        if (
            used != usage.get("tool_result_bytes")
            or requested < used
            or requested <= limit
        ):
            errors.append(f"{label} tool-result exhaustion arithmetic is inconsistent")
    elif dimension == "parallel_tool_calls":
        if used != 0 or requested <= limit:
            errors.append(f"{label} parallel-tool exhaustion arithmetic is inconsistent")
    else:
        elapsed_ms = usage.get("elapsed_ms")
        if (
            used != requested
            or requested < limit
            or not isinstance(elapsed_ms, int)
            or elapsed_ms < math.ceil(float(used) * 1000)
        ):
            errors.append(f"{label} wall-time exhaustion arithmetic is inconsistent")
    return errors


def _identity(value: dict[str, Any] | None) -> dict[str, str]:
    if not isinstance(value, dict):
        return {}
    identity: dict[str, str] = {}
    for key in ("run_id", "tenant_id"):
        normalized = _nonempty_string(value.get(key))
        if normalized:
            identity[key] = normalized
    session_id = _nonempty_string(value.get("session_id"))
    thread_id = _nonempty_string(value.get("thread_id"))
    if session_id and thread_id and session_id != thread_id:
        identity["session_conflict"] = f"{session_id}!={thread_id}"
    elif session_id or thread_id:
        identity["session_id"] = session_id or thread_id or ""
    return identity


def _coalesce_objects(
    values: list[dict[str, Any]],
    *,
    label: str,
    errors: list[str],
) -> dict[str, Any] | None:
    if not values:
        return None
    signatures = {_canonical_json(value) for value in values}
    if len(signatures) != 1:
        errors.append(f"conflicting {label} receipts")
        return None
    return dict(values[0])


def _normalize_turns(producer: dict[str, Any]) -> tuple[list[_Turn], list[str]]:
    raw_turns = producer.get("turns")
    if not isinstance(raw_turns, list):
        return [], ["producer turns must be a list"]
    if not raw_turns:
        return [], ["producer requires at least one canonical turn"]
    turns: list[_Turn] = []
    errors: list[str] = []
    seen_turn_indices: set[int] = set()
    for row_index, raw_turn in enumerate(raw_turns, start=1):
        if not isinstance(raw_turn, dict):
            errors.append(f"turn row {row_index} must be an object")
            continue
        turn_index = raw_turn.get("turn_index")
        if (
            isinstance(turn_index, bool)
            or not isinstance(turn_index, int)
            or turn_index <= 0
            or turn_index in seen_turn_indices
        ):
            errors.append(f"turn row {row_index} has invalid or duplicate turn_index")
            continue
        seen_turn_indices.add(turn_index)
        raw_events = raw_turn.get("events")
        if not isinstance(raw_events, list):
            errors.append(f"turn {turn_index} events must be a list")
            raw_events = []
        events: list[_Event] = []
        terminal_seen = False
        terminal_events: list[_Event] = []
        for event_index, raw_event in enumerate(raw_events, start=1):
            if not isinstance(raw_event, dict):
                errors.append(f"turn {turn_index} event {event_index} must be an object")
                continue
            event_type = _nonempty_string(raw_event.get("event_type"))
            if not event_type:
                errors.append(f"turn {turn_index} event {event_index} has no event_type")
                continue
            if terminal_seen:
                errors.append(f"turn {turn_index} emitted {event_type} after a terminal event")
            raw_data = raw_event.get("data")
            data = dict(raw_data) if isinstance(raw_data, dict) else {}
            if event_type in {
                "approval_required",
                "approval_result",
                "run_budget_exceeded",
                "run_error",
                "run_finished",
                "side_effect_unknown",
                "tool_call_end",
                "tool_call_result",
                "tool_call_start",
            } and not isinstance(raw_data, dict):
                errors.append(
                    f"turn {turn_index} event {event_index} requires an object payload"
                )
            events.append(_Event(turn_index, event_index, event_type, data))
            if event_type in TERMINAL_EVENT_TYPES:
                terminal_seen = True
                terminal_events.append(events[-1])

        envelope_candidates = [
            value
            for value in [
                raw_turn.get("terminal_envelope"),
                *[event.data.get("terminal_envelope") for event in events],
            ]
            if isinstance(value, dict) and value
        ]
        terminal_envelope = _coalesce_objects(
            envelope_candidates,
            label=f"turn {turn_index} terminal envelope",
            errors=errors,
        )
        if len(terminal_events) != 1:
            errors.append(f"turn {turn_index} requires exactly one terminal event")
        elif terminal_envelope is None:
            errors.append(f"turn {turn_index} terminal event has no terminal envelope")
        else:
            terminal_event = terminal_events[0]
            terminal_type = terminal_event.event_type
            event_envelope = terminal_event.data.get("terminal_envelope")
            if not isinstance(event_envelope, dict) or not event_envelope:
                errors.append(
                    f"turn {turn_index} terminal event has no terminal envelope payload"
                )
            elif _canonical_json(event_envelope) != _canonical_json(terminal_envelope):
                errors.append(
                    f"turn {turn_index} terminal event conflicts with terminal envelope"
                )
            envelope_status = terminal_envelope.get("status")
            exit_reason = _nonempty_string(terminal_envelope.get("exit_reason"))
            terminal_consistent = {
                "approval_required": (
                    envelope_status == "blocked" and exit_reason == "approval_pending"
                ),
                "run_error": (
                    (envelope_status == "cancelled" and exit_reason == "cancelled")
                    or (
                        envelope_status == "failed"
                        and exit_reason
                        not in {
                            None,
                            "approval_pending",
                            "cancelled",
                            "side_effect_unknown",
                            "succeeded",
                        }
                    )
                ),
                "run_finished": (
                    envelope_status == "succeeded"
                    and exit_reason in {"max_iterations", "succeeded"}
                ),
                "side_effect_unknown": (
                    envelope_status == "blocked" and exit_reason == "side_effect_unknown"
                ),
            }[terminal_type]
            if not terminal_consistent:
                errors.append(
                    f"turn {turn_index} terminal type conflicts with envelope status/exit_reason"
                )
            if terminal_envelope.get("schema_version") != TURN_CONTRACT_SCHEMA_VERSION:
                errors.append(
                    f"turn {turn_index} terminal envelope requires schema "
                    f"{TURN_CONTRACT_SCHEMA_VERSION}"
                )
        snapshot_candidates = [
            value
            for value in [
                raw_turn.get("context_snapshot"),
                *[event.data.get("context_snapshot") for event in events],
                (terminal_envelope or {}).get("context_snapshot"),
            ]
            if isinstance(value, dict) and value
        ]
        context_snapshot = _coalesce_objects(
            snapshot_candidates,
            label=f"turn {turn_index} context snapshot",
            errors=errors,
        )
        if (
            context_snapshot is not None
            and context_snapshot.get("schema_version") != TURN_CONTRACT_SCHEMA_VERSION
        ):
            errors.append(
                f"turn {turn_index} context snapshot requires schema "
                f"{TURN_CONTRACT_SCHEMA_VERSION}"
            )
        raw_checkpoint = raw_turn.get("checkpoint")
        checkpoint = dict(raw_checkpoint) if isinstance(raw_checkpoint, dict) else None
        if raw_checkpoint is not None and not isinstance(raw_checkpoint, dict):
            errors.append(f"turn {turn_index} checkpoint must be an object")
        turns.append(
            _Turn(
                turn_index=turn_index,
                events=events,
                terminal_envelope=terminal_envelope,
                context_snapshot=context_snapshot,
                checkpoint=checkpoint,
            )
        )
    turns.sort(key=lambda item: item.turn_index)
    if [turn.turn_index for turn in turns] != list(range(1, len(turns) + 1)):
        errors.append("turn_index values must be contiguous and one-based")
    return turns, errors


def _binding_errors(trace: dict[str, Any], turns: list[_Turn]) -> list[str]:
    errors: list[str] = []
    trace_identity = _identity(trace)
    if set(trace_identity) != {"run_id", "tenant_id", "session_id"}:
        errors.append("trace must bind one run_id, tenant_id, and session_id")
    for turn in turns:
        receipts = (
            ("terminal envelope", turn.terminal_envelope),
            ("context snapshot", turn.context_snapshot),
            ("checkpoint", turn.checkpoint),
        )
        for receipt_label, receipt in receipts:
            if not isinstance(receipt, dict):
                continue
            receipt_identity = _identity(receipt)
            if "session_conflict" in receipt_identity:
                errors.append(
                    f"turn {turn.turn_index} {receipt_label} has conflicting session/thread identity"
                )
            if set(receipt_identity) != {"run_id", "tenant_id", "session_id"}:
                errors.append(
                    f"turn {turn.turn_index} {receipt_label} must independently bind "
                    "run_id, tenant_id, and session_id"
                )
            for field in ("run_id", "tenant_id", "session_id"):
                if (
                    field in receipt_identity
                    and field in trace_identity
                    and receipt_identity[field] != trace_identity[field]
                ):
                    errors.append(
                        f"turn {turn.turn_index} {receipt_label} crosses {field}"
                    )
        for event in turn.events:
            event_identity = _identity(event.data)
            if event.event_type in IDENTITY_BOUND_EVENT_TYPES and not {
                "run_id",
                "session_id",
            }.issubset(event_identity):
                errors.append(
                    f"turn {turn.turn_index} event {event.event_index} must bind "
                    "run_id and session_id"
                )
            if "session_conflict" in event_identity:
                errors.append(
                    f"turn {turn.turn_index} event {event.event_index} has conflicting session"
                )
            for field in ("run_id", "tenant_id", "session_id"):
                if (
                    field in event_identity
                    and field in trace_identity
                    and event_identity[field] != trace_identity[field]
                ):
                    errors.append(
                        f"turn {turn.turn_index} event {event.event_index} crosses {field}"
                    )
    return errors


def _terminal_status(status: Any) -> str | None:
    normalized = _nonempty_string(status)
    return normalized if normalized in TERMINAL_TOOL_STATUSES else None


def _component(status: str, reason: str, **details: Any) -> dict[str, Any]:
    return {"status": status, "reason": reason, **details}


def _tool_projection(
    turns: list[_Turn],
    replay_turns: dict[int, dict[str, Any]],
    *,
    binding_verified: bool,
) -> tuple[dict[str, Any], dict[str, dict[str, Any]], dict[str, _Event]]:
    starts: dict[str, _Event] = {}
    results: dict[str, _Event] = {}
    ends: dict[str, _Event] = {}
    failures: list[str] = []
    for turn in turns:
        for event in turn.events:
            if event.event_type not in {
                "tool_call_start",
                "tool_call_result",
                "tool_call_end",
            }:
                continue
            call_id = _nonempty_string(event.data.get("tool_call_id"))
            if not call_id:
                failures.append(f"{event.event_type} missing tool_call_id")
                continue
            target = {
                "tool_call_start": starts,
                "tool_call_result": results,
                "tool_call_end": ends,
            }[event.event_type]
            if call_id in target:
                failures.append(f"duplicate {event.event_type} for {call_id}")
                continue
            target[call_id] = event

    if not starts:
        failures.append("no canonical tool lifecycle was recorded")
    if set(starts) != set(results) or set(starts) != set(ends):
        failures.append("tool lifecycle start/result/end IDs are not one-to-one")
    projected_starts: dict[str, dict[str, Any]] = {}
    for call_id, start in starts.items():
        result = results.get(call_id)
        end = ends.get(call_id)
        name = _nonempty_string(start.data.get("tool_name") or start.data.get("name"))
        arguments = start.data.get("arguments")
        result_status = _terminal_status((result.data if result else {}).get("status"))
        end_status = _terminal_status((end.data if end else {}).get("status"))
        if not name or not isinstance(arguments, dict):
            failures.append(f"tool_call_start {call_id} lacks name or object arguments")
            continue
        start_primary_name = _nonempty_string(start.data.get("tool_name"))
        start_alias_name = _nonempty_string(start.data.get("name"))
        if (
            start_primary_name
            and start_alias_name
            and start_primary_name != start_alias_name
        ):
            failures.append(f"tool lifecycle {call_id} has conflicting start names")
        unexpected_controls = sorted(CONTROL_ARGUMENT_KEYS.intersection(arguments))
        if unexpected_controls:
            failures.append(
                f"tool lifecycle {call_id} exposes control arguments: "
                + ", ".join(unexpected_controls)
            )
        if result is None or end is None:
            continue
        if len({start.turn_index, result.turn_index, end.turn_index}) != 1:
            failures.append(f"tool lifecycle {call_id} crosses a terminal turn boundary")
        if not start.position < result.position < end.position:
            failures.append(f"tool lifecycle {call_id} is out of order")
        if result_status is None or end_status is None or result_status != end_status:
            failures.append(f"tool lifecycle {call_id} has conflicting terminal status")
        for terminal in (result, end):
            terminal_primary_name = _nonempty_string(terminal.data.get("tool_name"))
            terminal_alias_name = _nonempty_string(terminal.data.get("name"))
            terminal_name = _nonempty_string(
                terminal.data.get("tool_name") or terminal.data.get("name")
            )
            if (
                terminal_primary_name
                and terminal_alias_name
                and terminal_primary_name != terminal_alias_name
            ):
                failures.append(f"tool lifecycle {call_id} has conflicting terminal names")
            if not terminal_name:
                failures.append(f"tool lifecycle {call_id} terminal receipt lacks tool name")
            elif terminal_name != name:
                failures.append(f"tool lifecycle {call_id} changes tool name")
            success = terminal.data.get("success")
            if not isinstance(success, bool):
                failures.append(f"tool lifecycle {call_id} terminal receipt lacks success")
            elif (
                _terminal_status(terminal.data.get("status"))
                in SUCCESSFUL_TOOL_STATUSES
            ) is not success:
                failures.append(
                    f"tool lifecycle {call_id} terminal status conflicts with success"
                )
        if (
            isinstance(result.data.get("success"), bool)
            and isinstance(end.data.get("success"), bool)
            and result.data["success"] is not end.data["success"]
        ):
            failures.append(f"tool lifecycle {call_id} has conflicting success receipts")
        projected = {
            "call_id": call_id,
            "name": name,
            "arguments": dict(arguments),
            "dispatch_state": "dispatched",
        }
        replay_turns[start.turn_index].setdefault("tool_calls", []).append(projected)
        replay_turns[result.turn_index].setdefault("tool_results", []).append(
            {"tool_call_id": call_id, "status": result.data.get("status")}
        )
        projected_starts[call_id] = projected
    if not binding_verified:
        failures.append("producer identity binding is not verified")
    status = "verified" if not failures else "unknown"
    return (
        _component(
            status,
            "canonical start/result/end lifecycle" if status == "verified" else "; ".join(failures),
            call_count=len(starts),
        ),
        projected_starts,
        results,
    )


def _budget_projection(
    turns: list[_Turn],
    replay_turns: dict[int, dict[str, Any]],
    *,
    binding_verified: bool,
) -> dict[str, Any]:
    failures: list[str] = []
    previous: dict[str, Any] | None = None
    if not turns:
        failures.append("no turns were recorded")
    for turn in turns:
        snapshots: list[tuple[str, dict[str, Any]]] = []
        snapshot = turn.context_snapshot or {}
        bootstrap = snapshot.get("bootstrap")
        if isinstance(bootstrap, dict) and isinstance(bootstrap.get("run_budget"), dict):
            snapshots.append(("start", bootstrap["run_budget"]))
        exceeded_events = [
            event for event in turn.events if event.event_type == "run_budget_exceeded"
        ]
        if len(exceeded_events) > 1:
            failures.append(f"turn {turn.turn_index} has duplicate budget exhaustion receipts")
        for event in turn.events:
            if event.event_type == "run_budget_exceeded":
                value = event.data.get("budget")
                if not isinstance(value, dict):
                    failures.append(
                        f"turn {turn.turn_index} budget exhaustion receipt has no budget snapshot"
                    )
                    continue
                snapshots.append(("exceeded", value))
                snapshot_errors = _budget_snapshot_errors(
                    value,
                    label=f"turn {turn.turn_index} exceeded budget snapshot",
                )
                failures.extend(snapshot_errors)
                reason = _nonempty_string(event.data.get("reason"))
                if (
                    event.data.get("schema_version") != RUN_BUDGET_SCHEMA_VERSION
                    or event.data.get("status") != "exhausted"
                    or not reason
                    or value.get("exhausted") is not True
                    or value.get("reason") != reason
                ):
                    failures.append(
                        f"turn {turn.turn_index} has inconsistent budget exhaustion receipt"
                    )
                envelope = turn.terminal_envelope or {}
                if (
                    envelope.get("status") != "failed"
                    or envelope.get("exit_reason") != "run_budget_exceeded"
                ):
                    failures.append(
                        f"turn {turn.turn_index} budget exhaustion conflicts with terminal envelope"
                    )
                if not snapshot_errors:
                    failures.extend(
                        _budget_exhaustion_event_errors(
                            event.data,
                            value,
                            label=f"turn {turn.turn_index} budget exhaustion receipt",
                        )
                    )
        envelope_usage = (
            turn.terminal_envelope.get("usage")
            if isinstance(turn.terminal_envelope, dict)
            else None
        )
        if isinstance(envelope_usage, dict) and isinstance(
            envelope_usage.get("run_budget"), dict
        ):
            snapshots.append(("terminal", envelope_usage["run_budget"]))
        receipt_kinds = {kind for kind, _candidate in snapshots}
        if "start" not in receipt_kinds or not receipt_kinds.intersection(
            {"exceeded", "terminal"}
        ):
            failures.append(
                f"turn {turn.turn_index} budget requires start and terminal/exceeded receipts"
            )
        budget = snapshots[-1][1] if snapshots else None
        if not isinstance(budget, dict):
            failures.append(f"turn {turn.turn_index} has no run budget snapshot")
            continue
        if exceeded_events:
            exhausted_reason = _nonempty_string(exceeded_events[0].data.get("reason"))
            if budget.get("exhausted") is not True or budget.get(
                "reason"
            ) != exhausted_reason:
                failures.append(
                    f"turn {turn.turn_index} terminal budget snapshot lost exhaustion state"
                )
        terminal_exit_reason = _nonempty_string(
            (turn.terminal_envelope or {}).get("exit_reason")
        )
        if terminal_exit_reason == "run_budget_exceeded" and len(exceeded_events) != 1:
            failures.append(
                f"turn {turn.turn_index} run_budget_exceeded terminal requires one exhaustion event"
            )
        for snapshot_index, (_kind, candidate) in enumerate(snapshots, start=1):
            snapshot_errors = _budget_snapshot_errors(
                candidate,
                label=f"turn {turn.turn_index} run budget snapshot {snapshot_index}",
            )
            failures.extend(snapshot_errors)
            if snapshot_errors:
                continue
            limits = candidate["limits"]
            usage = candidate["usage"]
            remaining = candidate["remaining"]
            if previous is not None:
                if limits != previous.get("limits"):
                    failures.append("run budget limits changed across recorded snapshots")
                previous_usage = previous.get("usage") or {}
                previous_remaining = previous.get("remaining") or {}
                for key in ("model_turns", "tool_calls", "tool_result_bytes", "elapsed_ms"):
                    left = previous_usage.get(key)
                    right = usage.get(key)
                    if right < left:
                        failures.append(f"run budget usage {key} is not monotonic")
                for key in ("model_turns", "tool_calls", "tool_result_bytes", "wall_time_ms"):
                    left = previous_remaining.get(key)
                    right = remaining.get(key)
                    if right > left:
                        failures.append(f"run budget remaining {key} is not monotonic")
                if previous.get("exhausted") is True and candidate.get("exhausted") is not True:
                    failures.append("run budget exhaustion regressed to non-exhausted")
            previous = candidate
        limits = budget.get("limits")
        usage = budget.get("usage")
        remaining = budget.get("remaining")
        if _budget_snapshot_errors(
            budget,
            label=f"turn {turn.turn_index} terminal budget snapshot",
        ):
            continue
        assert isinstance(limits, dict)
        assert isinstance(usage, dict)
        assert isinstance(remaining, dict)
        maximum = limits.get("max_model_turns")
        iteration = usage.get("model_turns")
        remaining_turns = remaining.get("model_turns")
        if (
            isinstance(maximum, bool)
            or not isinstance(maximum, int)
            or maximum <= 0
            or isinstance(iteration, bool)
            or not isinstance(iteration, int)
            or iteration <= 0
            or isinstance(remaining_turns, bool)
            or not isinstance(remaining_turns, int)
            or remaining_turns != maximum - iteration
        ):
            failures.append(f"turn {turn.turn_index} has inconsistent model-turn budget evidence")
            continue
        replay_turns[turn.turn_index]["budget"] = {
            "iteration": iteration,
            "max_iterations": maximum,
            "remaining": remaining_turns,
        }
    if not binding_verified:
        failures.append("producer identity binding is not verified")
    failures = list(dict.fromkeys(failures))
    status = "verified" if not failures else "unknown"
    return _component(
        status,
        "canonical run_budget snapshots" if status == "verified" else "; ".join(failures),
    )


def _hitl_projection(
    turns: list[_Turn],
    replay_turns: dict[int, dict[str, Any]],
    starts: dict[str, dict[str, Any]],
    results: dict[str, _Event],
    *,
    binding_verified: bool,
) -> dict[str, Any]:
    failures: list[str] = []
    pause_events = [
        event for turn in turns for event in turn.events if event.event_type == "approval_required"
    ]
    resume_events = [
        event for turn in turns for event in turn.events if event.event_type == "approval_result"
    ]
    approval_checkpoints = [
        (turn.turn_index, turn.checkpoint)
        for turn in turns
        if isinstance(turn.checkpoint, dict) and turn.checkpoint.get("phase") == "approval_pending"
    ]
    resume_checkpoints = [
        (turn.turn_index, turn.checkpoint)
        for turn in turns
        if isinstance(turn.checkpoint, dict)
        and turn.checkpoint.get("phase") == "tool_call_pending"
    ]
    if (
        len(pause_events) != 1
        or len(resume_events) != 1
        or len(approval_checkpoints) != 1
        or len(resume_checkpoints) != 1
    ):
        failures.append(
            "HITL requires one pause, one approval result, one pause checkpoint, "
            "and one resume checkpoint"
        )
    if failures:
        return _component("unknown", "; ".join(failures))

    pause = pause_events[0]
    resume = resume_events[0]
    checkpoint_turn, checkpoint = approval_checkpoints[0]
    _resume_checkpoint_turn, resume_checkpoint = resume_checkpoints[0]
    assert isinstance(checkpoint, dict)
    assert isinstance(resume_checkpoint, dict)
    checkpoint_id = _nonempty_string(checkpoint.get("checkpoint_id"))
    approval_id = _nonempty_string(checkpoint.get("approval_id"))
    pending = checkpoint.get("pending_tool")
    receipt = checkpoint.get("checkpoint_receipt")
    if (
        not checkpoint_id
        or not approval_id
        or checkpoint.get("status") != "blocked"
        or not isinstance(pending, dict)
        or not isinstance(receipt, dict)
        or receipt.get("committed") is not True
        or receipt.get("durability") != "database"
    ):
        failures.append("approval checkpoint is not a committed database-durable blocked receipt")
        pending = pending if isinstance(pending, dict) else {}
    tool_id = _nonempty_string(pending.get("tool_id"))
    tool_name = _nonempty_string(pending.get("tool_name"))
    persisted_hash = _nonempty_string(pending.get("arguments_hash"))
    if not tool_id or not tool_name or not persisted_hash or len(persisted_hash) != 64:
        failures.append("approval checkpoint lacks bound tool identity or arguments hash")
    start = starts.get(tool_id or "")
    result = results.get(tool_id or "")
    arguments = start.get("arguments") if isinstance(start, dict) else None
    if not isinstance(arguments, dict):
        failures.append("approved tool has no canonical dispatch arguments")
        arguments = {}
    elif _runtime_arguments_hash(arguments) != persisted_hash:
        failures.append("approved dispatch arguments do not match the checkpoint hash")
    pause_data = pause.data
    resume_data = resume.data
    resume_pending = resume_checkpoint.get("pending_tool")
    resume_receipt = resume_checkpoint.get("checkpoint_receipt")
    resume_checkpoint_id = _nonempty_string(resume_checkpoint.get("checkpoint_id"))
    if (
        not resume_checkpoint_id
        or resume_checkpoint_id == checkpoint_id
        or resume_checkpoint.get("status") != "running"
        or resume_checkpoint.get("approval_id") != approval_id
        or not isinstance(resume_pending, dict)
        or resume_pending.get("tool_id") != tool_id
        or resume_pending.get("tool_name") != tool_name
        or resume_pending.get("arguments_hash") != persisted_hash
        or not isinstance(resume_receipt, dict)
        or resume_receipt.get("committed") is not True
        or resume_receipt.get("durability") != "database"
    ):
        failures.append("resume checkpoint is not database-durable and bound to the approval")
    if (
        pause_data.get("checkpoint_id") != checkpoint_id
        or pause_data.get("approval_id") != approval_id
        or pause_data.get("tool_id") != tool_id
        or pause_data.get("tool_name") != tool_name
    ):
        failures.append("approval pause event does not match its checkpoint")
    start_events = [
        event
        for turn in turns
        for event in turn.events
        if event.event_type == "tool_call_start" and event.data.get("tool_call_id") == tool_id
    ]
    end_events = [
        event
        for turn in turns
        for event in turn.events
        if event.event_type == "tool_call_end" and event.data.get("tool_call_id") == tool_id
    ]
    start_event = start_events[0] if len(start_events) == 1 else None
    end_event = end_events[0] if len(end_events) == 1 else None
    resume_position = (_resume_checkpoint_turn, 0)
    if (
        resume_data.get("approval_id") != approval_id
        or resume_data.get("tool_id") != tool_id
        or resume_data.get("tool_name") != tool_name
        or resume_data.get("approved") is not True
        or result is None
        or start_event is None
        or end_event is None
        or _resume_checkpoint_turn != start_event.turn_index
        or not pause.position < resume_position < start_event.position < result.position < end_event.position
        or not end_event.position < resume.position
    ):
        failures.append("approval result/dispatch is missing, mismatched, or out of order")
    if set(starts) != ({tool_id} if tool_id else set()):
        failures.append("HITL receipt cannot classify unrelated tool calls")
    if CONTROL_ARGUMENT_KEYS.intersection(arguments):
        failures.append("approved dispatch exposed runtime control arguments")
    if not binding_verified:
        failures.append("producer identity binding is not verified")
    if failures:
        return _component("unknown", "; ".join(failures))

    comparable_arguments = dict(arguments)
    evaluator_hash = _canonical_arguments_hash(comparable_arguments)
    pending_call = {
        "call_id": tool_id,
        "name": tool_name,
        "arguments": comparable_arguments,
        "arguments_hash": evaluator_hash,
        "dispatch_state": "pending_approval",
        "approval_required": True,
        "checkpoint_id": checkpoint_id,
    }
    dispatched_call = starts[tool_id]
    dispatched_call.update(
        {
            "arguments": comparable_arguments,
            "arguments_hash": evaluator_hash,
            "dispatch_state": "dispatched",
            "approval_required": True,
            "checkpoint_id": checkpoint_id,
        }
    )
    replay_turns[checkpoint_turn].setdefault("tool_calls", []).insert(0, pending_call)
    replay_turns[pause.turn_index]["hitl"] = {
        "state": "paused",
        "checkpoint_id": checkpoint_id,
        "dispatch_count": 0,
    }
    replay_turns[resume.turn_index]["hitl"] = {
        "state": "resumed",
        "checkpoint_id": checkpoint_id,
        "approval_granted": True,
        "approved_call_ids": [tool_id],
        "approved_arguments_hashes": {tool_id: evaluator_hash},
        "dispatch_count": 1,
    }
    return _component(
        "verified",
        "durable checkpoint, approval receipt, and canonical dispatch are hash-bound",
        checkpoint_id=checkpoint_id,
        approval_id=approval_id,
    )


def adapt_producer_case(row: dict[str, Any]) -> tuple[str, dict[str, Any]]:
    """Adapt one producer row without manufacturing unsupported evidence."""

    case_id = _nonempty_string(row.get("case_id"))
    if not case_id:
        raise ValueError("producer artifact row requires a non-empty case_id")
    producer = row.get("producer")
    if not isinstance(producer, dict):
        raise ValueError(f"producer artifact {case_id!r} requires a producer object")
    if producer.get("schema_version") != PRODUCER_EVIDENCE_SCHEMA_VERSION:
        raise ValueError(
            f"producer artifact {case_id!r} requires schema "
            f"{PRODUCER_EVIDENCE_SCHEMA_VERSION!r}"
        )
    trace = producer.get("trace")
    trace = dict(trace) if isinstance(trace, dict) else {}
    turns, normalization_errors = _normalize_turns(producer)
    trace_status = _nonempty_string(trace.get("status"))
    envelopes = [turn.terminal_envelope for turn in turns if turn.terminal_envelope]
    final_envelope = envelopes[-1] if envelopes else {}
    envelope_status = _nonempty_string(final_envelope.get("status"))
    if trace_status and envelope_status and trace_status != envelope_status:
        normalization_errors.append("trace status conflicts with the final terminal envelope")
    binding_failures = [*normalization_errors, *_binding_errors(trace, turns)]
    binding_verified = not binding_failures

    replay_turns = {turn.turn_index: {"turn_index": turn.turn_index} for turn in turns}
    for turn in turns:
        identities = [
            _identity(value)
            for value in (turn.terminal_envelope, turn.context_snapshot, turn.checkpoint)
            if isinstance(value, dict)
        ]
        tenant_ids = {item["tenant_id"] for item in identities if "tenant_id" in item}
        if len(tenant_ids) == 1:
            replay_turns[turn.turn_index]["tenant_id"] = next(iter(tenant_ids))

    tool_component, starts, results = _tool_projection(
        turns,
        replay_turns,
        binding_verified=binding_verified,
    )
    budget_component = _budget_projection(
        turns,
        replay_turns,
        binding_verified=binding_verified,
    )
    hitl_component = _hitl_projection(
        turns,
        replay_turns,
        starts,
        results,
        binding_verified=binding_verified,
    )
    tool_observed = any(
        event.event_type in {"tool_call_start", "tool_call_result", "tool_call_end"}
        for turn in turns
        for event in turn.events
    )
    budget_observed = any(
        (
            isinstance((turn.context_snapshot or {}).get("bootstrap"), dict)
            and isinstance(
                (turn.context_snapshot or {}).get("bootstrap", {}).get("run_budget"),
                dict,
            )
        )
        or any(event.event_type == "run_budget_exceeded" for event in turn.events)
        or (
            isinstance((turn.terminal_envelope or {}).get("usage"), dict)
            and isinstance(
                (turn.terminal_envelope or {}).get("usage", {}).get("run_budget"),
                dict,
            )
        )
        for turn in turns
    )
    hitl_observed = any(
        event.event_type in {"approval_required", "approval_result"}
        for turn in turns
        for event in turn.events
    ) or any(
        isinstance(turn.checkpoint, dict)
        and turn.checkpoint.get("phase")
        in {"approval_pending", "tool_call_pending", "tool_call_completed"}
        for turn in turns
    )
    integrity_failures = list(binding_failures)
    for applicable, label, component in (
        (tool_observed, "tool_pairing", tool_component),
        (budget_observed, "budget", budget_component),
        (hitl_observed, "hitl", hitl_component),
    ):
        if applicable and component.get("status") != "verified":
            integrity_failures.append(
                f"observed {label} evidence is {component.get('status') or 'unknown'}"
            )
    integrity_verified = not integrity_failures

    span_kinds = trace.get("span_kinds")
    if not isinstance(span_kinds, list):
        spans = trace.get("spans")
        span_kinds = [
            str(span["span_kind"])
            for span in spans or []
            if isinstance(span, dict) and _nonempty_string(span.get("span_kind"))
        ]
    span_kinds = list(dict.fromkeys(str(item) for item in span_kinds if isinstance(item, str)))
    status = trace_status or envelope_status if integrity_verified else "unknown"
    replay: dict[str, Any] = {
        "status": status or "unknown",
        "output_preview": str(trace.get("output_preview") or ""),
        "span_kinds": span_kinds,
        "turns": [replay_turns[index] for index in sorted(replay_turns)],
        "source_adapter": {
            "name": "canonical_assistant_producer",
            "schema_version": ADAPTER_SCHEMA_VERSION,
        },
        "adapter_evidence": {
            "source": "canonical_assistant_producer",
            "schema_version": ADAPTER_SCHEMA_VERSION,
            "evidence_tier": "recorded_runtime_artifact",
            "integrity": _component(
                "verified" if integrity_verified else "not_verified",
                "all observed canonical receipts are internally consistent"
                if integrity_verified
                else "; ".join(integrity_failures),
            ),
            "components": {
                "binding": _component(
                    "verified" if binding_verified else "unknown",
                    "run/tenant/session receipts agree"
                    if binding_verified
                    else "; ".join(binding_failures),
                ),
                "plan": _component(
                    "unknown",
                    "producer has no stable plan identity plus per-turn completion receipt",
                ),
                "tool_pairing": tool_component,
                "budget": budget_component,
                "hitl": hitl_component,
                "compaction": _component(
                    "unknown",
                    "lineage does not prove literal retained/dropped required facts",
                ),
                "security": _component(
                    "unknown",
                    "producer has no prompt-policy or cross-tenant access decision receipt",
                ),
            },
        },
    }
    exit_reason = _nonempty_string(final_envelope.get("exit_reason"))
    if exit_reason:
        replay["exit_reason"] = exit_reason
    for key in (
        "cache_hit",
        "cached_tokens",
        "cost_cents",
        "input_tokens",
        "output_tokens",
        "total_latency_ms",
        "total_tokens",
        "trace_id",
    ):
        if key in trace:
            replay[key] = trace[key]
    return case_id, replay


def adapt_producer_artifacts(rows: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    """Adapt rows and reject ambiguous case joins."""

    observations: dict[str, dict[str, Any]] = {}
    for row in rows:
        case_id, replay = adapt_producer_case(row)
        if case_id in observations:
            raise ValueError(f"duplicate producer artifact case_id {case_id!r}")
        observations[case_id] = replay
    return observations


def summarize_adapter_evidence(
    observations: dict[str, dict[str, Any]],
) -> dict[str, Any]:
    """Return auditable per-component evidence status counts."""

    component_counts = {
        component: {"verified": 0, "unknown": 0, "not_run": 0}
        for component in ADAPTER_COMPONENTS
    }
    integrity_counts = {"verified": 0, "not_verified": 0}
    for replay in observations.values():
        evidence = replay.get("adapter_evidence")
        components = evidence.get("components") if isinstance(evidence, dict) else {}
        integrity = evidence.get("integrity") if isinstance(evidence, dict) else None
        integrity_status = (
            integrity.get("status") if isinstance(integrity, dict) else "not_verified"
        )
        normalized_integrity = (
            integrity_status if integrity_status == "verified" else "not_verified"
        )
        integrity_counts[normalized_integrity] += 1
        for component in ADAPTER_COMPONENTS:
            payload = components.get(component) if isinstance(components, dict) else None
            status = payload.get("status") if isinstance(payload, dict) else "unknown"
            normalized = status if status in {"verified", "unknown", "not_run"} else "unknown"
            component_counts[component][normalized] += 1
    return {
        "source_adapter": "canonical_assistant_producer",
        "schema_version": ADAPTER_SCHEMA_VERSION,
        "observation_count": len(observations),
        "status": (
            "verified"
            if observations and integrity_counts["not_verified"] == 0
            else "not_verified"
        ),
        "integrity": integrity_counts,
        "components": component_counts,
    }
