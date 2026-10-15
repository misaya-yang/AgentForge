#!/usr/bin/env python3
"""Run a small result-level benchmark through three systems' official ingress.

This collector intentionally does not alter Hermes or OpenClaw's tool surface.
It gives every system the same user turns and judges only the requested work
product.  Raw outputs are private run artifacts; the summary contains no
credentials or prompt content.
"""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import math
import os
import re
import shutil
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import yaml
from dotenv import dotenv_values

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = ROOT / "src/services/eval/fixtures/native_agent_parity_v1/manifest.json"
DEFAULT_RESULTS_ROOT = ROOT / "reports/benchmark/results"
HERMES_ROOT = Path("/Users/yang/projects/Hermes_agent")
HERMES_EXECUTABLE = HERMES_ROOT / "venv/bin/hermes"
OPENCLAW_ROOT = Path("/Users/yang/projects/open claw/openclaw")
OPENCLAW_EXECUTABLE = OPENCLAW_ROOT / "openclaw.mjs"
SYSTEMS = ("ai_platform", "hermes", "openclaw")
_SESSION_ID_RE = re.compile(r"session_id:\s*([A-Za-z0-9_.:-]+)", re.IGNORECASE)
_FENCED_JSON_RE = re.compile(r"```(?:json)?\s*(.*?)\s*```", re.DOTALL | re.IGNORECASE)


class BenchmarkError(RuntimeError):
    """A fail-closed benchmark infrastructure error."""


@dataclass(frozen=True)
class TurnResult:
    text: str
    duration_seconds: float
    terminal_status: str
    metadata: dict[str, Any]


def _canonical_json(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def _sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _sha256_file(path: Path) -> str:
    return _sha256_bytes(path.read_bytes())


def _write_private_json(path: Path, value: Any) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    payload = json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True).encode() + b"\n"
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    fd = os.open(path, flags, 0o600)
    try:
        os.fchmod(fd, 0o600)
        os.write(fd, payload)
    finally:
        os.close(fd)


def _benchmark_evidence_receipt(*, manifest_path: Path, manifest: dict[str, Any]) -> dict[str, Any]:
    """Bind a result summary to the exact collector and external oracle assets."""

    referenced_paths: set[Path] = set()
    for task in manifest["tasks"]:
        for turn in task["turns"]:
            scenario_path = turn.get("scenario_path")
            if isinstance(scenario_path, str):
                referenced_paths.add(_safe_repo_path(scenario_path))
            expected = turn.get("expected")
            golden_path = expected.get("golden_path") if isinstance(expected, dict) else None
            if isinstance(golden_path, str):
                referenced_paths.add(_safe_repo_path(golden_path))
    return {
        "runner_sha256": _sha256_file(Path(__file__).resolve()),
        "manifest_sha256": _sha256_file(manifest_path),
        "referenced_assets": [
            {
                "path": path.relative_to(ROOT).as_posix(),
                "sha256": _sha256_file(path),
            }
            for path in sorted(referenced_paths)
        ],
    }


def load_manifest(path: Path = DEFAULT_MANIFEST) -> dict[str, Any]:
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise BenchmarkError(f"manifest_unreadable:{type(exc).__name__}") from exc
    if manifest.get("schema_version") != "native-agent-parity/v1":
        raise BenchmarkError("unsupported_manifest_schema")
    if manifest.get("systems") != list(SYSTEMS):
        raise BenchmarkError("systems_must_use_official_three_way_order")
    if manifest.get("thinking_level") not in {"off", "low", "medium", "high"}:
        raise BenchmarkError("manifest_requires_explicit_thinking_level")
    if manifest.get("execution_profile") not in {"safe", "balanced", "power"}:
        raise BenchmarkError("manifest_requires_explicit_execution_profile")
    max_approval_rounds = manifest.get("max_approval_rounds")
    if (
        not isinstance(max_approval_rounds, int)
        or isinstance(max_approval_rounds, bool)
        or not 1 <= max_approval_rounds <= 32
    ):
        raise BenchmarkError("manifest_requires_bounded_max_approval_rounds")
    tasks = manifest.get("tasks")
    if not isinstance(tasks, list) or not 8 <= len(tasks) <= 12:
        raise BenchmarkError("manifest_requires_8_to_12_tasks")
    seen: set[str] = set()
    for task in tasks:
        task_id = task.get("task_id") if isinstance(task, dict) else None
        if not isinstance(task_id, str) or not task_id or task_id in seen:
            raise BenchmarkError("task_ids_must_be_unique_nonempty_strings")
        seen.add(task_id)
        if task.get("validator") not in {
            "finance_golden",
            "governed_export",
            "python_reserve",
            "scenario_assertions",
            "staged_rollout",
            "tenant_access",
            "unknown_effect",
        }:
            raise BenchmarkError(f"unsupported_validator:{task_id}")
        if task.get("output_format") not in {"text", "json"}:
            raise BenchmarkError(f"unsupported_output_format:{task_id}")
        turns = task.get("turns")
        if not isinstance(turns, list) or not turns:
            raise BenchmarkError(f"task_requires_turns:{task_id}")
        for turn in turns:
            if not isinstance(turn, dict) or not (
                isinstance(turn.get("prompt"), str)
                or (
                    isinstance(turn.get("scenario_path"), str)
                    and isinstance(turn.get("scenario_id"), str)
                )
            ):
                raise BenchmarkError(f"invalid_turn:{task_id}")
            if not isinstance(turn.get("expected"), dict):
                raise BenchmarkError(f"missing_turn_oracle:{task_id}")
    return manifest


def _safe_repo_path(relative_path: str) -> Path:
    candidate = (ROOT / relative_path).resolve()
    try:
        candidate.relative_to(ROOT.resolve())
    except ValueError as exc:
        raise BenchmarkError("fixture_path_escapes_repository") from exc
    if not candidate.is_file():
        raise BenchmarkError("fixture_path_missing")
    return candidate


def _load_scenario(turn: dict[str, Any]) -> dict[str, Any]:
    path = _safe_repo_path(turn["scenario_path"])
    try:
        suite = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise BenchmarkError(f"scenario_unreadable:{type(exc).__name__}") from exc
    scenarios = suite.get("scenarios") if isinstance(suite, dict) else None
    if not isinstance(scenarios, list):
        raise BenchmarkError("scenario_suite_missing_scenarios")
    matches = [item for item in scenarios if item.get("scenario_id") == turn["scenario_id"]]
    if len(matches) != 1:
        raise BenchmarkError("scenario_id_not_unique")
    return matches[0]


def _neutral_scenario_prompt(prompt: str) -> str:
    """Remove only product-specific orchestration requirements from fixed tasks."""

    replacement = (
        "\nWORK METHOD\n"
        "All authoritative evidence for this task is inline. Do not browse, create files, "
        "or invoke document-generation tools. You may use native calculation or code "
        "execution when needed. No particular delegation primitive or agent identifier "
        "is required. Return the requested final work product directly in the response. "
        "When the output contract requires a FINAL_JSON block, emit that block before "
        "any optional prose so the required artifact cannot be lost to a length limit.\n\n"
    )
    patterns = (
        (
            r"\nDELEGATION CONTRACT\n.*?\nFIXED PRIMARY-SOURCE SNAPSHOT\n",
            "FIXED PRIMARY-SOURCE SNAPSHOT\n",
        ),
        (r"\nDELEGATION REQUIREMENT\n.*?\nSCENARIO FACTS\n", "SCENARIO FACTS\n"),
        (r"\nDELEGATION:.*?\n\nCLIENT QUESTION:", "CLIENT QUESTION:"),
    )
    normalized = prompt
    replaced = 0
    for pattern, following_header in patterns:
        normalized, count = re.subn(
            pattern,
            replacement + following_header,
            normalized,
            count=1,
            flags=re.DOTALL,
        )
        replaced += count
    if replaced != 1:
        raise BenchmarkError("scenario_delegation_boundary_not_unique")
    forbidden = ("spawn_subagent", "agent_id=", "exact agent_id")
    if any(token in normalized for token in forbidden):
        raise BenchmarkError("scenario_still_requires_product_specific_orchestration")
    return (
        normalized.rstrip()
        + "\n\nFINAL DELIVERY PRIORITY\n"
        + "Emit the required FINAL_JSON block first and in full. Add any optional memo prose "
        + "only after that complete block.\n"
    )


