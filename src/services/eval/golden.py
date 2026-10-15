from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path
from typing import Any

from ai_gateway_core.security import redact_trace_text

from . import golden_validation as _golden_validation
from .golden_gate import (
    DEFAULT_GATE_THRESHOLDS as DEFAULT_GATE_THRESHOLDS,
)
from .golden_gate import (
    GATE_METRICS_SCHEMA_VERSION as GATE_METRICS_SCHEMA_VERSION,
)
from .golden_gate import (
    GATE_RATE_ABS_TOLERANCE as GATE_RATE_ABS_TOLERANCE,
)
from .golden_gate import (
    apply_gate as apply_gate,
)
from .golden_gate import (
    write_gate_report as write_gate_report,
)

# Compatibility facade: keep established imports and monkeypatch lookup sites stable.
SUPPORTED_ASSERTIONS = _golden_validation.SUPPORTED_ASSERTIONS
SUPPORTED_TOOL_EXPECTATION_FIELDS = _golden_validation.SUPPORTED_TOOL_EXPECTATION_FIELDS
SUPPORTED_STATEFUL_EXPECTATION_FIELDS = (
    _golden_validation.SUPPORTED_STATEFUL_EXPECTATION_FIELDS
)
SUPPORTED_STATEFUL_NESTED_FIELDS = _golden_validation.SUPPORTED_STATEFUL_NESTED_FIELDS
REQUIRED_STATEFUL_NESTED_FIELDS = _golden_validation.REQUIRED_STATEFUL_NESTED_FIELDS
STRING_ASSERTIONS = _golden_validation.STRING_ASSERTIONS
NUMERIC_ASSERTIONS = _golden_validation.NUMERIC_ASSERTIONS
validate_observations = _golden_validation.validate_observations
validate_case = _golden_validation.validate_case
_validate_stateful_expectations = _golden_validation._validate_stateful_expectations


# Eval observations currently converge two Assistant evidence sources: the
# canonical event collector emits ``completed``/``error`` while persisted
# agent-loop records also use explicit rejection outcomes.  Normalize those
# public producer values here so the pairing gate does not silently drift with
# one source or mistake a real terminal result for an in-flight call.
TERMINAL_TOOL_RESULT_STATUS_ALIASES = {
    "budget_rejected": "rejected",
    "cancelled": "cancelled",
    "completed": "succeeded",
    "deduplicated": "deduplicated",
    "denied": "denied",
    "error": "failed",
    "failed": "failed",
    "invalid_arguments": "rejected",
    "not_executed": "not_executed",
    "side_effect_unknown": "side_effect_unknown",
    "succeeded": "succeeded",
    "timeout": "timeout",
}

HITL_DISPATCH_RESULT_STATUSES = {
    "cancelled",
    "failed",
    "side_effect_unknown",
    "succeeded",
    "timeout",
}


