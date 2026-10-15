"""Strict V2 Agent Runtime observation contract shared by Eval and harness gates."""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from typing import Any


def _payload(event: Mapping[str, Any]) -> tuple[str, Mapping[str, Any]]:
    envelope = event.get("event")
    if not isinstance(envelope, Mapping):
        return "", {}
    raw = envelope.get("payload")
    if not isinstance(raw, Mapping):
        return "", {}
    data = raw.get("data")
    normalized = dict(data) if isinstance(data, Mapping) else {}
    if not normalized.get("run_id") and envelope.get("turn_id"):
        normalized["turn_id"] = envelope["turn_id"]
    return str(raw.get("event_type") or ""), normalized


def _identity(data: Mapping[str, Any], *keys: str) -> str:
    return str(next((data.get(key) for key in keys if data.get(key)), ""))


def validate_runtime_observation(
    thread: Mapping[str, Any], events: Iterable[Mapping[str, Any]]
) -> list[str]:
    failures: list[str] = []
    runtime = thread.get("runtime")
    if not isinstance(runtime, Mapping) or runtime.get("owner") != "agent_runtime":
        failures.append("thread.runtime.owner must be agent_runtime")

    sequences: list[int] = []
    types: list[str] = []
    starts: dict[str, Mapping[str, Any]] = {}
    results: dict[str, Mapping[str, Any]] = {}
    ends: dict[str, Mapping[str, Any]] = {}
    child_starts: dict[str, Mapping[str, Any]] = {}
    child_results: dict[str, Mapping[str, Any]] = {}
    compaction_receipt_expected = False
    retained_ids: list[str] = []
    output_text = ""
    root_run_id = ""
    terminals: list[tuple[str, Mapping[str, Any]]] = []

    def record_unique(
        values: dict[str, Mapping[str, Any]], identity: str, data: Mapping[str, Any], label: str
    ) -> None:
        if not identity:
            failures.append(f"{label} lacks identity")
        elif identity in values:
            failures.append(f"duplicate {label}: {identity}")
        else:
            values[identity] = data

    for event in events:
        sequence = event.get("sequence")
        if not isinstance(sequence, int):
            failures.append("event.sequence must be an integer")
        else:
            sequences.append(sequence)
        event_type, data = _payload(event)
        if not event_type:
            failures.append("event payload must contain event_type")
            continue
        types.append(event_type)
        if event_type == "run_started":
            run_id = _identity(data, "run_id", "turn_id")
            if not run_id:
                failures.append("run_started lacks run identity")
            elif root_run_id and root_run_id != run_id:
                failures.append("run identity changed within observation")
            else:
                root_run_id = run_id
        if event_type == "text_delta":
            output_text += str(data.get("content") or data.get("delta") or "")
        if event_type in {"tool_call_start", "compat/v1/tool_call_start", "tool_call"}:
            record_unique(
                starts,
                _identity(data, "call_id", "tool_call_id", "item_id"),
                data,
                "tool call start",
            )
        elif event_type in {"tool_call_result", "compat/v1/tool_call_result", "tool_result"}:
            record_unique(
                results,
                _identity(data, "call_id", "tool_call_id", "item_id"),
                data,
                "tool result",
            )
        elif event_type in {"tool_call_end", "compat/v1/tool_call_end"}:
            record_unique(
                ends,
                _identity(data, "call_id", "tool_call_id", "item_id"),
                data,
                "tool call end",
            )
        elif event_type in {"subagent_started", "child_turn_started"}:
            record_unique(
                child_starts,
                _identity(data, "agent_id", "child_id", "thread_id"),
                data,
                "child start",
            )
        elif event_type in {"subagent_finished", "child_turn_finished"}:
            record_unique(
                child_results,
                _identity(data, "agent_id", "child_id", "thread_id"),
                data,
                "child result",
            )
        elif event_type in {
            "compaction_completed",
            "context_compaction_completed",
            "context_compaction",
        }:
            if event_type == "context_compaction":
                if data.get("compacted") is not True:
                    failures.append("context_compaction must confirm compacted=true")
            else:
                compaction_receipt_expected = True
            raw_ids = data.get("retained_item_ids")
            if isinstance(raw_ids, list):
                retained_ids = [str(item) for item in raw_ids if item]
        if event_type in {
            "run_finished",
            "run_error",
            "run_stopped",
            "run_completed",
            "cancelled",
        }:
            terminals.append((event_type, data))

    if sequences != sorted(set(sequences)):
        failures.append("event sequences must be strictly increasing and unique")
    if "run_started" not in types:
        failures.append("run_started is required")
    if len(terminals) != 1:
        failures.append("exactly one terminal event is required")
    elif terminals:
        event_type, data = terminals[0]
        status = str(data.get("status") or "")
        allowed = {
            "run_finished": {"succeeded"},
            "run_completed": {"succeeded"},
            "run_error": {"failed", "cancelled"},
            "run_stopped": {"cancelled"},
            "cancelled": {"cancelled"},
        }[event_type]
        if status not in allowed:
            failures.append(f"invalid terminal status: {event_type}/{status or 'missing'}")
        terminal_run_id = _identity(data, "run_id", "turn_id")
        if root_run_id and terminal_run_id != root_run_id:
            failures.append("terminal run identity does not match run_started")

    started_ids = set(starts)
    for label, values in (("tool results", results), ("tool ends", ends)):
        missing = sorted(started_ids - set(values))
        orphan = sorted(set(values) - started_ids)
        if missing:
            failures.append(f"unpaired {label}: {','.join(missing)}")
        if orphan:
            failures.append(f"orphan {label}: {','.join(orphan)}")
    if set(child_starts) != set(child_results):
        failures.append("child lifecycle receipts must pair exactly")
    if compaction_receipt_expected and not retained_ids:
        failures.append("compaction must emit retained item ids")
    stopped = bool(terminals) and (
        terminals[0][0] in {"run_stopped", "cancelled"}
        or terminals[0][0] == "run_error" and terminals[0][1].get("status") == "cancelled"
    )
    if stopped and (set(starts) != set(results) or set(starts) != set(ends)):
        failures.append("stopped turns must close every published tool call")
    if terminals and terminals[0][0] in {"run_finished", "run_completed"} and not output_text:
        failures.append("successful candidate must emit visible output")
    return failures


def assert_runtime_observation(
    thread: Mapping[str, Any], events: Iterable[Mapping[str, Any]]
) -> None:
    failures = validate_runtime_observation(thread, events)
    if failures:
        raise ValueError("; ".join(failures))


__all__ = ["assert_runtime_observation", "validate_runtime_observation"]