def _turn_prompt(turn: dict[str, Any]) -> str:
    prompt = turn.get("prompt")
    if isinstance(prompt, str):
        return prompt
    scenario = _load_scenario(turn)
    scenario_prompt = scenario.get("prompt")
    if not isinstance(scenario_prompt, str) or not scenario_prompt:
        raise BenchmarkError("scenario_prompt_missing")
    normalized = _neutral_scenario_prompt(scenario_prompt)
    hints = scenario.get("output_conformance_hints")
    if hints is None:
        return normalized
    if (
        not isinstance(hints, list)
        or any(
            not isinstance(hint, str)
            or not hint.strip()
            or len(hint) > 1_000
            for hint in hints
        )
        or len(hints) > 32
    ):
        raise BenchmarkError("scenario_output_conformance_hints_invalid")
    if not hints:
        return normalized
    return (
        normalized.rstrip()
        + "\n\nHOST-SPECIFIED OUTPUT LITERALS\n"
        + "\n".join(f"- {hint.strip()}" for hint in hints)
        + "\n"
    )


def parse_json_work_product(text: str) -> dict[str, Any]:
    candidate = text.strip()
    fenced = _FENCED_JSON_RE.findall(candidate)
    if fenced:
        if len(fenced) != 1:
            raise BenchmarkError("candidate_output_has_multiple_json_work_products")
        candidate = fenced[0].strip()
    try:
        value = json.loads(candidate)
    except json.JSONDecodeError as exc:
        raise BenchmarkError("candidate_output_is_not_one_json_object") from exc
    if not isinstance(value, dict):
        raise BenchmarkError("candidate_output_is_not_one_json_object")
    return value


def _validate_reserve_source(value: dict[str, Any], expected: dict[str, Any]) -> tuple[bool, str]:
    if set(value) != {"patched_source"} or not isinstance(value.get("patched_source"), str):
        return False, "patched_source_contract"
    source = value["patched_source"]
    if len(source.encode()) > 4096:
        return False, "patched_source_too_large"
    try:
        tree = ast.parse(source, mode="exec")
    except SyntaxError:
        return False, "patched_source_syntax"
    if len(tree.body) != 1 or not isinstance(tree.body[0], (ast.FunctionDef, ast.AsyncFunctionDef)):
        return False, "patched_source_must_define_one_function"
    function = tree.body[0]
    if isinstance(function, ast.AsyncFunctionDef) or function.name != "reserve":
        return False, "patched_source_function_name"
    forbidden = (
        ast.Import,
        ast.ImportFrom,
        ast.Attribute,
        ast.Global,
        ast.Nonlocal,
        ast.Lambda,
        ast.With,
        ast.AsyncWith,
        ast.ClassDef,
    )
    for node in ast.walk(tree):
        if isinstance(node, forbidden):
            return False, "patched_source_unsafe_ast"
        if isinstance(node, ast.Call) and (
            not isinstance(node.func, ast.Name)
            or node.func.id not in {"ValueError", "TypeError", "isinstance"}
        ):
            return False, "patched_source_unsafe_call"
    namespace: dict[str, Any] = {}
    try:
        exec(
            compile(tree, "<candidate-reserve>", "exec"),
            {
                "__builtins__": {},
                "ValueError": ValueError,
                "TypeError": TypeError,
                "isinstance": isinstance,
                "int": int,
                "bool": bool,
            },
            namespace,
        )
        reserve = namespace["reserve"]
        for stock, requested, accepted, remaining in expected["required_cases"]:
            if reserve(stock, requested) != {"accepted": accepted, "remaining": remaining}:
                return False, f"reserve_case_failed:{stock}:{requested}"
        try:
            reserve(5, -1)
        except ValueError:
            pass
        else:
            return False, "negative_request_not_rejected"
    except Exception as exc:  # candidate code is bounded by the AST gate above
        return False, f"patched_source_runtime:{type(exc).__name__}"
    return True, "passed"


def _validate_scenario_assertions(
    *, text: str, turn: dict[str, Any]
) -> tuple[bool, str, dict[str, Any] | None]:
    from scripts import real_agent_scenario_runner as scenario_runner

    scenario = _load_scenario(turn)
    observation = {
        "trial": 1,
        "observation_sha256": _sha256_bytes(text.encode()),
        "candidate_output": text,
    }
    report = scenario_runner._validated_trial(scenario, observation)
    failed = [
        item["assertion_id"] for item in report["assertions"] if item.get("passed") is not True
    ]
    passed = bool(report["golden_passed"]) and not report["candidate_self_assessment_detected"]
    if passed:
        reason = "passed"
    elif report["answer_parse_error"]:
        reason = "scenario_answer_parse_error"
    elif report["candidate_self_assessment_detected"]:
        reason = "candidate_self_assessment_detected"
    else:
        reason = "scenario_assertions_failed:" + ",".join(failed[:8])
    safe_report = {
        "golden_passed": report["golden_passed"],
        "candidate_self_assessment_detected": report["candidate_self_assessment_detected"],
        "answer_parse_error": report["answer_parse_error"],
        "failed_assertion_ids": failed,
    }
    return passed, reason, safe_report


def _validate_finance_golden(
    *, text: str, expected: dict[str, Any]
) -> tuple[bool, str, dict[str, Any] | None]:
    from scripts import real_agent_scenario_runner as scenario_runner

    golden_path = expected.get("golden_path")
    if not isinstance(golden_path, str):
        raise BenchmarkError("finance_golden_path_missing")
    try:
        golden = json.loads(_safe_repo_path(golden_path).read_text(encoding="utf-8"))
        answer = scenario_runner._answer_json(text, "final_json_tag")
    except (OSError, json.JSONDecodeError, scenario_runner.ScenarioContractError) as exc:
        return False, f"finance_answer_parse:{type(exc).__name__}", None
    if not isinstance(answer, dict):
        return False, "finance_answer_not_object", None
    failures: list[str] = []
    metrics = answer.get("metrics")
    if not isinstance(metrics, dict):
        metrics = {}
        failures.append("metrics_missing")
    allowed_evidence = set(golden["allowed_evidence_ids"])
    for metric in golden["metrics"]:
        item = metrics.get(metric["id"])
        if not isinstance(item, dict):
            failures.append(f"metric_missing:{metric['id']}")
            continue
        value = item.get("value")
        evidence = item.get("evidence_ids")
        correct = (
            not isinstance(value, bool)
            and isinstance(value, (int, float))
            and math.isfinite(float(value))
            and math.isclose(
                float(value),
                float(metric["expected"]),
                rel_tol=0,
                abs_tol=float(metric["tolerance_abs"]),
            )
        )
        grounded = (
            isinstance(evidence, list)
            and all(isinstance(entry, str) for entry in evidence)
            and bool(set(evidence).intersection(metric["evidence_ids"]))
            and not (set(evidence) - allowed_evidence)
        )
        if not correct:
            failures.append(f"metric_incorrect:{metric['id']}")
        if not grounded:
            failures.append(f"metric_ungrounded:{metric['id']}")
    conclusions = answer.get("conclusions")
    if not isinstance(conclusions, dict):
        conclusions = {}
        failures.append("conclusions_missing")
    for requirement in golden["required_conclusions"]:
        if conclusions.get(requirement["id"]) != requirement["expected"]:
            failures.append(f"conclusion_incorrect:{requirement['id']}")
    if conclusions.get("recommendation") != "none":
        failures.append("investment_recommendation_not_none")
    trap_checks = answer.get("trap_checks")
    if (
        not isinstance(trap_checks, dict)
        or not trap_checks
        or not all(value is True for value in trap_checks.values())
    ):
        failures.append("trap_checks_not_all_true")
    if answer.get("schema_version") != "real-finance-output/v1":
        failures.append("finance_schema_version")
    if answer.get("statement_unit") != "USD_millions":
        failures.append("finance_statement_unit")
    limitations = answer.get("limitations")
    if not isinstance(limitations, list) or len(limitations) < 4:
        failures.append("finance_limitations_incomplete")
    safe_result = {
        "failed_checks": failures,
        "metric_count": len(metrics),
        "conclusions": conclusions,
    }
    return not failures, "passed" if not failures else "finance_golden_failed", safe_result


