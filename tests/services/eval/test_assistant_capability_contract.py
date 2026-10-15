from __future__ import annotations

import pytest

from src.services.eval.assistant_capability import (
    CapabilityCasePolicy,
    CapabilityCheck,
    CapabilityTrialReceipt,
    evaluate_capability_suite,
)


def _receipt(
    case_id: str,
    trial: int,
    *,
    passed: bool = True,
    tier: str = "real_provider",
    kind: str = "outcome",
    name: str = "exact_answer",
) -> CapabilityTrialReceipt:
    return CapabilityTrialReceipt(
        case_id=case_id,
        evidence_tier=tier,
        trial=trial,
        checks=(
            CapabilityCheck(
                name=name,
                kind=kind,
                passed=passed,
                evidence=f"trial-{trial} bounded receipt",
            ),
        ),
    )


def test_quality_case_passes_two_of_three_result_level_trials() -> None:
    policy = CapabilityCasePolicy(
        case_id="assistant.multi_turn.exact_marker",
        evidence_tier="real_provider",
    )
    receipts = [_receipt(policy.case_id, 1), _receipt(policy.case_id, 2)]
    receipts.append(_receipt(policy.case_id, 3, passed=False))

    result = evaluate_capability_suite([policy], receipts)

    assert result["passed"] is True
    assert result["case_pass_rate"] == 1.0
    assert result["cases"][0]["observed_passes"] == 2


def test_critical_case_requires_every_repetition() -> None:
    policy = CapabilityCasePolicy(
        case_id="assistant.security.tenant_isolation",
        evidence_tier="local_live",
        critical=True,
        minimum_passes=3,
    )
    receipts = [
        _receipt(policy.case_id, 1, tier="local_live", kind="safety", name="tenant_isolated"),
        _receipt(policy.case_id, 2, tier="local_live", kind="safety", name="tenant_isolated"),
        _receipt(
            policy.case_id,
            3,
            tier="local_live",
            kind="safety",
            name="tenant_isolated",
            passed=False,
        ),
    ]

    result = evaluate_capability_suite([policy], receipts)

    assert result["passed"] is False
    assert result["critical_failures"] == [policy.case_id]


@pytest.mark.parametrize("check_name", ["non_empty_output", "no_run_error"])
def test_proxy_only_receipts_never_prove_capability(check_name: str) -> None:
    policy = CapabilityCasePolicy(
        case_id="assistant.code.executes",
        evidence_tier="local_live",
    )
    receipts = [
        _receipt(
            policy.case_id,
            trial,
            tier="local_live",
            kind="proxy",
            name=check_name,
        )
        for trial in range(1, 4)
    ]

    result = evaluate_capability_suite([policy], receipts)

    assert result["passed"] is False
    assert result["cases"][0]["observed_passes"] == 0
    assert "proxy-only trials cannot prove capability" in " ".join(result["failures"])


def test_protocol_receipt_requires_an_outcome_or_safety_check() -> None:
    policy = CapabilityCasePolicy(
        case_id="assistant.document.valid_pdf",
        evidence_tier="local_live",
    )
    receipts = [
        _receipt(
            policy.case_id,
            trial,
            tier="local_live",
            kind="protocol",
            name="terminal_received",
        )
        for trial in range(1, 4)
    ]

    assert evaluate_capability_suite([policy], receipts)["passed"] is False


def test_mixed_evidence_tiers_fail_closed() -> None:
    policy = CapabilityCasePolicy(
        case_id="assistant.kb.grounded",
        evidence_tier="real_provider",
    )
    receipts = [_receipt(policy.case_id, 1), _receipt(policy.case_id, 2)]
    receipts.append(_receipt(policy.case_id, 3, tier="mock"))

    result = evaluate_capability_suite([policy], receipts)

    assert result["passed"] is False
    assert "receipt evidence tier does not match policy" in " ".join(result["failures"])


def test_trial_numbers_and_counts_are_exact() -> None:
    policy = CapabilityCasePolicy(
        case_id="assistant.control.cancel",
        evidence_tier="browser",
        critical=True,
        minimum_passes=3,
    )
    receipts = [
        _receipt(policy.case_id, 1, tier="browser", kind="safety", name="cancelled"),
        _receipt(policy.case_id, 3, tier="browser", kind="safety", name="cancelled"),
        _receipt(policy.case_id, 4, tier="browser", kind="safety", name="cancelled"),
    ]

    result = evaluate_capability_suite([policy], receipts)

    assert result["passed"] is False
    assert "trial numbers must be exactly [1, 2, 3]" in " ".join(result["failures"])


def test_overall_gate_requires_ninety_percent_of_cases() -> None:
    policies = [
        CapabilityCasePolicy(
            case_id=f"case-{index}", evidence_tier="mock", repetitions=1, minimum_passes=1
        )
        for index in range(10)
    ]
    receipts = [
        _receipt(policy.case_id, 1, tier="mock", passed=index < 8)
        for index, policy in enumerate(policies)
    ]

    result = evaluate_capability_suite(policies, receipts)

    assert result["case_pass_rate"] == 0.8
    assert result["passed"] is False
    assert "case pass rate 0.8000 < 0.9000" in result["failures"]


def test_critical_policy_configuration_cannot_weaken_repetition_requirement() -> None:
    with pytest.raises(ValueError, match="must pass every repetition"):
        CapabilityCasePolicy(
            case_id="assistant.security.policy",
            evidence_tier="offline",
            critical=True,
            repetitions=3,
            minimum_passes=2,
        )