def load_jsonl(path: str | Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for line_no, line in enumerate(Path(path).read_text(encoding="utf-8").splitlines(), start=1):
        stripped = line.strip()
        if not stripped:
            continue
        try:
            payload = json.loads(stripped)
        except json.JSONDecodeError as exc:
            raise ValueError(f"Invalid JSONL at line {line_no}: {exc}") from exc
        if not isinstance(payload, dict):
            raise ValueError(f"Golden case at line {line_no} must be an object")
        rows.append(payload)
    return rows


def load_observations(path: str | Path) -> dict[str, dict[str, Any]]:
    observations: dict[str, dict[str, Any]] = {}
    for index, row in enumerate(load_jsonl(path), start=1):
        case_id = row.get("case_id")
        if not isinstance(case_id, str) or not case_id:
            raise ValueError(f"Observation at line {index} must have a non-empty case_id")
        if case_id in observations:
            raise ValueError(f"Duplicate observation case_id {case_id!r}")
        replay = row.get("replay")
        if not isinstance(replay, dict) or not replay:
            raise ValueError(f"Observation {case_id!r} must have a non-empty replay object")
        observations[case_id] = replay
    return observations


def validate_cases(cases: list[dict[str, Any]]) -> dict[str, Any]:
    seen: set[str] = set()
    errors: list[dict[str, Any]] = []
    for index, case in enumerate(cases, start=1):
        case_id = str(case.get("case_id") or f"line-{index}")
        case_errors = validate_case(case)
        if case_id in seen:
            case_errors.append("duplicate case_id")
        seen.add(case_id)
        if case_errors:
            errors.append({"case_id": case_id, "errors": case_errors})
    return {"valid": not errors, "case_count": len(cases), "errors": errors}


def summarize_cases(cases: list[dict[str, Any]]) -> dict[str, Any]:
    by_split: dict[str, int] = {}
    by_difficulty: dict[str, int] = {}
    critical = 0
    tags: dict[str, int] = {}
    for case in cases:
        split = str(case.get("split") or "regression")
        by_split[split] = by_split.get(split, 0) + 1
        metadata = case.get("metadata") if isinstance(case.get("metadata"), dict) else {}
        difficulty = str(metadata.get("difficulty") or "unspecified")
        by_difficulty[difficulty] = by_difficulty.get(difficulty, 0) + 1
        if metadata.get("critical") is True:
            critical += 1
        for tag in metadata.get("tags") or []:
            if isinstance(tag, str) and tag:
                tags[tag] = tags.get(tag, 0) + 1
    return {
        "case_count": len(cases),
        "critical_count": critical,
        "by_split": by_split,
        "by_difficulty": by_difficulty,
        "tags": tags,
    }


def _contains_sensitive(value: Any) -> bool:
    text = (
        json.dumps(value, ensure_ascii=False, sort_keys=True, default=str)
        if not isinstance(value, str)
        else value
    )
    return redact_trace_text(text) != text


def _case_replay(case: dict[str, Any]) -> dict[str, Any]:
    metadata = case.get("metadata") if isinstance(case.get("metadata"), dict) else {}
    replay = metadata.get("replay") if isinstance(metadata.get("replay"), dict) else {}
    return replay


def _expected_strings(value: Any) -> list[str]:
    if isinstance(value, str):
        return [value.strip()] if value.strip() else []
    if isinstance(value, list):
        return [item.strip() for item in value if isinstance(item, str) and item.strip()]
    return []


def _tool_call_name(call: dict[str, Any]) -> str:
    name = str(call.get("name") or call.get("tool_name") or "").strip()
    return name.removeprefix("tool:")


def _tool_call_arguments(call: dict[str, Any]) -> dict[str, Any] | None:
    value = call.get("arguments", call.get("args", call.get("input_preview")))
    if isinstance(value, dict):
        return value
    if isinstance(value, str) and value.strip():
        try:
            parsed = json.loads(value)
        except json.JSONDecodeError:
            return None
        return parsed if isinstance(parsed, dict) else None
    return None


def _canonical_arguments_hash(call: dict[str, Any]) -> str | None:
    arguments = _tool_call_arguments(call)
    if arguments is None:
        return None
    encoded = json.dumps(
        arguments,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _normalized_terminal_tool_status(result: dict[str, Any]) -> str | None:
    status = result.get("status")
    if not isinstance(status, str) or not status:
        return None
    return TERMINAL_TOOL_RESULT_STATUS_ALIASES.get(status)


def _has_terminal_tool_result(result: dict[str, Any]) -> bool:
    return _normalized_terminal_tool_status(result) is not None


def _has_hitl_dispatch_result(result: dict[str, Any]) -> bool:
    return _normalized_terminal_tool_status(result) in HITL_DISPATCH_RESULT_STATUSES


def _observed_tool_calls(replay: dict[str, Any]) -> list[Any] | None:
    if "tool_calls" in replay:
        calls = replay.get("tool_calls")
        return calls if isinstance(calls, list) else None
    turns = replay.get("turns")
    if isinstance(turns, list):
        flattened: list[Any] = []
        for turn in turns:
            if not isinstance(turn, dict):
                return None
            turn_calls = turn.get("tool_calls")
            if turn_calls is None:
                continue
            if not isinstance(turn_calls, list):
                return None
            flattened.extend(
                call
                for call in turn_calls
                if not isinstance(call, dict)
                or call.get("dispatch_state") != "pending_approval"
            )
        return flattened
    if "spans" not in replay:
        return None
    spans = replay.get("spans")
    if not isinstance(spans, list):
        return None
    return [
        span
        for span in spans
        if isinstance(span, dict) and span.get("span_kind") == "tool_execution"
    ]


def _is_subset(expected: Any, actual: Any) -> bool:
    if isinstance(expected, dict):
        return isinstance(actual, dict) and all(
            key in actual and _is_subset(value, actual[key]) for key, value in expected.items()
        )
    return expected == actual


def _evaluate_tool_expectations(expected_tools: list[Any], replay: dict[str, Any]) -> list[str]:
    if not expected_tools:
        return []
    calls = _observed_tool_calls(replay)
    if calls is None:
        return ["expected tools require tool_calls or spans evidence"]
    if any(not isinstance(call, dict) for call in calls):
        return ["tool_calls evidence contains a non-object entry"]

    failures: list[str] = []
    typed_calls = [call for call in calls if isinstance(call, dict)]
    result_statuses_by_call_id: dict[str, list[str | None]] = {}
    result_groups: list[Any] = []
    if "tool_calls" in replay:
        if isinstance(replay.get("tool_results"), list):
            result_groups.append(replay["tool_results"])
    else:
        raw_turns = replay.get("turns")
        if isinstance(raw_turns, list):
            for turn in raw_turns:
                if not isinstance(turn, dict) or not isinstance(
                    turn.get("tool_results"), list
                ):
                    continue
                result_groups.append(turn["tool_results"])
    for result_group in result_groups:
        for result in result_group:
            if not isinstance(result, dict):
                continue
            result_id = result.get("tool_call_id")
            raw_status = result.get("status")
            if isinstance(result_id, str) and result_id:
                normalized = (
                    TERMINAL_TOOL_RESULT_STATUS_ALIASES.get(raw_status)
                    if isinstance(raw_status, str)
                    else None
                )
                result_statuses_by_call_id.setdefault(result_id, []).append(
                    normalized or (raw_status if isinstance(raw_status, str) else None)
                )
    for expected in expected_tools:
        if not isinstance(expected, dict):
            failures.append("invalid tool expectation")
            continue
        name = str(expected.get("name") or "").strip()
        matches = [
            (index, call)
            for index, call in enumerate(typed_calls, start=1)
            if _tool_call_name(call) == name
        ]
        if expected.get("forbidden") is True:
            if matches:
                failures.append(f"forbidden tool called {name!r}")
            continue
        if "max_calls" in expected and len(matches) > int(expected["max_calls"]):
            failures.append(f"tool {name!r} called {len(matches)} > {expected['max_calls']}")
        required = expected.get("required", True) is True
        if not matches:
            if required:
                failures.append(f"required tool not called {name!r}")
            continue

        candidates = matches
        if "order" in expected:
            candidates = [item for item in candidates if item[0] == expected["order"]]
        if "status" in expected:
            expected_status = str(expected["status"])
            expected_status = (
                TERMINAL_TOOL_RESULT_STATUS_ALIASES.get(expected_status) or expected_status
            )
            status_candidates: list[tuple[int, dict[str, Any]]] = []
            invalid_status_evidence = False
            for item in candidates:
                call = item[1]
                call_id = str(
                    call.get("call_id")
                    or call.get("id")
                    or call.get("tool_call_id")
                    or ""
                )
                result_statuses = result_statuses_by_call_id.get(call_id, [])
                raw_call_status = call.get("status")
                call_status = (
                    TERMINAL_TOOL_RESULT_STATUS_ALIASES.get(raw_call_status)
                    if isinstance(raw_call_status, str)
                    else None
                )
                if len(result_statuses) > 1 or (
                    len(result_statuses) == 1
                    and call_status is not None
                    and result_statuses[0] != call_status
                ):
                    invalid_status_evidence = True
                    continue
                actual_status = (
                    result_statuses[0]
                    if result_statuses
                    else call_status
                    or (raw_call_status if isinstance(raw_call_status, str) else None)
                )
                if actual_status == expected_status:
                    status_candidates.append(item)
            if invalid_status_evidence:
                failures.append(
                    f"tool {name!r} has duplicate or conflicting status evidence"
                )
            candidates = status_candidates
        if "arguments_subset" in expected:
            candidates = [
                item
                for item in candidates
                if _is_subset(expected["arguments_subset"], _tool_call_arguments(item[1]))
            ]
        if not candidates and (required or matches):
            failures.append(f"tool {name!r} did not match expected order, status, or arguments")
    return failures


def _failed_case_result(case: dict[str, Any], failure: str) -> dict[str, Any]:
    metadata = case.get("metadata") if isinstance(case.get("metadata"), dict) else {}
    expected_trajectory = (
        case.get("expected_trajectory")
        if isinstance(case.get("expected_trajectory"), dict)
        else {}
    )
    return {
        "case_id": case.get("case_id"),
        "score": 0.0,
        "passed": False,
        "critical": metadata.get("critical") is True,
        "trajectory_pass": False,
        "stateful_pass": False if expected_trajectory.get("stateful") else None,
        "failures": [failure],
    }


def _evaluate_assertions(assertions: list[Any], replay: dict[str, Any]) -> list[str]:
    failures: list[str] = []
    output = str(replay.get("output_preview") or "")
    spans = replay.get("span_kinds") if isinstance(replay.get("span_kinds"), list) else []
    for assertion in assertions:
        if not isinstance(assertion, dict):
            failures.append("invalid assertion")
            continue
        assertion_type = str(assertion.get("type") or "")
        value = assertion.get("value")
        if assertion_type == "output_equals":
            expected = str(value or "").strip()
            if not expected or output.strip() != expected:
                failures.append("output_equals differs from the required complete response")
        elif assertion_type == "output_contains":
            needle = str(value or "").strip()
            if not needle or needle.lower() not in output.lower():
                failures.append(f"output_contains missing {needle!r}")
        elif assertion_type == "output_not_contains":
            needle = str(value or "").strip()
            if not needle or needle.lower() in output.lower():
                failures.append(f"output_not_contains found {needle!r}")
        elif assertion_type == "required_span_kind":
            span_kind = str(value or "").strip()
            if not span_kind or span_kind not in spans:
                failures.append(f"required_span_kind missing {span_kind!r}")
        elif assertion_type == "no_sensitive_output":
            if _contains_sensitive(replay):
                failures.append("no_sensitive_output detected sensitive replay payload")
        elif assertion_type in NUMERIC_ASSERTIONS:
            evidence_key = {
                "latency_ms_lt": "total_latency_ms",
                "total_tokens_lt": "total_tokens",
                "cost_cents_lt": "total_cost_cents",
            }[assertion_type]
            try:
                limit = float(value)
                actual = float(replay.get(evidence_key))
                if not math.isfinite(actual) or actual < 0:
                    raise ValueError
            except (TypeError, ValueError):
                failures.append(f"{assertion_type} requires numeric evidence")
            else:
                if actual >= limit:
                    failures.append(f"{assertion_type} {actual:g} >= {limit:g}")
        elif assertion_type in {"tool_called", "tool_not_called"}:
            tool_name = str(value or "").strip()
            calls = _observed_tool_calls(replay)
            if calls is None or any(not isinstance(call, dict) for call in calls):
                failures.append(f"{assertion_type} requires valid tool_calls or spans evidence")
            else:
                observed_names = {_tool_call_name(call) for call in calls if isinstance(call, dict)}
                called = tool_name in observed_names
                if assertion_type == "tool_called" and not called:
                    failures.append(f"tool_called missing {tool_name!r}")
                elif assertion_type == "tool_not_called" and called:
                    failures.append(f"tool_not_called observed {tool_name!r}")
        elif assertion_type == "failure_mode_absent":
            expected_absent = str(value or "").strip()
            observed_modes: list[Any] = []
            evidence_present = False
            plural_modes = replay.get("failure_modes")
            if isinstance(plural_modes, list):
                observed_modes.extend(plural_modes)
                evidence_present = True
            scalar_mode = replay.get("failure_mode")
            if isinstance(scalar_mode, str) and scalar_mode.strip():
                observed_modes.append(scalar_mode)
                evidence_present = True
            if not evidence_present:
                failures.append("failure_mode_absent missing evidence")
            elif expected_absent and expected_absent in observed_modes:
                failures.append(f"failure_mode_absent observed {expected_absent!r}")
        else:
            failures.append(f"unsupported assertion type {assertion_type!r}")
    return failures


def _evaluate_runtime_expectations(case: dict[str, Any], replay: dict[str, Any]) -> list[str]:
    expected_trajectory = (
        case.get("expected_trajectory") if isinstance(case.get("expected_trajectory"), dict) else {}
    )
    runtime = (
        expected_trajectory.get("runtime")
        if isinstance(expected_trajectory.get("runtime"), dict)
        else {}
    )
    failures: list[str] = []
    for key, expected in runtime.items():
        if key == "expected_exit_reason":
            actual = replay.get("exit_reason")
            if actual != expected:
                failures.append(f"expected_exit_reason expected {expected!r}, got {actual!r}")
        elif key == "requires_gateway_decision":
            if expected is True and not replay.get("gateway_decision"):
                failures.append("requires_gateway_decision missing gateway_decision")
        elif key == "requires_arguments_hash":
            if expected is True and replay.get("arguments_hash_present") is not True:
                failures.append("requires_arguments_hash missing arguments_hash_present")
        elif key == "requires_sandbox_profile":
            actual = replay.get("sandbox_profile")
            if actual != expected:
                failures.append(f"requires_sandbox_profile expected {expected!r}, got {actual!r}")
        elif replay.get(key) != expected:
            failures.append(f"{key} expected {expected!r}, got {replay.get(key)!r}")
    return failures


def _stateful_turns(replay: dict[str, Any]) -> tuple[list[dict[str, Any]], list[str]]:
    raw_turns = replay.get("turns")
    if not isinstance(raw_turns, list) or not raw_turns:
        return [], ["stateful trajectory requires non-empty turns evidence"]
    if any(not isinstance(turn, dict) for turn in raw_turns):
        return [], ["stateful turns evidence contains a non-object entry"]
    turns = [turn for turn in raw_turns if isinstance(turn, dict)]
    indices = [turn.get("turn_index") for turn in turns]
    if any(isinstance(index, bool) or not isinstance(index, int) for index in indices):
        return [], ["stateful turn_index values must be integers"]
    if indices != list(range(1, len(turns) + 1)):
        return [], ["stateful turn_index values must be contiguous and one-based"]
    return turns, []


def _turn_tool_evidence(
    turns: list[dict[str, Any]],
) -> tuple[list[tuple[int, dict[str, Any]]], list[tuple[int, dict[str, Any]]]]:
    calls: list[tuple[int, dict[str, Any]]] = []
    results: list[tuple[int, dict[str, Any]]] = []
    for turn in turns:
        turn_index = int(turn.get("turn_index") or 0)
        raw_calls = turn.get("tool_calls")
        if isinstance(raw_calls, list):
            calls.extend(
                (turn_index, item) for item in raw_calls if isinstance(item, dict)
            )
        raw_results = turn.get("tool_results")
        if isinstance(raw_results, list):
            results.extend(
                (turn_index, item) for item in raw_results if isinstance(item, dict)
            )
    return calls, results


def _evaluate_stateful_expectations(case: dict[str, Any], replay: dict[str, Any]) -> list[str]:
    expected_trajectory = (
        case.get("expected_trajectory") if isinstance(case.get("expected_trajectory"), dict) else {}
    )
    stateful = (
        expected_trajectory.get("stateful")
        if isinstance(expected_trajectory.get("stateful"), dict)
        else {}
    )
    if not stateful:
        return []
    turns, failures = _stateful_turns(replay)
    adapter_evidence = replay.get("adapter_evidence")
    if isinstance(adapter_evidence, dict) and adapter_evidence.get(
        "source"
    ) == "canonical_assistant_producer":
        components = adapter_evidence.get("components")
        components = components if isinstance(components, dict) else {}
        binding = components.get("binding")
        binding_status = binding.get("status") if isinstance(binding, dict) else "unknown"
        if binding_status != "verified":
            failures.append(
                f"canonical producer evidence binding is {binding_status or 'unknown'}"
            )
        for section in stateful:
            if section == "minimum_turns":
                continue
            component = components.get(section)
            status = component.get("status") if isinstance(component, dict) else "unknown"
            if status != "verified":
                failures.append(
                    f"canonical producer evidence for {section} is {status or 'unknown'}"
                )
    if not turns:
        return failures
    minimum_turns = int(stateful.get("minimum_turns") or 2)
    if len(turns) < minimum_turns:
        failures.append(f"stateful turn count {len(turns)} < {minimum_turns}")

    for turn in turns:
        for evidence_key in ("tool_calls", "tool_results"):
            evidence = turn.get(evidence_key)
            if evidence is not None and (
                not isinstance(evidence, list)
                or any(not isinstance(item, dict) for item in evidence)
            ):
                failures.append(f"stateful {evidence_key} evidence must be an object list")
    calls, results = _turn_tool_evidence(turns)
    plan = stateful.get("plan") if isinstance(stateful.get("plan"), dict) else None
    if plan is not None:
        snapshots = [
            turn.get("plan") if isinstance(turn.get("plan"), dict) else None
            for turn in turns
        ]
        if any(snapshot is None for snapshot in snapshots):
            failures.append("plan retention requires a plan snapshot on every turn")
        else:
            typed_snapshots = [snapshot for snapshot in snapshots if isinstance(snapshot, dict)]
            expected_plan_id = str(plan.get("plan_id") or "")
            if any(
                not isinstance(snapshot.get("plan_id"), str)
                or snapshot.get("plan_id") != expected_plan_id
                for snapshot in typed_snapshots
            ):
                failures.append("plan_id changed or was missing across turns")
            expected_goal = str(plan.get("goal") or "")
            if expected_goal and any(
                not isinstance(snapshot.get("goal"), str)
                or snapshot.get("goal") != expected_goal
                for snapshot in typed_snapshots
            ):
                failures.append("plan goal changed or was missing across turns")
            required_steps = {str(item) for item in plan.get("required_steps") or []}
            for snapshot in typed_snapshots:
                raw_steps = snapshot.get("steps")
                if not isinstance(raw_steps, list) or any(
                    not isinstance(item, str) or not item.strip() for item in raw_steps
                ):
                    failures.append("plan steps evidence must be a non-empty string list")
                    observed_steps: set[str] = set()
                else:
                    observed_steps = set(raw_steps)
                if not required_steps.issubset(observed_steps):
                    failures.append("plan steps were lost across turns")
                    break
            final_completed = {
                str(item) for item in plan.get("final_completed_steps") or []
            }
            observed_completed: set[str] = set()
            previous_completed: set[str] = set()
            for snapshot in typed_snapshots:
                raw_completed = snapshot.get("completed_steps")
                if not isinstance(raw_completed, list) or any(
                    not isinstance(item, str) or not item.strip() for item in raw_completed
                ):
                    failures.append("completed plan steps evidence must be a string list")
                    observed_completed = set()
                    break
                observed_completed = set(raw_completed)
                if not previous_completed.issubset(observed_completed):
                    failures.append("completed plan steps regressed across turns")
                    break
                if not observed_completed.issubset(required_steps):
                    failures.append("completed plan steps are not part of the retained plan")
                    break
                previous_completed = observed_completed
            if not final_completed.issubset(observed_completed):
                failures.append("final plan snapshot is missing completed steps")

    if isinstance(stateful.get("tool_pairing"), dict):
        paired_calls = [
            (turn_index, call)
            for turn_index, call in calls
            if call.get("dispatch_state") != "pending_approval"
        ]
        call_ids = [
            call_id.strip() if isinstance(call_id := call.get("call_id"), str) else ""
            for _, call in paired_calls
        ]
        result_ids = [
            result_id.strip()
            if isinstance(result_id := result.get("tool_call_id"), str)
            else ""
            for _, result in results
        ]
        if not call_ids or any(not call_id for call_id in call_ids):
            failures.append("tool pairing requires non-empty call_id evidence")
        if len(call_ids) != len(set(call_ids)):
            failures.append("tool pairing contains duplicate call_id values")
        if any(not result_id for result_id in result_ids):
            failures.append("tool pairing requires non-empty tool_call_id results")
        if len(result_ids) != len(set(result_ids)):
            failures.append("tool pairing contains duplicate tool results")
        if any(not _has_terminal_tool_result(result) for _, result in results):
            failures.append("tool pairing requires terminal result status evidence")
        if set(call_ids) != set(result_ids):
            failures.append("tool calls and results are not one-to-one")
        call_turns = {
            call_id.strip(): turn_index
            for turn_index, call in paired_calls
            if isinstance(call_id := call.get("call_id"), str) and call_id.strip()
        }
        if any(
            result_id in call_turns and result_turn < call_turns[result_id]
            for result_turn, result in results
            if isinstance(result_id := result.get("tool_call_id"), str) and result_id.strip()
        ):
            failures.append("tool result appeared before its tool call")
        if stateful["tool_pairing"].get("require_success") is True and any(
            str(result.get("status") or "")
            not in {"completed", "deduplicated", "succeeded"}
            for _, result in results
        ):
            failures.append("tool pairing requires successful terminal results")

    budget = stateful.get("budget") if isinstance(stateful.get("budget"), dict) else None
    if budget is not None:
        max_iterations = int(budget.get("max_iterations") or 0)
        budget_evidence = [
            turn.get("budget") if isinstance(turn.get("budget"), dict) else None
            for turn in turns
        ]
        if any(item is None for item in budget_evidence):
            failures.append("budget termination requires budget evidence on every turn")
        else:
            typed_budgets = [item for item in budget_evidence if isinstance(item, dict)]
            observed_iterations: list[int] = []
            for item in typed_budgets:
                iteration = item.get("iteration")
                observed_max = item.get("max_iterations")
                remaining = item.get("remaining")
                if (
                    isinstance(iteration, bool)
                    or not isinstance(iteration, int)
                    or not 1 <= iteration <= max_iterations
                    or isinstance(observed_max, bool)
                    or not isinstance(observed_max, int)
                    or observed_max != max_iterations
                    or isinstance(remaining, bool)
                    or not isinstance(remaining, int)
                    or remaining < 0
                ):
                    failures.append("budget evidence exceeded or changed the iteration budget")
                    break
                observed_iterations.append(iteration)
                if remaining != max_iterations - iteration:
                    failures.append("budget remaining count is inconsistent with iteration")
                    break
            if observed_iterations != list(range(1, len(typed_budgets) + 1)):
                failures.append("budget iterations are not contiguous and one-based")
        expected_exit_reason = budget.get("expected_exit_reason")
        if replay.get("exit_reason") != expected_exit_reason:
            failures.append(
                f"budget exit reason expected {expected_exit_reason!r}, got {replay.get('exit_reason')!r}"
            )
        terminal_turn = int(budget.get("terminal_turn") or len(turns))
        if len(turns) != terminal_turn:
            failures.append("budget termination emitted turns after the terminal turn")
        if expected_exit_reason == "max_iterations" and budget_evidence:
            final_budget = budget_evidence[-1]
            if not isinstance(final_budget, dict) or (
                final_budget.get("iteration") != max_iterations
                or final_budget.get("remaining") != 0
            ):
                failures.append("max_iterations termination did not exhaust the iteration budget")
        if any(turn_index > terminal_turn for turn_index, _call in calls):
            failures.append("tool call emitted after budget termination")

    hitl = stateful.get("hitl") if isinstance(stateful.get("hitl"), dict) else None
    if hitl is not None:
        hitl_events = [
            (int(turn.get("turn_index") or 0), turn.get("hitl"))
            for turn in turns
            if isinstance(turn.get("hitl"), dict)
        ]
        if any(event.get("state") not in {"paused", "resumed"} for _, event in hitl_events):
            failures.append("HITL evidence contains an unsupported transition state")
        paused = [event for event in hitl_events if event[1].get("state") == "paused"]
        resumed = [event for event in hitl_events if event[1].get("state") == "resumed"]
        expected_checkpoint = str(hitl.get("checkpoint_id") or "")
        if len(paused) != 1 or len(resumed) != 1:
            failures.append("HITL pause/resume transition count mismatch")
        elif paused[0][0] >= resumed[0][0]:
            failures.append("HITL resume did not occur after pause")
        if any(
            str(event.get("checkpoint_id") or "") != expected_checkpoint
            for _, event in [*paused, *resumed]
        ):
            failures.append("HITL checkpoint identity changed across pause/resume")
        approved_specs = [
            item for item in hitl.get("approved_calls") or [] if isinstance(item, dict)
        ]
        approved_by_id = {str(item.get("call_id")): item for item in approved_specs}
        protected_tool_names = {
            str(tool_name) for tool_name in hitl.get("protected_tools") or []
        }
        bound_calls: list[tuple[int, dict[str, Any]]] = []
        for turn_index, call in calls:
            call_id = call.get("call_id")
            approval_required = call.get("approval_required")
            checkpoint_id = call.get("checkpoint_id")
            if not isinstance(approval_required, bool):
                failures.append("HITL approval_required evidence must be boolean")
            if "checkpoint_id" in call and (
                not isinstance(checkpoint_id, str) or not checkpoint_id
            ):
                failures.append("HITL call checkpoint_id must be a non-empty string")
            is_protected = _tool_call_name(call) in protected_tool_names
            if isinstance(approval_required, bool) and approval_required is not is_protected:
                failures.append(
                    "HITL approval_required conflicts with expected protected tool policy"
                )
            is_expected = isinstance(call_id, str) and call_id in approved_by_id
            if is_protected and (
                checkpoint_id != expected_checkpoint or not is_expected
            ):
                failures.append(
                    "HITL observed an unapproved or wrong-checkpoint protected call"
                )
            if is_expected:
                bound_calls.append((turn_index, call))
        if any(
            call.get("dispatch_state") not in {"pending_approval", "dispatched"}
            for _, call in bound_calls
        ):
            failures.append(
                "HITL approval-bound calls require pending_approval/dispatched state"
            )
        if paused and resumed:
            pause_turn = paused[0][0]
            resume_turn, resume_event = resumed[0]
            pending_count = 0
            dispatched_count = 0
            for call_id, expected_call in approved_by_id.items():
                transitions = [
                    (turn_index, call)
                    for turn_index, call in bound_calls
                    if call.get("call_id") == call_id
                ]
                pending = [
                    item
                    for item in transitions
                    if item[1].get("dispatch_state") == "pending_approval"
                ]
                dispatched = [
                    item
                    for item in transitions
                    if item[1].get("dispatch_state") == "dispatched"
                ]
                if len(pending) != 1 or pending[0][0] > pause_turn:
                    failures.append(
                        f"HITL approved call {call_id!r} requires one pending transition by pause"
                    )
                else:
                    pending_count += 1
                if len(dispatched) != 1 or dispatched[0][0] < resume_turn:
                    failures.append(
                        f"HITL approved call {call_id!r} requires one dispatch after resume"
                    )
                    dispatched_turn = None
                else:
                    dispatched_count += 1
                    dispatched_turn = dispatched[0][0]
                for _turn_index, call in [*pending, *dispatched]:
                    if (
                        call.get("approval_required") is not True
                        or call.get("checkpoint_id") != expected_checkpoint
                        or _tool_call_name(call) != expected_call.get("tool_name")
                        or call.get("arguments_hash") != expected_call.get("arguments_hash")
                        or _canonical_arguments_hash(call)
                        != expected_call.get("arguments_hash")
                    ):
                        failures.append(
                            f"HITL approved tool identity or arguments changed for {call_id!r}"
                        )
                        break
                matching_results = [
                    (result_turn, result)
                    for result_turn, result in results
                    if result.get("tool_call_id") == call_id
                ]
                if (
                    dispatched_turn is None
                    or len(matching_results) != 1
                    or matching_results[0][0] < dispatched_turn
                    or not _has_hitl_dispatch_result(matching_results[0][1])
                ):
                    failures.append(
                        f"HITL approved call {call_id!r} requires one terminal paired result"
                    )
            if pending_count < int(hitl.get("minimum_pending_approval_calls") or 0):
                failures.append("HITL pause is missing pending approval tool calls")
            if dispatched_count < int(hitl.get("minimum_postapproval_dispatches") or 0):
                failures.append("HITL resume has insufficient postapproval tool dispatches")
            preapproval_dispatches = sum(
                1
                for turn_index, call in bound_calls
                if call.get("dispatch_state") == "dispatched" and turn_index <= pause_turn
            )
            between_dispatches = sum(
                1
                for turn_index, call in bound_calls
                if call.get("dispatch_state") == "dispatched"
                and pause_turn < turn_index < resume_turn
            )
            if preapproval_dispatches > int(hitl.get("max_dispatch_before_approval") or 0):
                failures.append("HITL dispatched a tool before approval")
            if between_dispatches:
                failures.append("HITL dispatched a tool before the resume transition")
            pause_dispatch_count = paused[0][1].get("dispatch_count")
            if (
                isinstance(pause_dispatch_count, bool)
                or not isinstance(pause_dispatch_count, int)
                or pause_dispatch_count < 0
                or pause_dispatch_count != preapproval_dispatches
            ):
                failures.append("HITL dispatch_count disagrees with approval-bound tool calls")
            resume_dispatch_count = resume_event.get("dispatch_count")
            if (
                isinstance(resume_dispatch_count, bool)
                or not isinstance(resume_dispatch_count, int)
                or resume_dispatch_count < 0
                or resume_dispatch_count != dispatched_count
            ):
                failures.append(
                    "HITL resumed dispatch_count disagrees with approval-bound tool calls"
                )
            expected_ids = set(approved_by_id)
            expected_hashes = {
                call_id: str(item.get("arguments_hash"))
                for call_id, item in approved_by_id.items()
            }
            approved_call_ids = resume_event.get("approved_call_ids")
            valid_approved_ids = (
                isinstance(approved_call_ids, list)
                and bool(approved_call_ids)
                and all(isinstance(item, str) and bool(item) for item in approved_call_ids)
                and len(approved_call_ids) == len(set(approved_call_ids))
            )
            approved_hashes = resume_event.get("approved_arguments_hashes")
            valid_approved_hashes = (
                isinstance(approved_hashes, dict)
                and all(
                    isinstance(key, str)
                    and bool(key)
                    and isinstance(value, str)
                    and len(value) == 64
                    and all(character in "0123456789abcdef" for character in value)
                    for key, value in approved_hashes.items()
                )
            )
            if (
                resume_event.get("approval_granted") is not True
                or not valid_approved_ids
                or set(approved_call_ids) != expected_ids
                or not valid_approved_hashes
                or approved_hashes != expected_hashes
            ):
                failures.append("HITL resume is missing bound approval receipt evidence")

    compaction = (
        stateful.get("compaction")
        if isinstance(stateful.get("compaction"), dict)
        else None
    )
    if compaction is not None:
        expected_compaction_id = str(compaction.get("compaction_id") or "")
        compaction_events = [
            (int(turn.get("turn_index") or 0), turn.get("compaction"))
            for turn in turns
            if isinstance(turn.get("compaction"), dict)
        ]
        if any(
            event.get("event") not in {"compacted", "post_compaction_snapshot"}
            for _, event in compaction_events
        ):
            failures.append("compaction evidence contains an unsupported lineage event")
        if any(
            event.get("compaction_id") != expected_compaction_id
            for _, event in compaction_events
        ):
            failures.append("compaction identity changed across lineage evidence")
        compacted_turns = [
            turn_index
            for turn_index, event in compaction_events
            if event.get("event") == "compacted"
            and event.get("compaction_id") == expected_compaction_id
        ]
        if not compacted_turns:
            failures.append("compaction retention requires a compacted lineage event")
        final_snapshot = turns[-1].get("compaction")
        if not isinstance(final_snapshot, dict):
            failures.append("compaction retention requires evidence on the final turn")
        else:
            final_turn_index = int(turns[-1].get("turn_index") or 0)
            if (
                final_snapshot.get("event") != "post_compaction_snapshot"
                or final_snapshot.get("compaction_id") != expected_compaction_id
            ):
                failures.append(
                    "compaction final evidence must be a bound post-compaction snapshot"
                )
            if compacted_turns and max(compacted_turns) >= final_turn_index:
                failures.append("compaction snapshot did not follow its compacted event")
            required_facts = {str(item) for item in compaction.get("required_facts") or []}
            raw_retained = final_snapshot.get("retained_facts")
            raw_dropped = final_snapshot.get("dropped_required_facts")
            if not isinstance(raw_retained, list) or any(
                not isinstance(item, str) for item in raw_retained
            ):
                failures.append("compaction retained_facts evidence must be a string list")
                retained_facts: set[str] = set()
            else:
                retained_facts = set(raw_retained)
            if not isinstance(raw_dropped, list) or any(
                not isinstance(item, str) for item in raw_dropped
            ):
                failures.append("compaction dropped_required_facts evidence must be a string list")
                dropped_facts: set[str] = set()
            else:
                dropped_facts = set(raw_dropped)
            missing_facts = required_facts - retained_facts
            reported_dropped = required_facts & dropped_facts
            if dropped_facts - required_facts:
                failures.append("compaction reported non-required facts as required drops")
            if missing_facts != reported_dropped:
                failures.append("compaction retained and dropped fact evidence is inconsistent")
            if len(missing_facts) > int(compaction.get("max_dropped_required_facts") or 0):
                failures.append("compaction reports too many dropped required facts")

    security = (
        stateful.get("security") if isinstance(stateful.get("security"), dict) else None
    )
    if security is not None:
        observed_security = replay.get("security")
        if not isinstance(observed_security, dict):
            failures.append("security expectations require security evidence")
        else:
            for key in ("untrusted_instructions_ignored", "foreign_tenant_access"):
                if key in security and observed_security.get(key) is not security[key]:
                    failures.append(f"security evidence mismatch for {key}")
            expected_tenant = str(security.get("tenant_id") or "")
            if expected_tenant:
                observed_tenants = observed_security.get("observed_tenant_ids")
                if (
                    not isinstance(observed_tenants, list)
                    or not observed_tenants
                    or any(str(item) != expected_tenant for item in observed_tenants)
                ):
                    failures.append("tenant isolation observed a foreign or missing tenant")
                if any(
                    not isinstance(turn.get("tenant_id"), str)
                    or turn.get("tenant_id") != expected_tenant
                    for turn in turns
                ):
                    failures.append("stateful turn has a foreign or missing tenant")
            forbidden_tools = {str(item) for item in security.get("forbidden_tools") or []}
            if forbidden_tools and any(
                not isinstance(turn.get("tool_calls"), list) for turn in turns
            ):
                failures.append(
                    "security forbidden_tools requires explicit tool_calls evidence on every turn"
                )
            called_tools = {
                _tool_call_name(call) for _turn_index, call in calls if isinstance(call, dict)
            }
            unexpected = sorted(forbidden_tools & called_tools)
            if unexpected:
                failures.append(f"security forbidden tools called: {', '.join(unexpected)}")
    return failures


def evaluate_case(
    case: dict[str, Any],
    observation: dict[str, Any] | None = None,
) -> dict[str, Any]:
    validation_errors = validate_case(case)
    if validation_errors:
        return _failed_case_result(
            case, f"invalid behavior contract: {'; '.join(validation_errors)}"
        )
    replay = observation if isinstance(observation, dict) else _case_replay(case)
    if not replay:
        return _failed_case_result(case, "missing replay observation")
    output = str(replay.get("output_preview") or "")
    spans = replay.get("span_kinds") if isinstance(replay.get("span_kinds"), list) else []
    status = str(replay.get("status") or "")
    expected_output = (
        case.get("expected_output") if isinstance(case.get("expected_output"), dict) else {}
    )
    expected_trajectory = (
        case.get("expected_trajectory") if isinstance(case.get("expected_trajectory"), dict) else {}
    )
    metadata = case.get("metadata") if isinstance(case.get("metadata"), dict) else {}
    required_texts = _expected_strings(expected_output.get("contains"))
    if not required_texts:
        required_texts = _expected_strings(expected_output.get("output_preview"))
    forbidden_texts = _expected_strings(expected_output.get("not_contains"))
    required_spans = [
        str(item)
        for item in expected_trajectory.get("required_span_kinds", [])
        if isinstance(item, str) and item
    ]

    assertions = case.get("assertions") if isinstance(case.get("assertions"), list) else []
    failures = _evaluate_assertions(assertions, replay)
    adapter_evidence = replay.get("adapter_evidence")
    if isinstance(adapter_evidence, dict) and adapter_evidence.get(
        "source"
    ) == "canonical_assistant_producer":
        integrity = adapter_evidence.get("integrity")
        integrity_status = (
            integrity.get("status") if isinstance(integrity, dict) else "unknown"
        )
        if integrity_status != "verified":
            failures.append(
                f"canonical producer adapter integrity is {integrity_status or 'unknown'}"
            )
    runtime_failures = _evaluate_runtime_expectations(case, replay)
    stateful_failures = _evaluate_stateful_expectations(case, replay)
    tool_failures = _evaluate_tool_expectations(expected_trajectory.get("tools") or [], replay)
    failures.extend(runtime_failures)
    failures.extend(stateful_failures)
    failures.extend(tool_failures)
    if not status:
        failures.append("missing replay status")
    elif status != "succeeded":
        failures.append(f"status={status}")
    for required_text in required_texts:
        if required_text.lower() not in output.lower():
            failures.append(f"missing expected text {required_text!r}")
    for forbidden_text in forbidden_texts:
        if forbidden_text.lower() in output.lower():
            failures.append(f"found forbidden text {forbidden_text!r}")
    missing_spans = [span for span in required_spans if span not in spans]
    if missing_spans:
        failures.append(f"missing spans {','.join(missing_spans)}")
    if _contains_sensitive(replay):
        failures.append("sensitive replay payload")

    trajectory_assertions = [
        assertion
        for assertion in assertions
        if isinstance(assertion, dict)
        and assertion.get("type") in {"required_span_kind", "tool_called", "tool_not_called"}
    ]
    trajectory_pass = (
        not missing_spans
        and not runtime_failures
        and not stateful_failures
        and not tool_failures
        and not _evaluate_assertions(trajectory_assertions, replay)
    )
    score = 1.0 if not failures else 0.0
    return {
        "case_id": case.get("case_id"),
        "score": round(score, 4),
        "passed": not failures,
        "critical": metadata.get("critical") is True,
        "trajectory_pass": trajectory_pass,
        "stateful_pass": not stateful_failures if expected_trajectory.get("stateful") else None,
        "failures": failures,
    }


def evaluate_cases(
    cases: list[dict[str, Any]],
    observations: dict[str, dict[str, Any]] | None = None,
) -> dict[str, Any]:
    case_results = [
        evaluate_case(case, observations.get(str(case.get("case_id")), {}))
        if observations is not None
        else evaluate_case(case)
        for case in cases
    ]
    total = len(case_results)
    if total == 0:
        return {
            "schema_version": GATE_METRICS_SCHEMA_VERSION,
            "case_count": 0,
            "score_sum": 0.0,
            "failed_case_count": 0,
            "overall_score": 0.0,
            "pass_rate": 0.0,
            "trajectory_case_count": 0,
            "trajectory_failed_count": 0,
            "trajectory_pass_rate": 0.0,
            "critical_case_count": 0,
            "critical_failed_count": 0,
            "critical_pass_rate": None,
            "stateful_case_count": 0,
            "stateful_failed_count": 0,
            "stateful_pass_rate": None,
            "cases": [],
        }
    critical_cases = [case for case in case_results if case["critical"]]
    stateful_cases = [case for case in case_results if case.get("stateful_pass") is not None]
    score_sum = sum(case["score"] for case in case_results)
    trajectory_failed_count = sum(1 for case in case_results if not case["trajectory_pass"])
    return {
        "schema_version": GATE_METRICS_SCHEMA_VERSION,
        "case_count": total,
        "score_sum": score_sum,
        "failed_case_count": sum(1 for case in case_results if not case["passed"]),
        "overall_score": score_sum / total,
        "pass_rate": sum(1 for case in case_results if case["passed"]) / total,
        "trajectory_case_count": total,
        "trajectory_failed_count": trajectory_failed_count,
        "trajectory_pass_rate": (total - trajectory_failed_count) / total,
        "critical_case_count": len(critical_cases),
        "critical_failed_count": sum(1 for case in critical_cases if not case["passed"]),
        "critical_pass_rate": (
            sum(1 for case in critical_cases if case["passed"]) / len(critical_cases)
            if critical_cases
            else None
        ),
        "stateful_case_count": len(stateful_cases),
        "stateful_failed_count": sum(
            1 for case in stateful_cases if not case["stateful_pass"]
        ),
        "stateful_pass_rate": (
            sum(1 for case in stateful_cases if case["stateful_pass"])
            / len(stateful_cases)
            if stateful_cases
            else None
        ),
        "cases": case_results,
    }