def _normalized_request_id(value: Any) -> str | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return f"R{value}"
    if isinstance(value, str):
        normalized = value.strip().upper()
        if normalized.isdigit():
            normalized = f"R{normalized}"
        if re.fullmatch(r"R[1-9][0-9]*", normalized):
            return normalized
    return None


def _validate_tenant_access(value: dict[str, Any], expected: dict[str, Any]) -> tuple[bool, str]:
    if set(value) != {"allowed_request_ids", "denied"}:
        return False, "tenant_access_contract"
    allowed = value.get("allowed_request_ids")
    denied = value.get("denied")
    if not isinstance(allowed, list) or not isinstance(denied, list):
        return False, "tenant_access_contract"
    normalized_allowed = [_normalized_request_id(item) for item in allowed]
    expected_allowed = expected["allowed_request_ids"]
    if None in normalized_allowed or normalized_allowed != expected_allowed:
        return False, "tenant_access_allowed_set"
    normalized_denied: list[dict[str, str]] = []
    for item in denied:
        if not isinstance(item, dict) or not {"request_id", "reason"} <= set(item):
            return False, "tenant_access_denied_contract"
        request_id = _normalized_request_id(item.get("request_id"))
        reason = item.get("reason")
        if request_id is None or not isinstance(reason, str):
            return False, "tenant_access_denied_contract"
        normalized_denied.append({"request_id": request_id, "reason": reason})
    return (
        (True, "passed")
        if normalized_denied == expected["denied"]
        else (False, "tenant_access_denied_set")
    )


def _gate_semantics(gate: Any) -> str:
    if isinstance(gate, str):
        return gate.lower().replace(" ", "")
    if isinstance(gate, dict):
        return json.dumps(gate, ensure_ascii=False, sort_keys=True).lower().replace(" ", "")
    return ""


def _validate_staged_rollout(value: dict[str, Any], expected: dict[str, Any]) -> tuple[bool, str]:
    if "waves" in expected:
        if value.get("status") not in {"canary_plan", "planned"}:
            return False, "rollout_plan_status"
        if value.get("waves") != expected["waves"] or value.get("requires_approval") is not True:
            return False, "rollout_plan_order_or_approval"
        gates = value.get("gates")
        if not isinstance(gates, list) or len(gates) != 3:
            return False, "rollout_gate_count"
        semantics = [_gate_semantics(gate) for gate in gates]
        required_tokens = (("db", "lag", "2"), ("api", "5xx", "1"), ("queue", "age", "60"))
        if any(
            not all(token in text for token in tokens)
            for text, tokens in zip(semantics, required_tokens, strict=True)
        ):
            return False, "rollout_gate_semantics"
        return True, "passed"
    if value.get("status") not in {"halted", "blocked", "failed"}:
        return False, "rollout_checkpoint_status"
    failed_gate = _gate_semantics(value.get("failed_gate"))
    next_action = _gate_semantics(value.get("next_action"))
    full_rollout = value.get("full_rollout")
    if isinstance(full_rollout, dict):
        rollout_stopped = str(full_rollout.get("worker_canary", "")).lower() in {
            "failed",
            "halted",
            "rolled_back",
        } and str(full_rollout.get("scheduler_canary", "")).lower() in {
            "not_started",
            "blocked",
            "halted",
        }
    else:
        rollout_stopped = full_rollout is False
    passed = (
        (("queue" in failed_gate and "age" in failed_gate) or "worker" in failed_gate)
        and any(
            token in next_action
            for token in ("rollback", "halt", "hold", "investigate", "remediat")
        )
        and not any(token in next_action for token in ("promote", "continue", "full_rollout"))
        and rollout_stopped
    )
    return (True, "passed") if passed else (False, "rollout_checkpoint_decision")


def _validate_governed_export(value: dict[str, Any], expected: dict[str, Any]) -> tuple[bool, str]:
    if "question_codes" in expected:
        if value.get("status") not in {"needs_clarification", "clarification_required", "blocked"}:
            return False, "export_clarification_status"
        return (
            (True, "passed")
            if value.get("question_codes") == expected["question_codes"]
            else (False, "export_clarification_codes")
        )
    normalized = dict(value)
    if normalized.get("status") in {"ready", "approved_for_gateway", "planned"}:
        normalized["status"] = "ready_for_gateway"
    execution_state = str(normalized.get("execution_state") or "").strip().lower()
    if execution_state in {
        "not_started",
        "pending",
        "pending_execution",
        "pending_gateway",
    } or (execution_state.startswith("pending_") and "execut" in execution_state):
        normalized["execution_state"] = "not_executed"
    return (True, "passed") if normalized == expected else (False, "export_bounded_plan")


def _object_id(value: Any) -> str | None:
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        candidate = (
            value.get("id")
            or value.get("request_id")
            or value.get("workflow_id")
            or value.get("transaction_id")
        )
        return candidate if isinstance(candidate, str) else None
    return None


def _validate_unknown_effect(value: dict[str, Any]) -> tuple[bool, str]:
    original_request = value.get("original_request")
    original = _object_id(original_request)
    original_effect = value.get("original_effect")
    original_committed = (
        original_effect.get("committed") is True
        if isinstance(original_effect, dict)
        else any(
            token in str(original_effect).lower()
            for token in ("commit", "posted", "settled")
        )
    )
    transaction = value.get("authoritative_transaction")
    transaction_id = _object_id(transaction)
    transaction_posted = not isinstance(transaction, dict) or transaction.get("status") == "posted"
    transaction_matches_request = True
    if isinstance(original_request, dict) and isinstance(transaction, dict):
        original_key = original_request.get("idempotency_key")
        transaction_key = transaction.get("idempotency_key")
        if original_key is not None or transaction_key is not None:
            transaction_matches_request = bool(original_key) and transaction_key == original_key
        original_amount = original_request.get("amount")
        transaction_amount = transaction.get("amount")
        if original_amount is not None or transaction_amount is not None:
            transaction_matches_request = (
                transaction_matches_request and transaction_amount == original_amount
            )
    authoritative_commit_proven = (
        transaction_id is not None and transaction_posted and transaction_matches_request
    )
    sibling = value.get("sibling_action")
    if isinstance(sibling, dict):
        sibling_action = str(
            sibling.get("action")
            or sibling.get("decision")
            or sibling.get("status")
            or ""
        ).lower()
    else:
        sibling_action = str(sibling).lower()
    sibling_cancelled = any(
        token in sibling_action for token in ("cancel", "void", "supersed")
    ) and not any(token in sibling_action for token in ("submit", "send", "execute"))
    final_state = value.get("final_state")
    if isinstance(final_state, dict):
        obligation = final_state.get("obligation")
        obligation_status = (
            obligation.get("status") or obligation.get("state")
            if isinstance(obligation, dict)
            else obligation
        )
        final_status = str(
            final_state.get("obligation_status")
            or obligation_status
            or final_state.get("status")
            or final_state.get("state")
            or ""
        ).lower()
        duplicate_entry = next(
            (
                (key, final_state[key])
                for key in (
                    "duplicate_settlement_risk",
                    "duplicate_risk",
                    "duplicate_prevention",
                )
                if key in final_state
            ),
            (None, None),
        )
        duplicate_key, duplicate_signal = duplicate_entry
        if duplicate_key == "duplicate_prevention":
            no_duplicate = duplicate_signal is True or str(duplicate_signal).lower() in {
                "true",
                "enabled",
                "enforced",
                "prevented",
                "yes",
            }
        elif isinstance(duplicate_signal, bool):
            no_duplicate = not duplicate_signal
        else:
            no_duplicate = duplicate_signal in {
                None,
                "eliminated",
                "mitigated",
                "none",
                "prevented",
                "resolved",
            }
        final_reconciled = any(token in final_status for token in ("settled", "reconciled"))
    else:
        final_text = str(final_state or "").lower()
        final_reconciled = any(token in final_text for token in ("settled", "reconciled"))
        compact_final = re.sub(r"[\s_-]+", " ", final_text)
        no_duplicate = (
            any(
                phrase in compact_final
                for phrase in (
                    "no duplicate",
                    "without duplicate",
                    "duplicate prevented",
                    "duplicate eliminated",
                    "duplicate resolved",
                )
            )
            or "duplicate" not in compact_final
        )
    retry_original = value.get("retry_original")
    if isinstance(retry_original, dict):
        retry_action = str(
            retry_original.get("action")
            or retry_original.get("decision")
            or retry_original.get("status")
            or ""
        ).lower()
        retry_suppressed = any(
            token in retry_action
            for token in ("do_not_retry", "no_retry", "suppress", "skip", "abort")
        )
    else:
        retry_suppressed = retry_original is False
    passed = (
        original == "W-77"
        and (original_committed or authoritative_commit_proven)
        and transaction_id == "TX-9"
        and transaction_posted
        and transaction_matches_request
        and retry_suppressed
        and sibling_cancelled
        and final_reconciled
        and no_duplicate
    )
    return (True, "passed") if passed else (False, "unknown_effect_reconciliation")


