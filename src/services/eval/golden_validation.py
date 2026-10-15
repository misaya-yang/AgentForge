from __future__ import annotations

import math
from typing import Any

SUPPORTED_ASSERTIONS = {
    "output_equals",
    "output_contains",
    "output_not_contains",
    "required_span_kind",
    "no_sensitive_output",
    "latency_ms_lt",
    "total_tokens_lt",
    "cost_cents_lt",
    "tool_called",
    "tool_not_called",
    "failure_mode_absent",
}

SUPPORTED_TOOL_EXPECTATION_FIELDS = {
    "name",
    "required",
    "forbidden",
    "arguments_subset",
    "order",
    "max_calls",
    "status",
}

SUPPORTED_STATEFUL_EXPECTATION_FIELDS = {
    "minimum_turns",
    "plan",
    "tool_pairing",
    "budget",
    "hitl",
    "compaction",
    "security",
}

SUPPORTED_STATEFUL_NESTED_FIELDS = {
    "plan": {"plan_id", "goal", "required_steps", "final_completed_steps"},
    "tool_pairing": {"required", "require_success"},
    "budget": {"max_iterations", "expected_exit_reason", "terminal_turn"},
    "hitl": {
        "approved_calls",
        "checkpoint_id",
        "max_dispatch_before_approval",
        "minimum_pending_approval_calls",
        "minimum_postapproval_dispatches",
        "protected_tools",
        "resume_count",
    },
    "compaction": {
        "compaction_id",
        "required_facts",
        "max_dropped_required_facts",
    },
    "security": {
        "untrusted_instructions_ignored",
        "forbidden_tools",
        "tenant_id",
        "foreign_tenant_access",
    },
}

REQUIRED_STATEFUL_NESTED_FIELDS = {
    "plan": {"plan_id", "goal", "required_steps", "final_completed_steps"},
    "tool_pairing": {"required"},
    "budget": {"max_iterations", "expected_exit_reason", "terminal_turn"},
    "hitl": {
        "approved_calls",
        "checkpoint_id",
        "max_dispatch_before_approval",
        "minimum_pending_approval_calls",
        "minimum_postapproval_dispatches",
        "protected_tools",
        "resume_count",
    },
    "compaction": {
        "compaction_id",
        "required_facts",
        "max_dropped_required_facts",
    },
}

STRING_ASSERTIONS = {
    "output_equals",
    "output_contains",
    "output_not_contains",
    "required_span_kind",
    "tool_called",
    "tool_not_called",
    "failure_mode_absent",
}

NUMERIC_ASSERTIONS = {"latency_ms_lt", "total_tokens_lt", "cost_cents_lt"}

__all__ = [
    "NUMERIC_ASSERTIONS",
    "REQUIRED_STATEFUL_NESTED_FIELDS",
    "STRING_ASSERTIONS",
    "SUPPORTED_ASSERTIONS",
    "SUPPORTED_STATEFUL_EXPECTATION_FIELDS",
    "SUPPORTED_STATEFUL_NESTED_FIELDS",
    "SUPPORTED_TOOL_EXPECTATION_FIELDS",
    "validate_case",
    "validate_observations",
]


def validate_observations(
    cases: list[dict[str, Any]],
    observations: dict[str, dict[str, Any]],
) -> dict[str, Any]:
    case_ids = {
        str(case.get("case_id"))
        for case in cases
        if isinstance(case.get("case_id"), str) and case.get("case_id")
    }
    observation_ids = set(observations)
    errors: list[dict[str, Any]] = []
    for case_id in sorted(case_ids - observation_ids):
        errors.append({"case_id": case_id, "errors": ["missing replay observation"]})
    for case_id in sorted(observation_ids - case_ids):
        errors.append({"case_id": case_id, "errors": ["observation has no expectation"]})
    for case_id in sorted(case_ids & observation_ids):
        replay = observations.get(case_id)
        replay_errors: list[str] = []
        if not isinstance(replay, dict) or not replay:
            replay_errors.append("replay observation must be a non-empty object")
        elif not isinstance(replay.get("status"), str) or not replay.get("status"):
            replay_errors.append("replay observation must have a non-empty status")
        if replay_errors:
            errors.append({"case_id": case_id, "errors": replay_errors})
    return {
        "valid": not errors,
        "case_count": len(case_ids),
        "observation_count": len(observation_ids),
        "joined_count": len(case_ids & observation_ids),
        "errors": errors,
    }


