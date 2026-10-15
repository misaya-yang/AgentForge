from __future__ import annotations

from ai_gateway_core.agents.spec import (
    agent_spec_safety_errors,
    redact_agent_spec_for_read,
    render_agent_outcome_contract,
    sanitize_agent_copy_spec,
    validate_agent_spec,
)


def _spec() -> dict:
    return {
        "schema_version": "agent-spec/v1",
        "identity": {},
        "outcome": {
            "goal": "Resolve support requests with a clear owner.",
            "triggers": ["A new support request arrives"],
            "inputs": ["Request text", "Customer tier"],
            "human_boundaries": ["Ask before issuing a refund"],
            "success_criteria": ["Queue and owner are explicit"],
        },
        "instructions": "Use only configured capabilities.",
        "model": {"model_id": "qwen3.7-plus"},
        "capabilities": [],
        "knowledge": [],
        "memory": {},
    }


def test_outcome_contract_is_closed_redacted_and_rendered_stably() -> None:
    spec = _spec()
    assert agent_spec_safety_errors(spec) == []
    assert redact_agent_spec_for_read(spec)["outcome"] == spec["outcome"]
    assert render_agent_outcome_contract(spec) == (
        "[Agent outcome contract]\n"
        "Goal: Resolve support requests with a clear owner.\n"
        "Triggers:\n- A new support request arrives\n"
        "Required inputs:\n- Request text\n- Customer tier\n"
        "Human boundaries:\n- Ask before issuing a refund\n"
        "Success criteria:\n- Queue and owner are explicit"
    )


def test_outcome_contract_rejects_unknown_or_empty_list_items() -> None:
    spec = _spec()
    spec["outcome"]["unknown"] = "forbidden"
    spec["outcome"]["success_criteria"] = [""]
    errors = validate_agent_spec(spec)
    assert {error["code"] for error in errors} == {
        "AGENT_SPEC_FIELD_FORBIDDEN",
        "AGENT_OUTCOME_LIST_INVALID",
    }


def test_copy_preserves_present_outcome_without_inventing_one_for_legacy_specs() -> None:
    copied = sanitize_agent_copy_spec(_spec())
    assert copied["outcome"] == _spec()["outcome"]
    legacy = _spec()
    legacy.pop("outcome")
    assert "outcome" not in sanitize_agent_copy_spec(legacy)