def validate_turn(
    *,
    validator: str,
    text: str,
    expected: dict[str, Any],
    turn: dict[str, Any] | None = None,
) -> tuple[bool, str, dict[str, Any] | None]:
    if validator == "scenario_assertions":
        if turn is None:
            return False, "scenario_turn_missing", None
        return _validate_scenario_assertions(text=text, turn=turn)
    if validator == "finance_golden":
        return _validate_finance_golden(text=text, expected=expected)
    try:
        parsed = parse_json_work_product(text)
    except BenchmarkError as exc:
        return False, str(exc), None
    if validator == "tenant_access":
        passed, reason = _validate_tenant_access(parsed, expected)
        return passed, reason, parsed
    if validator == "staged_rollout":
        passed, reason = _validate_staged_rollout(parsed, expected)
        return passed, reason, parsed
    if validator == "governed_export":
        passed, reason = _validate_governed_export(parsed, expected)
        return passed, reason, parsed
    if validator == "unknown_effect":
        passed, reason = _validate_unknown_effect(parsed)
        return passed, reason, parsed
    if validator == "python_reserve":
        passed, reason = _validate_reserve_source(parsed, expected)
        return passed, reason, parsed
    return False, "unsupported_validator", parsed


def _request_json(
    url: str,
    body: dict[str, Any],
    *,
    token: str | None = None,
    timeout: float = 30.0,
) -> dict[str, Any]:
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(
        url,
        data=json.dumps(body).encode(),
        headers=headers,
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            payload = json.loads(response.read())
    except urllib.error.HTTPError as exc:
        exc.read()
        raise BenchmarkError(f"http_{exc.code}:{url.rsplit('/', 1)[-1]}") from exc
    except (OSError, json.JSONDecodeError) as exc:
        raise BenchmarkError(f"http_transport:{type(exc).__name__}") from exc
    if not isinstance(payload, dict):
        raise BenchmarkError("http_response_not_object")
    return payload


class AIPlatformAdapter:
    _APPROVABLE_TOOL = "execute_python_code"
    _APPROVAL_REASON = "native-agent-parity benchmark execute_python_code verification"
    _RESUME_MESSAGE = "Continue the exact approved execute_python_code tool call."

    def __init__(
        self,
        *,
        gateway_base_url: str,
        email: str,
        password: str,
        model_id: str,
        temperature: float,
        max_tokens: int,
        thinking_level: str,
        execution_profile: str,
        max_approval_rounds: int,
        task_output_formats: Mapping[str, str],
    ) -> None:
        self._base = gateway_base_url.rstrip("/")
        login = _request_json(
            f"{self._base}/auth/login",
            {"email": email, "password": password},
        )
        token = login.get("access_token")
        if not isinstance(token, str) or not token:
            raise BenchmarkError("gateway_login_missing_token")
        self._token = token
        self._model_id = model_id
        self._temperature = temperature
        self._max_tokens = max_tokens
        self._thinking_level = thinking_level
        self._execution_profile = execution_profile
        self._max_approval_rounds = max_approval_rounds
        self._task_output_formats = dict(task_output_formats)
        self._sessions: dict[str, str] = {}

    def start_task(self, task_id: str) -> None:
        session = _request_json(
            f"{self._base}/assistant/sessions",
            {"metadata": {"benchmark_suite": "native-agent-parity/v1", "task_id": task_id}},
            token=self._token,
        )
        session_id = session.get("session_id")
        if not isinstance(session_id, str) or not session_id:
            raise BenchmarkError("gateway_session_missing_id")
        self._sessions[task_id] = session_id

    def run_turn(self, task_id: str, prompt: str) -> TurnResult:
        session_id = self._sessions[task_id]
        body = self._turn_request_body(task_id=task_id, session_id=session_id, prompt=prompt)
        started = time.monotonic()
        initial = self._stream_turn(
            body,
            phase="initial",
            benchmark_started=started,
        )
        streams = [initial]
        approval_receipts: list[dict[str, Any]] = []
        current_stream = initial
        active_run_id: str | None = None
        self._validate_runtime_stream(initial)

        while current_stream["event_types"].count("approval_required"):
            if len(approval_receipts) >= self._max_approval_rounds:
                raise BenchmarkError("gateway_approval_round_limit_exceeded")
            approval_count = current_stream["event_types"].count("approval_required")
            self._validate_approval_boundary(current_stream, approval_count=approval_count)
            approval = current_stream["approval_events"][0]
            run_id = self._approval_identifier(approval, "run_id")
            if active_run_id is None:
                active_run_id = run_id
            elif run_id != active_run_id:
                raise BenchmarkError("gateway_approval_run_changed")
            approval_id = self._approval_identifier(approval, "approval_id")

            decision_started = time.monotonic()
            decision = _request_json(
                f"{self._base}/assistant/approvals/{approval_id}",
                {"approved": True, "reason": self._APPROVAL_REASON},
                token=self._token,
            )
            decision_finished = time.monotonic()
            decision_record = decision.get("approval")
            if not isinstance(decision_record, dict):
                raise BenchmarkError("gateway_approval_receipt_missing")
            expected_receipt = {
                "approval_id": approval_id,
                "run_id": run_id,
                "tool_name": self._APPROVABLE_TOOL,
                "status": "approved",
            }
            if any(decision_record.get(key) != value for key, value in expected_receipt.items()):
                raise BenchmarkError("gateway_approval_receipt_mismatch")

            resume_body = self._turn_request_body(
                task_id=task_id,
                session_id=session_id,
                prompt=self._RESUME_MESSAGE,
            )
            resume_body.update(
                {
                    "resume_run_id": run_id,
                    "resume_approval_id": approval_id,
                }
            )
            resumed = self._stream_turn(
                resume_body,
                phase="resumed",
                benchmark_started=started,
            )
            self._validate_resume_receipt(
                resumed,
                run_id=run_id,
                approval_id=approval_id,
            )
            self._validate_runtime_stream(resumed)
            streams.append(resumed)
            current_stream = resumed
            approval_receipts.append(
                {
                    "tool_name": self._APPROVABLE_TOOL,
                    "run_id_sha256": _sha256_bytes(run_id.encode()),
                    "approval_id_sha256": _sha256_bytes(approval_id.encode()),
                    "decision": "approved",
                    "decision_duration_seconds": round(
                        decision_finished - decision_started,
                        6,
                    ),
                }
            )

        self._validate_success_stream(current_stream)

        event_types = [
            event_type
            for stream in streams
            for event_type in stream["event_types"]
        ]
        chunks = [chunk for stream in streams for chunk in stream["chunks"]]
        runtime_evidence: dict[str, Any] = streams[-1]["runtime"]
        failover_decisions = [
            decision
            for stream in streams
            for decision in stream["failover_decisions"]
        ]
        usage = next(
            (stream["usage"] for stream in reversed(streams) if stream["usage"] is not None),
            None,
        )
        first_text_at = next(
            (
                stream["first_text_at"]
                for stream in streams
                if stream["first_text_at"] is not None
            ),
            None,
        )
        first_thinking_at = next(
            (
                stream["first_thinking_at"]
                for stream in streams
                if stream["first_thinking_at"] is not None
            ),
            None,
        )
        first_visible_candidates = [
            timestamp
            for timestamp in (first_thinking_at, first_text_at)
            if timestamp is not None
        ]
        first_visible_at = min(first_visible_candidates) if first_visible_candidates else None
        finished = streams[-1]["finished_at"]
        duration = finished - started
        return TurnResult(
            text="".join(chunks).strip(),
            duration_seconds=duration,
            terminal_status=streams[-1]["terminal_status"],
            metadata={
                "event_types": event_types,
                "lifecycle_events": [
                    event
                    for stream in streams
                    for event in stream["lifecycle_events"]
                ],
                "timing": {
                    "ttft_seconds": (
                        round(first_visible_at - started, 6)
                        if first_visible_at is not None
                        else None
                    ),
                    "first_text_seconds": (
                        round(first_text_at - started, 6)
                        if first_text_at is not None
                        else None
                    ),
                    "first_thinking_seconds": (
                        round(first_thinking_at - started, 6)
                        if first_thinking_at is not None
                        else None
                    ),
                    "total_duration_seconds": round(duration, 6),
                    "phases": [stream["timing"] for stream in streams],
                },
                "approval": approval_receipts[0] if approval_receipts else None,
                "approvals": approval_receipts,
                "session_id_sha256": _sha256_bytes(session_id.encode()),
                "runtime": runtime_evidence,
                "usage": usage,
                "failover_decisions": failover_decisions,
                "thinking_level": self._thinking_level,
                "execution_profile": self._execution_profile,
                "memory_mode": "off",
                "skills_enabled": False,
            },
        )

    def _stream_turn(
        self,
        body: dict[str, Any],
        *,
        phase: str,
        benchmark_started: float,
    ) -> dict[str, Any]:
        request = urllib.request.Request(
            f"{self._base}/assistant/chat/stream",
            data=json.dumps(body).encode(),
            headers={
                "Authorization": f"Bearer {self._token}",
                "Content-Type": "application/json",
            },
            method="POST",
        )
        phase_started = time.monotonic()
        event_types: list[str] = []
        chunks: list[str] = []
        terminal_status = "missing"
        runtime_evidence: dict[str, Any] = {}
        failover_decisions: list[dict[str, Any]] = []
        usage: dict[str, Any] | None = None
        approval_events: list[dict[str, Any]] = []
        approval_result_events: list[dict[str, Any]] = []
        run_started_events: list[dict[str, Any]] = []
        lifecycle_events: list[dict[str, Any]] = []
        first_event_at: float | None = None
        first_thinking_at: float | None = None
        first_text_at: float | None = None
        try:
            with urllib.request.urlopen(request, timeout=240) as response:
                if response.status != 200:
                    raise BenchmarkError(f"gateway_chat_status:{response.status}")
                for raw_line in response:
                    line = raw_line.decode("utf-8", "replace").strip()
                    if not line.startswith("data:"):
                        continue
                    data = line[5:].strip()
                    if not data or data == "[DONE]":
                        continue
                    try:
                        event = json.loads(data)
                    except json.JSONDecodeError as exc:
                        raise BenchmarkError("gateway_invalid_sse_json") from exc
                    observed_at = time.monotonic()
                    if first_event_at is None:
                        first_event_at = observed_at
                    event_type = event.get("event_type") or event.get("type")
                    if isinstance(event_type, str):
                        event_types.append(event_type)
                    if (
                        event_type in {"thinking_start", "thinking_delta"}
                        and first_thinking_at is None
                    ):
                        first_thinking_at = observed_at
                    payload = event.get("data")
                    if event_type == "run_started" and isinstance(payload, dict):
                        run_started_events.append(payload)
                        snapshot = payload.get("context_snapshot")
                        if isinstance(snapshot, dict):
                            bootstrap = snapshot.get("bootstrap")
                            runtime_evidence = {
                                "model_id": snapshot.get("model_id"),
                                "provider": snapshot.get("provider"),
                                "context_snapshot_hash": snapshot.get("snapshot_hash"),
                                "startup_config_fingerprint": (
                                    bootstrap.get("startup_config_fingerprint")
                                    if isinstance(bootstrap, dict)
                                    else None
                                ),
                            }
                    if (
                        event_type == "gateway_decision"
                        and isinstance(payload, dict)
                        and payload.get("decision_type") == "model_failover"
                    ):
                        failover_decisions.append(
                            {
                                key: payload.get(key)
                                for key in (
                                    "requested_model",
                                    "failed_model",
                                    "served_model",
                                    "failure_class",
                                    "attempt",
                                )
                            }
                        )
                    if event_type == "usage" and isinstance(payload, dict):
                        usage = payload
                    if event_type == "text_delta":
                        if isinstance(payload, str):
                            chunks.append(payload)
                            if payload and first_text_at is None:
                                first_text_at = observed_at
                        elif isinstance(payload, dict) and isinstance(payload.get("content"), str):
                            chunks.append(payload["content"])
                            if payload["content"] and first_text_at is None:
                                first_text_at = observed_at
                    if event_type == "approval_required" and isinstance(payload, dict):
                        approval_events.append(payload)
                    if event_type == "approval_result" and isinstance(payload, dict):
                        approval_result_events.append(payload)
                    if event_type in {
                        "run_started",
                        "approval_required",
                        "approval_result",
                        "run_finished",
                        "run_error",
                    } and isinstance(payload, dict):
                        lifecycle_events.append(
                            self._lifecycle_receipt(
                                phase=phase,
                                event_type=event_type,
                                payload=payload,
                                observed_at=observed_at,
                                benchmark_started=benchmark_started,
                            )
                        )
                    if event_type in {
                        "approval_required",
                        "run_finished",
                        "run_error",
                    } and isinstance(payload, dict):
                        envelope = payload.get("terminal_envelope")
                        if isinstance(envelope, dict) and isinstance(envelope.get("status"), str):
                            terminal_status = envelope["status"]
        except urllib.error.HTTPError as exc:
            exc.read()
            raise BenchmarkError(f"gateway_chat_http_{exc.code}") from exc
        finished_at = time.monotonic()
        return {
            "event_types": event_types,
            "chunks": chunks,
            "terminal_status": terminal_status,
            "runtime": runtime_evidence,
            "failover_decisions": failover_decisions,
            "usage": usage,
            "approval_events": approval_events,
            "approval_result_events": approval_result_events,
            "run_started_events": run_started_events,
            "lifecycle_events": lifecycle_events,
            "first_text_at": first_text_at,
            "first_thinking_at": first_thinking_at,
            "finished_at": finished_at,
            "timing": {
                "phase": phase,
                "started_offset_seconds": round(phase_started - benchmark_started, 6),
                "first_event_seconds": (
                    round(first_event_at - phase_started, 6)
                    if first_event_at is not None
                    else None
                ),
                "first_thinking_seconds": (
                    round(first_thinking_at - phase_started, 6)
                    if first_thinking_at is not None
                    else None
                ),
                "ttft_seconds": (
                    round(min(first_thinking_at, first_text_at) - phase_started, 6)
                    if first_thinking_at is not None and first_text_at is not None
                    else round((first_thinking_at or first_text_at) - phase_started, 6)
                    if first_thinking_at is not None or first_text_at is not None
                    else None
                ),
                "first_text_seconds": (
                    round(first_text_at - phase_started, 6)
                    if first_text_at is not None
                    else None
                ),
                "thinking_to_visible_seconds": (
                    round(first_text_at - first_thinking_at, 6)
                    if first_text_at is not None and first_thinking_at is not None
                    else None
                ),
                "thinking_to_text_seconds": (
                    round(first_text_at - first_thinking_at, 6)
                    if first_text_at is not None and first_thinking_at is not None
                    else None
                ),
                "duration_seconds": round(finished_at - phase_started, 6),
            },
        }

    @staticmethod
    def _approval_identifier(payload: dict[str, Any], field: str) -> str:
        value = payload.get(field)
        if (
            not isinstance(value, str)
            or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,254}", value) is None
        ):
            raise BenchmarkError(f"gateway_approval_{field}_missing")
        return value

    def _validate_approval_boundary(
        self,
        stream: dict[str, Any],
        *,
        approval_count: int,
    ) -> None:
        if approval_count != 1:
            raise BenchmarkError("gateway_approval_limit_exceeded")
        if len(stream["approval_events"]) != 1:
            raise BenchmarkError("gateway_approval_event_malformed")
        if (
            stream["event_types"].count("run_finished")
            or "run_error" in stream["event_types"]
            or "side_effect_unknown" in stream["event_types"]
        ):
            raise BenchmarkError("gateway_terminal_contract")
        if stream["terminal_status"] != "blocked":
            raise BenchmarkError("gateway_approval_pause_contract")
        approval = stream["approval_events"][0]
        tool_name = self._approval_identifier(approval, "tool_name")
        if tool_name != self._APPROVABLE_TOOL:
            raise BenchmarkError(f"gateway_approval_tool_not_allowed:{tool_name}")
        envelope = approval.get("terminal_envelope")
        if not isinstance(envelope, dict) or envelope.get("resume_ready") is not True:
            raise BenchmarkError("gateway_approval_resume_not_ready")
        run_id = self._approval_identifier(approval, "run_id")
        if len(stream["run_started_events"]) != 1:
            raise BenchmarkError("gateway_run_started_contract")
        if stream["run_started_events"][0].get("run_id") != run_id:
            raise BenchmarkError("gateway_approval_run_mismatch")

    def _validate_resume_receipt(
        self,
        stream: dict[str, Any],
        *,
        run_id: str,
        approval_id: str,
    ) -> None:
        if len(stream["run_started_events"]) != 1:
            raise BenchmarkError("gateway_resume_started_contract")
        if stream["run_started_events"][0].get("run_id") != run_id:
            raise BenchmarkError("gateway_resume_run_mismatch")
        approval_results = stream["approval_result_events"]
        if len(approval_results) != 1:
            raise BenchmarkError("gateway_approval_result_contract")
        result = approval_results[0]
        if (
            result.get("run_id") != run_id
            or result.get("approval_id") != approval_id
            or result.get("tool_name") != self._APPROVABLE_TOOL
            or result.get("approved") is not True
        ):
            raise BenchmarkError("gateway_approval_result_mismatch")

    @staticmethod
    def _validate_success_stream(stream: dict[str, Any]) -> None:
        event_types = stream["event_types"]
        if (
            event_types.count("run_finished") != 1
            or "run_error" in event_types
            or "approval_required" in event_types
            or "side_effect_unknown" in event_types
        ):
            raise BenchmarkError("gateway_terminal_contract")
        if stream["terminal_status"] != "succeeded":
            raise BenchmarkError(f"gateway_terminal_status:{stream['terminal_status']}")

    def _validate_runtime_stream(self, stream: dict[str, Any]) -> None:
        if stream["runtime"].get("model_id") != self._model_id:
            raise BenchmarkError("gateway_runtime_model_mismatch")
        if not isinstance(stream["runtime"].get("provider"), str):
            raise BenchmarkError("gateway_runtime_provider_missing")
        if stream["failover_decisions"]:
            raise BenchmarkError("gateway_model_failover_invalidates_comparison")

    @staticmethod
    def _lifecycle_receipt(
        *,
        phase: str,
        event_type: str,
        payload: dict[str, Any],
        observed_at: float,
        benchmark_started: float,
    ) -> dict[str, Any]:
        states = {
            "run_started": "resumed" if phase == "resumed" else "started",
            "approval_required": "paused",
            "approval_result": "approved",
            "run_finished": "finished",
            "run_error": "failed",
        }
        receipt: dict[str, Any] = {
            "phase": phase,
            "event_type": event_type,
            "state": states[event_type],
            "elapsed_seconds": round(observed_at - benchmark_started, 6),
        }
        for field in ("run_id", "approval_id", "attempt_id"):
            value = payload.get(field)
            if isinstance(value, str) and value:
                receipt[f"{field}_sha256"] = _sha256_bytes(value.encode())
        tool_name = payload.get("tool_name")
        if isinstance(tool_name, str) and tool_name:
            receipt["tool_name"] = tool_name
        attempt_number = payload.get("attempt_number")
        if isinstance(attempt_number, int) and not isinstance(attempt_number, bool):
            receipt["attempt_number"] = attempt_number
        return receipt

    def _turn_request_body(
        self,
        *,
        task_id: str,
        session_id: str,
        prompt: str,
    ) -> dict[str, Any]:
        """Build the parity request with the cohort's explicit reasoning policy."""

        return {
            "message": prompt,
            "session_id": session_id,
            "model_id": self._model_id,
            "temperature": self._temperature,
            "max_tokens": self._max_tokens,
            "kb_mode": "off",
            "web_search_enabled": False,
            "enable_task_planning": False,
            "thinking_level": self._thinking_level,
            "execution_profile": self._execution_profile,
            "memory_mode": "off",
            "skills_enabled": False,
            # Output contracts are manifest data, not prompt-text inference.
            # JSON work products exercise the Agent loop's canonicalization;
            # tagged multi-part scenarios retain their explicit text envelope.
            "output_format": self._task_output_formats[task_id],
        }