def validate_case(case: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    for key in (
        "case_id",
        "input",
        "expected_output",
        "expected_trajectory",
        "assertions",
        "metadata",
    ):
        if key not in case:
            errors.append(f"missing {key}")
    if not isinstance(case.get("case_id"), str) or not case.get("case_id"):
        errors.append("case_id must be a non-empty string")
    for key in ("input", "expected_output", "expected_trajectory", "metadata"):
        if key in case and not isinstance(case.get(key), dict):
            errors.append(f"{key} must be an object")
    expected_trajectory = case.get("expected_trajectory")
    if (
        isinstance(expected_trajectory, dict)
        and "runtime" in expected_trajectory
        and not isinstance(expected_trajectory.get("runtime"), dict)
    ):
        errors.append("expected_trajectory.runtime must be an object")
    if isinstance(expected_trajectory, dict) and "required_span_kinds" in expected_trajectory:
        required_spans = expected_trajectory.get("required_span_kinds")
        if not isinstance(required_spans, list) or any(
            not isinstance(item, str) or not item.strip() for item in required_spans
        ):
            errors.append("expected_trajectory.required_span_kinds must be a string list")
    if isinstance(expected_trajectory, dict) and "stateful" in expected_trajectory:
        errors.extend(_validate_stateful_expectations(expected_trajectory.get("stateful")))
    expected_output = case.get("expected_output")
    if isinstance(expected_output, dict):
        for key in ("reference", "rubric"):
            if key in expected_output and (
                not isinstance(expected_output[key], str) or not expected_output[key].strip()
            ):
                errors.append(f"expected_output.{key} must be a non-empty string")
        for key in ("contains", "not_contains"):
            if key not in expected_output:
                continue
            value = expected_output[key]
            if isinstance(value, str):
                valid = bool(value.strip())
            else:
                valid = (
                    isinstance(value, list)
                    and bool(value)
                    and all(isinstance(item, str) and item.strip() for item in value)
                )
            if not valid:
                errors.append(f"expected_output.{key} must be a non-empty string or string list")
    if isinstance(expected_trajectory, dict) and "tools" in expected_trajectory:
        tools = expected_trajectory.get("tools")
        if not isinstance(tools, list):
            errors.append("expected_trajectory.tools must be a list")
        else:
            for index, tool in enumerate(tools, start=1):
                prefix = f"expected_trajectory.tools[{index}]"
                if not isinstance(tool, dict):
                    errors.append(f"{prefix} must be an object")
                    continue
                unknown = sorted(set(tool) - SUPPORTED_TOOL_EXPECTATION_FIELDS)
                if unknown:
                    errors.append(f"{prefix} has unsupported fields {', '.join(unknown)}")
                if not isinstance(tool.get("name"), str) or not tool.get("name", "").strip():
                    errors.append(f"{prefix}.name must be a non-empty string")
                for key in ("required", "forbidden"):
                    if key in tool and not isinstance(tool[key], bool):
                        errors.append(f"{prefix}.{key} must be boolean")
                if tool.get("required") is True and tool.get("forbidden") is True:
                    errors.append(f"{prefix} cannot be both required and forbidden")
                if "arguments_subset" in tool and not isinstance(tool["arguments_subset"], dict):
                    errors.append(f"{prefix}.arguments_subset must be an object")
                if "order" in tool and (
                    isinstance(tool["order"], bool)
                    or not isinstance(tool["order"], int)
                    or tool["order"] < 1
                ):
                    errors.append(f"{prefix}.order must be a positive integer")
                if "max_calls" in tool and (
                    isinstance(tool["max_calls"], bool)
                    or not isinstance(tool["max_calls"], int)
                    or tool["max_calls"] < 0
                ):
                    errors.append(f"{prefix}.max_calls must be a non-negative integer")
                if "status" in tool and (
                    not isinstance(tool["status"], str) or not tool["status"].strip()
                ):
                    errors.append(f"{prefix}.status must be a non-empty string")
    if "assertions" in case and not isinstance(case.get("assertions"), list):
        errors.append("assertions must be a list")
    elif isinstance(case.get("assertions"), list):
        for index, assertion in enumerate(case["assertions"], start=1):
            if not isinstance(assertion, dict):
                errors.append(f"assertions[{index}] must be an object")
                continue
            assertion_type = assertion.get("type")
            if assertion_type not in SUPPORTED_ASSERTIONS:
                errors.append(f"assertions[{index}] has unsupported type {assertion_type!r}")
            elif assertion_type in STRING_ASSERTIONS and (
                not isinstance(assertion.get("value"), str) or not assertion["value"].strip()
            ):
                errors.append(f"assertions[{index}].value must be a non-empty string")
            elif assertion_type in NUMERIC_ASSERTIONS and (
                isinstance(assertion.get("value"), bool)
                or not isinstance(assertion.get("value"), (int, float))
                or not math.isfinite(float(assertion["value"]))
                or assertion["value"] <= 0
            ):
                errors.append(f"assertions[{index}].value must be a positive number")
    split = case.get("split", "regression")
    if not isinstance(split, str) or not split:
        errors.append("split must be a non-empty string")
    metadata = case.get("metadata") if isinstance(case.get("metadata"), dict) else {}
    if metadata.get("critical") is not None and not isinstance(metadata.get("critical"), bool):
        errors.append("metadata.critical must be boolean when present")
    if metadata.get("behavior_confirmed") is not None and not isinstance(
        metadata.get("behavior_confirmed"), bool
    ):
        errors.append("metadata.behavior_confirmed must be boolean when present")
    if metadata.get("owner") is not None and not isinstance(metadata.get("owner"), str):
        errors.append("metadata.owner must be a string when present")
    if metadata.get("difficulty") is not None and not isinstance(
        metadata.get("difficulty"), str
    ):
        errors.append("metadata.difficulty must be a string when present")
    if metadata.get("review_status") is not None and metadata.get("review_status") not in {
        "pending",
        "approved",
        "rejected",
        "needs_fix",
    }:
        errors.append("metadata.review_status is invalid")
    if metadata.get("tags") is not None and (
        not isinstance(metadata.get("tags"), list)
        or any(not isinstance(tag, str) or not tag.strip() for tag in metadata["tags"])
    ):
        errors.append("metadata.tags must be a string list when present")
    return errors

def _validate_stateful_expectations(value: Any) -> list[str]:
    prefix = "expected_trajectory.stateful"
    if not isinstance(value, dict) or not value:
        return [f"{prefix} must be a non-empty object"]
    errors: list[str] = []
    unknown = sorted(set(value) - SUPPORTED_STATEFUL_EXPECTATION_FIELDS)
    if unknown:
        errors.append(f"{prefix} has unsupported fields {', '.join(unknown)}")
    minimum_turns = value.get("minimum_turns")
    if minimum_turns is not None and (
        isinstance(minimum_turns, bool)
        or not isinstance(minimum_turns, int)
        or minimum_turns < 2
    ):
        errors.append(f"{prefix}.minimum_turns must be an integer >= 2")
    for section, allowed in SUPPORTED_STATEFUL_NESTED_FIELDS.items():
        if section not in value:
            continue
        section_value = value.get(section)
        section_prefix = f"{prefix}.{section}"
        if not isinstance(section_value, dict) or not section_value:
            errors.append(f"{section_prefix} must be a non-empty object")
            continue
        section_unknown = sorted(set(section_value) - allowed)
        if section_unknown:
            errors.append(
                f"{section_prefix} has unsupported fields {', '.join(section_unknown)}"
            )
        missing = sorted(REQUIRED_STATEFUL_NESTED_FIELDS.get(section, set()) - set(section_value))
        if missing:
            errors.append(f"{section_prefix} is missing required fields {', '.join(missing)}")

    plan = value.get("plan")
    if isinstance(plan, dict):
        for key in ("plan_id", "goal"):
            if key in plan and (not isinstance(plan[key], str) or not plan[key].strip()):
                errors.append(f"{prefix}.plan.{key} must be a non-empty string")
        for key in ("required_steps", "final_completed_steps"):
            if key in plan and (
                not isinstance(plan[key], list)
                or not plan[key]
                or any(not isinstance(item, str) or not item.strip() for item in plan[key])
            ):
                errors.append(f"{prefix}.plan.{key} must be a non-empty string list")

    tool_pairing = value.get("tool_pairing")
    if isinstance(tool_pairing, dict) and tool_pairing.get("required") is not True:
        errors.append(f"{prefix}.tool_pairing.required must be true")
    if isinstance(tool_pairing, dict) and "require_success" in tool_pairing and not isinstance(
        tool_pairing.get("require_success"), bool
    ):
        errors.append(f"{prefix}.tool_pairing.require_success must be boolean")

    budget = value.get("budget")
    if isinstance(budget, dict):
        for key in ("max_iterations", "terminal_turn"):
            if key in budget and (
                isinstance(budget[key], bool)
                or not isinstance(budget[key], int)
                or budget[key] < 1
            ):
                errors.append(f"{prefix}.budget.{key} must be a positive integer")
        if "expected_exit_reason" in budget and (
            not isinstance(budget["expected_exit_reason"], str)
            or not budget["expected_exit_reason"].strip()
        ):
            errors.append(f"{prefix}.budget.expected_exit_reason must be a non-empty string")
        if (
            budget.get("expected_exit_reason") == "max_iterations"
            and isinstance(budget.get("max_iterations"), int)
            and not isinstance(budget.get("max_iterations"), bool)
            and budget.get("terminal_turn") != budget.get("max_iterations")
        ):
            errors.append(
                f"{prefix}.budget.terminal_turn must equal max_iterations for max_iterations exit"
            )

    hitl = value.get("hitl")
    if isinstance(hitl, dict):
        if not isinstance(hitl.get("checkpoint_id"), str) or not hitl.get(
            "checkpoint_id", ""
        ).strip():
            errors.append(f"{prefix}.hitl.checkpoint_id must be a non-empty string")
        maximum_dispatch = hitl.get("max_dispatch_before_approval")
        if maximum_dispatch is not None and (
            isinstance(maximum_dispatch, bool)
            or not isinstance(maximum_dispatch, int)
            or maximum_dispatch < 0
        ):
            errors.append(
                f"{prefix}.hitl.max_dispatch_before_approval must be a non-negative integer"
            )
        resume_count = hitl.get("resume_count")
        if (
            resume_count is not None
            and (
                isinstance(resume_count, bool)
                or not isinstance(resume_count, int)
                or resume_count != 1
            )
        ):
            errors.append(f"{prefix}.hitl.resume_count must equal 1 in the v1 contract")
        for key in ("minimum_pending_approval_calls", "minimum_postapproval_dispatches"):
            minimum_count = hitl.get(key)
            if minimum_count is not None and (
                isinstance(minimum_count, bool)
                or not isinstance(minimum_count, int)
                or minimum_count < 1
            ):
                errors.append(f"{prefix}.hitl.{key} must be a positive integer")
        approved_calls = hitl.get("approved_calls")
        protected_tools = hitl.get("protected_tools")
        valid_protected_tools = (
            isinstance(protected_tools, list)
            and bool(protected_tools)
            and all(
                isinstance(tool_name, str) and bool(tool_name.strip())
                for tool_name in protected_tools
            )
            and len(protected_tools) == len(set(protected_tools))
        )
        if not valid_protected_tools:
            errors.append(f"{prefix}.hitl.protected_tools must be a unique string list")
        if not isinstance(approved_calls, list) or not approved_calls:
            errors.append(f"{prefix}.hitl.approved_calls must be a non-empty object list")
        else:
            approved_ids: set[str] = set()
            for index, approved_call in enumerate(approved_calls, start=1):
                call_prefix = f"{prefix}.hitl.approved_calls[{index}]"
                if not isinstance(approved_call, dict):
                    errors.append(f"{call_prefix} must be an object")
                    continue
                unknown = sorted(
                    set(approved_call) - {"call_id", "tool_name", "arguments_hash"}
                )
                if unknown:
                    errors.append(f"{call_prefix} has unsupported fields {', '.join(unknown)}")
                for key in ("call_id", "tool_name", "arguments_hash"):
                    if not isinstance(approved_call.get(key), str) or not approved_call.get(
                        key, ""
                    ).strip():
                        errors.append(f"{call_prefix}.{key} must be a non-empty string")
                call_id = approved_call.get("call_id")
                if isinstance(call_id, str) and call_id.strip():
                    if call_id in approved_ids:
                        errors.append(f"{call_prefix}.call_id must be unique")
                    approved_ids.add(call_id)
                arguments_hash = approved_call.get("arguments_hash")
                if isinstance(arguments_hash, str) and (
                    len(arguments_hash) != 64
                    or any(character not in "0123456789abcdef" for character in arguments_hash)
                ):
                    errors.append(f"{call_prefix}.arguments_hash must be lowercase SHA-256")
                if (
                    valid_protected_tools
                    and isinstance(approved_call.get("tool_name"), str)
                    and approved_call.get("tool_name") not in set(protected_tools)
                ):
                    errors.append(
                        f"{call_prefix}.tool_name must be listed in protected_tools"
                    )
            if isinstance(hitl.get("minimum_pending_approval_calls"), int) and hitl.get(
                "minimum_pending_approval_calls"
            ) > len(approved_calls):
                errors.append(
                    f"{prefix}.hitl.minimum_pending_approval_calls exceeds approved_calls"
                )
            if isinstance(hitl.get("minimum_postapproval_dispatches"), int) and hitl.get(
                "minimum_postapproval_dispatches"
            ) > len(approved_calls):
                errors.append(
                    f"{prefix}.hitl.minimum_postapproval_dispatches exceeds approved_calls"
                )

    compaction = value.get("compaction")
    if isinstance(compaction, dict):
        if not isinstance(compaction.get("compaction_id"), str) or not compaction.get(
            "compaction_id", ""
        ).strip():
            errors.append(f"{prefix}.compaction.compaction_id must be a non-empty string")
        required_facts = compaction.get("required_facts")
        if (
            not isinstance(required_facts, list)
            or not required_facts
            or any(not isinstance(item, str) or not item.strip() for item in required_facts)
        ):
            errors.append(f"{prefix}.compaction.required_facts must be a string list")
        maximum = compaction.get("max_dropped_required_facts")
        if maximum is not None and (
            isinstance(maximum, bool) or not isinstance(maximum, int) or maximum < 0
        ):
            errors.append(
                f"{prefix}.compaction.max_dropped_required_facts must be non-negative"
            )

    security = value.get("security")
    if isinstance(security, dict):
        for key in ("untrusted_instructions_ignored", "foreign_tenant_access"):
            if key in security and not isinstance(security[key], bool):
                errors.append(f"{prefix}.security.{key} must be boolean")
        tenant_id = security.get("tenant_id")
        if tenant_id is not None and (
            not isinstance(tenant_id, str) or not tenant_id.strip()
        ):
            errors.append(f"{prefix}.security.tenant_id must be a non-empty string")
        forbidden_tools = security.get("forbidden_tools")
        if forbidden_tools is not None and (
            not isinstance(forbidden_tools, list)
            or any(not isinstance(item, str) or not item.strip() for item in forbidden_tools)
        ):
            errors.append(f"{prefix}.security.forbidden_tools must be a string list")
    return errors