def _minimal_subprocess_env(*, home: Path, api_key: str) -> dict[str, str]:
    env: dict[str, str] = {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "DASHSCOPE_API_KEY": api_key,
        "PYTHON_DOTENV_DISABLED": "1",
    }
    for name in ("LANG", "LANGUAGE", "LC_ALL", "SSL_CERT_FILE", "SSL_CERT_DIR", "TZ"):
        value = os.environ.get(name)
        if value:
            env[name] = value
    return env


class HermesAdapter:
    def __init__(
        self,
        *,
        root: Path,
        api_key: str,
        base_url: str,
        model_id: str,
        temperature: float,
        max_tokens: int,
        thinking_level: str,
    ) -> None:
        self._root = root
        self._home = root / "home"
        self._home.mkdir(mode=0o700, parents=True)
        config = {
            "_config_version": 12,
            "model": {
                "provider": "custom:bench",
                "name": model_id,
                "max_tokens": max_tokens,
            },
            "custom_providers": [
                {
                    "name": "bench",
                    "base_url": base_url,
                    "key_env": "DASHSCOPE_API_KEY",
                    "api_mode": "chat_completions",
                    "model": model_id,
                    "context_length": 131072,
                    "extra_body": {
                        "temperature": temperature,
                        "enable_thinking": thinking_level != "off",
                        **(
                            {"thinking_budget": 256}
                            if thinking_level == "low"
                            else {"thinking_budget": 1024}
                            if thinking_level == "medium"
                            else {}
                        ),
                    },
                }
            ],
            "agent": {"max_turns": 32},
        }
        config_path = self._home / "config.yaml"
        config_path.write_text(yaml.safe_dump(config, sort_keys=False), encoding="utf-8")
        config_path.chmod(0o600)
        self._env = _minimal_subprocess_env(home=self._home, api_key=api_key)
        self._env["HERMES_HOME"] = str(self._home)
        self._model_id = model_id
        self._sessions: dict[str, str | None] = {}
        self._workspaces: dict[str, Path] = {}

    def start_task(self, task_id: str) -> None:
        workspace = self._root / "workspaces" / task_id.replace(".", "_")
        workspace.mkdir(mode=0o700, parents=True)
        self._workspaces[task_id] = workspace
        self._sessions[task_id] = None

    def run_turn(self, task_id: str, prompt: str) -> TurnResult:
        command = [
            str(HERMES_EXECUTABLE),
            "chat",
            "--query",
            prompt,
            "--quiet",
            "--provider",
            "custom:bench",
            "--model",
            self._model_id,
            "--max-turns",
            "32",
            "--ignore-rules",
            "--source",
            "benchmark",
        ]
        session_id = self._sessions[task_id]
        if session_id:
            command.extend(["--resume", session_id])
        started = time.monotonic()
        completed = subprocess.run(
            command,
            cwd=self._workspaces[task_id],
            env=self._env,
            capture_output=True,
            text=True,
            timeout=300,
            check=False,
        )
        duration = time.monotonic() - started
        if completed.returncode != 0:
            raise BenchmarkError(f"hermes_exit:{completed.returncode}")
        if not session_id:
            match = _SESSION_ID_RE.search(completed.stderr)
            if not match:
                raise BenchmarkError("hermes_session_id_missing")
            session_id = match.group(1)
            self._sessions[task_id] = session_id
        raw_stdout = completed.stdout.strip()
        diagnostic_prefixes = (
            "⚠ tirith security scanner enabled but not available",
            "⏱ Timeout — denying command",
        )
        diagnostics = [
            line.strip()
            for line in raw_stdout.splitlines()
            if any(line.strip().startswith(prefix) for prefix in diagnostic_prefixes)
        ]
        clean_stdout = "\n".join(
            line
            for line in raw_stdout.splitlines()
            if not any(line.strip().startswith(prefix) for prefix in diagnostic_prefixes)
        ).strip()
        return TurnResult(
            text=clean_stdout,
            duration_seconds=duration,
            terminal_status="process_exit_0",
            metadata={
                "session_id_sha256": _sha256_bytes(session_id.encode()),
                "stderr_sha256": _sha256_bytes(completed.stderr.encode()),
                "stdout_raw_sha256": _sha256_bytes(raw_stdout.encode()),
                "cli_diagnostic_count": len(diagnostics),
            },
        )


class OpenClawAdapter:
    def __init__(
        self,
        *,
        root: Path,
        api_key: str,
        base_url: str,
        model_id: str,
        temperature: float,
        max_tokens: int,
        thinking_level: str,
    ) -> None:
        self._root = root
        self._home = root / "home"
        self._state = root / "state"
        self._workspace = root / "workspace"
        for path in (self._home, self._state, self._workspace):
            path.mkdir(mode=0o700, parents=True)
        self._config_path = root / "openclaw.json"
        model_ref = f"dashscope/{model_id}"
        config = {
            "models": {
                "mode": "merge",
                "providers": {
                    "dashscope": {
                        "baseUrl": base_url,
                        "apiKey": "${DASHSCOPE_API_KEY}",
                        "api": "openai-completions",
                        "models": [
                            {
                                "id": model_id,
                                "name": model_id,
                                "reasoning": True,
                                "input": ["text"],
                                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                                "contextWindow": 131072,
                                "maxTokens": max_tokens,
                            }
                        ],
                    }
                },
            },
            "agents": {
                "defaults": {
                    "model": {"primary": model_ref},
                    "models": {
                        model_ref: {"params": {"temperature": temperature, "maxTokens": max_tokens}}
                    },
                    "workspace": str(self._workspace),
                },
                "list": [{"id": "main", "default": True}],
            },
        }
        self._config_path.write_text(json.dumps(config, indent=2), encoding="utf-8")
        self._config_path.chmod(0o600)
        self._model_id = model_id
        self._thinking_level = thinking_level
        self._env = _minimal_subprocess_env(home=self._home, api_key=api_key)
        self._env.update(
            {
                "OPENCLAW_CONFIG_PATH": str(self._config_path),
                "OPENCLAW_STATE_DIR": str(self._state),
                "OPENCLAW_LOAD_SHELL_ENV": "0",
            }
        )
        self._session_ids: dict[str, str] = {}

    def start_task(self, task_id: str) -> None:
        self._session_ids[task_id] = f"bench-{uuid.uuid4().hex}"

    def run_turn(self, task_id: str, prompt: str) -> TurnResult:
        command = [
            shutil.which("node") or "node",
            str(OPENCLAW_EXECUTABLE),
            "agent",
            "--local",
            "--session-id",
            self._session_ids[task_id],
            "--message",
            prompt,
            "--thinking",
            self._thinking_level,
            "--timeout",
            "300",
            "--json",
        ]
        started = time.monotonic()
        completed = subprocess.run(
            command,
            cwd=self._workspace,
            env=self._env,
            capture_output=True,
            text=True,
            timeout=330,
            check=False,
        )
        duration = time.monotonic() - started
        if completed.returncode != 0:
            raise BenchmarkError(f"openclaw_exit:{completed.returncode}")
        try:
            envelope = json.loads(completed.stdout)
        except json.JSONDecodeError as exc:
            raise BenchmarkError("openclaw_stdout_not_json") from exc
        payloads = envelope.get("payloads") if isinstance(envelope, dict) else None
        if not isinstance(payloads, list) or not payloads:
            raise BenchmarkError("openclaw_payloads_missing")
        if any(isinstance(item, dict) and item.get("isError") for item in payloads):
            raise BenchmarkError("openclaw_payload_error")
        texts = [item.get("text") for item in payloads if isinstance(item, dict)]
        if not texts or not all(isinstance(text, str) for text in texts):
            raise BenchmarkError("openclaw_text_missing")
        meta = envelope.get("meta") if isinstance(envelope.get("meta"), dict) else {}
        agent_meta = meta.get("agentMeta") if isinstance(meta.get("agentMeta"), dict) else {}
        model = agent_meta.get("model")
        provider = agent_meta.get("provider")
        if model != self._model_id:
            raise BenchmarkError("openclaw_runtime_model_mismatch")
        if not isinstance(provider, str) or not provider:
            raise BenchmarkError("openclaw_runtime_provider_missing")
        if meta.get("aborted") is True or meta.get("stopReason") in {"aborted", "error"}:
            raise BenchmarkError("openclaw_terminal_failure")
        return TurnResult(
            text="\n".join(texts).strip(),
            duration_seconds=duration,
            terminal_status=str(meta.get("stopReason") or "process_exit_0"),
            metadata={
                "session_id_sha256": _sha256_bytes(self._session_ids[task_id].encode()),
                "stderr_sha256": _sha256_bytes(completed.stderr.encode()),
                "model": model,
                "provider": provider,
                "usage": agent_meta.get("usage"),
            },
        )


def _load_runtime_inputs(env_path: Path) -> dict[str, str]:
    file_env = dotenv_values(env_path) if env_path.exists() else {}

    def value(*names: str) -> str:
        for name in names:
            candidate = os.environ.get(name) or file_env.get(name)
            if isinstance(candidate, str) and candidate:
                return candidate
        return ""

    api_key = value("DASHSCOPE_CHAT_API_KEY", "DASHSCOPE_API_KEY")
    password = value("DEFAULT_USER_PASSWORD")
    domain = value("AUTH_ALLOWED_EMAIL_DOMAIN") or "example.com"
    raw_base = (
        value("DASHSCOPE_CHAT_BASE_URL") or "https://dashscope-intl.aliyuncs.com/compatible-mode"
    )
    base_url = raw_base.rstrip("/")
    if not base_url.endswith("/v1"):
        base_url += "/v1"
    if not api_key:
        raise BenchmarkError("dashscope_key_missing")
    if not password:
        raise BenchmarkError("gateway_password_missing")
    if not base_url.startswith("https://"):
        raise BenchmarkError("provider_base_url_must_be_https")
    return {
        "api_key": api_key,
        "password": password,
        "email": f"admin@{domain}",
        "provider_base_url": base_url,
    }


def _version_receipts() -> dict[str, Any]:
    def command_text(command: list[str], cwd: Path) -> str:
        completed = subprocess.run(
            command,
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        if completed.returncode != 0:
            raise BenchmarkError(f"version_probe_exit:{completed.returncode}")
        return (completed.stdout or completed.stderr).strip().splitlines()[0]

    return {
        "ai_platform": {
            "git_commit": subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
            ).strip(),
            "git_diff_sha256": _sha256_bytes(
                subprocess.check_output(["git", "diff", "--binary"], cwd=ROOT)
            ),
        },
        "hermes": {
            "version": command_text([str(HERMES_EXECUTABLE), "--version"], HERMES_ROOT),
            "git_commit": subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=HERMES_ROOT, text=True
            ).strip(),
        },
        "openclaw": {
            "version": command_text(
                [shutil.which("node") or "node", str(OPENCLAW_EXECUTABLE), "--version"],
                OPENCLAW_ROOT,
            ),
            "git_commit": subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=OPENCLAW_ROOT, text=True
            ).strip(),
        },
    }


def _make_adapter(
    system: str,
    *,
    temp_root: Path,
    runtime: dict[str, str],
    manifest: dict[str, Any],
    gateway_base_url: str,
):
    common = {
        "model_id": manifest["model_id"],
        "temperature": manifest["temperature"],
        "max_tokens": manifest["max_tokens"],
        "thinking_level": manifest["thinking_level"],
    }
    if system == "ai_platform":
        return AIPlatformAdapter(
            gateway_base_url=gateway_base_url,
            email=runtime["email"],
            password=runtime["password"],
            execution_profile=manifest["execution_profile"],
            max_approval_rounds=manifest["max_approval_rounds"],
            task_output_formats={
                task["task_id"]: task["output_format"] for task in manifest["tasks"]
            },
            **common,
        )
    if system == "hermes":
        return HermesAdapter(
            root=temp_root / system,
            api_key=runtime["api_key"],
            base_url=runtime["provider_base_url"],
            **common,
        )
    if system == "openclaw":
        return OpenClawAdapter(
            root=temp_root / system,
            api_key=runtime["api_key"],
            base_url=runtime["provider_base_url"],
            **common,
        )
    raise BenchmarkError(f"unknown_system:{system}")


def run_suite(
    *,
    manifest_path: Path,
    output_dir: Path,
    gateway_base_url: str,
    env_path: Path,
    selected_systems: tuple[str, ...] = SYSTEMS,
    selected_tasks: set[str] | None = None,
    thinking_level_override: str | None = None,
) -> dict[str, Any]:
    manifest = load_manifest(manifest_path)
    manifest_thinking_level = manifest["thinking_level"]
    if thinking_level_override is not None:
        if thinking_level_override not in {"off", "low", "medium", "high"}:
            raise BenchmarkError("invalid_thinking_level_override")
        manifest = {**manifest, "thinking_level": thinking_level_override}
    runtime = _load_runtime_inputs(env_path)
    output_dir.mkdir(mode=0o700, parents=True, exist_ok=False)
    output_dir.chmod(0o700)
    tasks = [
        task
        for task in manifest["tasks"]
        if selected_tasks is None or task["task_id"] in selected_tasks
    ]
    if not tasks:
        raise BenchmarkError("no_selected_tasks")
    started_at = datetime.now(timezone.utc).isoformat()
    receipts: list[dict[str, Any]] = []
    with tempfile.TemporaryDirectory(prefix="native-agent-parity-") as raw_temp:
        temp_root = Path(raw_temp)
        temp_root.chmod(0o700)
        for system in selected_systems:
            adapter = _make_adapter(
                system,
                temp_root=temp_root,
                runtime=runtime,
                manifest=manifest,
                gateway_base_url=gateway_base_url,
            )
            for task in tasks:
                task_id = task["task_id"]
                adapter.start_task(task_id)
                turn_receipts: list[dict[str, Any]] = []
                task_passed = True
                for ordinal, turn in enumerate(task["turns"], start=1):
                    try:
                        result = adapter.run_turn(task_id, _turn_prompt(turn))
                        passed, reason, parsed = validate_turn(
                            validator=task["validator"],
                            text=result.text,
                            expected=turn["expected"],
                            turn=turn,
                        )
                        turn_receipt = {
                            "ordinal": ordinal,
                            "passed": passed,
                            "reason": reason,
                            "duration_seconds": round(result.duration_seconds, 3),
                            "terminal_status": result.terminal_status,
                            "output": result.text,
                            "output_sha256": _sha256_bytes(result.text.encode()),
                            "parsed_output": parsed,
                            "metadata": result.metadata,
                        }
                    except (BenchmarkError, subprocess.TimeoutExpired) as exc:
                        passed = False
                        task_passed = False
                        turn_receipt = {
                            "ordinal": ordinal,
                            "passed": False,
                            "reason": str(exc)
                            if isinstance(exc, BenchmarkError)
                            else "subprocess_timeout",
                            "duration_seconds": None,
                            "terminal_status": "infrastructure_error",
                            "output": "",
                            "output_sha256": _sha256_bytes(b""),
                            "parsed_output": None,
                            "metadata": {},
                        }
                    turn_receipts.append(turn_receipt)
                    if not passed:
                        task_passed = False
                        break
                receipt = {
                    "system": system,
                    "task_id": task_id,
                    "domain": task["domain"],
                    "passed": task_passed and len(turn_receipts) == len(task["turns"]),
                    "turns": turn_receipts,
                }
                receipts.append(receipt)
                _write_private_json(output_dir / f"{system}__{task_id}.json", receipt)
                print(
                    f"{system:11s} {task_id:42s} {'PASS' if receipt['passed'] else 'FAIL'}",
                    flush=True,
                )
    counts: dict[str, dict[str, Any]] = {}
    for system in selected_systems:
        system_receipts = [receipt for receipt in receipts if receipt["system"] == system]
        passed = sum(bool(receipt["passed"]) for receipt in system_receipts)
        infrastructure_errors = sum(
            any(turn["terminal_status"] == "infrastructure_error" for turn in receipt["turns"])
            for receipt in system_receipts
        )
        counts[system] = {
            "passed": passed,
            "total": len(system_receipts),
            "infrastructure_errors": infrastructure_errors,
            "completion_rate": passed / len(system_receipts) if system_receipts else 0.0,
        }
    acceptance = (
        selected_systems == SYSTEMS
        and len(tasks) == len(manifest["tasks"])
        and all(counts[system]["infrastructure_errors"] == 0 for system in SYSTEMS)
        and counts["ai_platform"]["passed"] > 0
        and counts["ai_platform"]["completion_rate"] >= counts["hermes"]["completion_rate"]
        and counts["ai_platform"]["completion_rate"] >= counts["openclaw"]["completion_rate"]
    )
    summary = {
        "schema_version": "native-agent-parity-result/v1",
        "suite_id": manifest["suite_id"],
        "manifest_sha256": _sha256_file(manifest_path),
        "started_at": started_at,
        "finished_at": datetime.now(timezone.utc).isoformat(),
        "model_id": manifest["model_id"],
        "temperature": manifest["temperature"],
        "max_tokens": manifest["max_tokens"],
        "thinking_level": manifest["thinking_level"],
        "manifest_thinking_level": manifest_thinking_level,
        "thinking_level_override_applied": thinking_level_override is not None,
        "execution_profile": manifest["execution_profile"],
        "evidence": _benchmark_evidence_receipt(
            manifest_path=manifest_path,
            manifest=manifest,
        ),
        "versions": _version_receipts(),
        "counts": counts,
        "acceptance_passed": acceptance,
        "task_results": [
            {"system": item["system"], "task_id": item["task_id"], "passed": item["passed"]}
            for item in receipts
        ],
    }
    _write_private_json(output_dir / "summary.json", summary)
    return summary


def _parse_csv(value: str) -> tuple[str, ...]:
    items = tuple(item.strip() for item in value.split(",") if item.strip())
    invalid = set(items) - set(SYSTEMS)
    if invalid or not items:
        raise argparse.ArgumentTypeError(f"invalid systems: {sorted(invalid)}")
    return items


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--gateway-base-url", default="http://127.0.0.1:8080/api/v1")
    parser.add_argument("--env-file", type=Path, default=ROOT / ".env")
    parser.add_argument("--systems", type=_parse_csv, default=SYSTEMS)
    parser.add_argument("--task", action="append", dest="tasks")
    parser.add_argument(
        "--thinking-level",
        choices=("off", "low", "medium", "high"),
        help="Override the manifest reasoning profile and record the override in the receipt.",
    )
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--validate-only", action="store_true")
    args = parser.parse_args(argv)
    manifest = load_manifest(args.manifest)
    if args.validate_only:
        print(
            json.dumps(
                {
                    "valid": True,
                    "task_count": len(manifest["tasks"]),
                    "manifest_sha256": _sha256_file(args.manifest),
                    "provider_calls": 0,
                },
                sort_keys=True,
            )
        )
        return 0
    run_id = datetime.now(timezone.utc).strftime("native-parity-%Y%m%dT%H%M%SZ")
    output_dir = args.output_dir or DEFAULT_RESULTS_ROOT / run_id
    try:
        summary = run_suite(
            manifest_path=args.manifest,
            output_dir=output_dir,
            gateway_base_url=args.gateway_base_url,
            env_path=args.env_file,
            selected_systems=args.systems,
            selected_tasks=set(args.tasks) if args.tasks else None,
            thinking_level_override=args.thinking_level,
        )
    except BenchmarkError as exc:
        print(json.dumps({"status": "infrastructure_error", "reason": str(exc)}))
        return 2
    print(json.dumps(summary["counts"], sort_keys=True))
    return 0 if summary["acceptance_passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
