"""Result-level capability receipts for Assistant and Knowledge live checks.

The existing golden gate remains the deterministic trajectory regression gate.
This module is intentionally smaller: it prevents live capability evidence from
counting a non-empty response or a missing ``run_error`` as task success.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from typing import Any

EVIDENCE_TIERS = frozenset({"offline", "mock", "local_live", "real_provider", "browser"})
CHECK_KINDS = frozenset({"outcome", "safety", "protocol", "proxy"})
SUBSTANTIVE_CHECK_KINDS = frozenset({"outcome", "safety"})
PROXY_ONLY_CHECK_NAMES = frozenset({"non_empty_output", "no_run_error"})


def _bounded_text(value: Any, *, field: str, max_length: int = 240) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{field} must be a non-empty string")
    normalized = " ".join(value.split()).strip()
    if len(normalized) > max_length:
        raise ValueError(f"{field} exceeds {max_length} characters")
    return normalized


@dataclass(frozen=True)
class CapabilityCheck:
    """One bounded assertion receipt; raw model/tool output is never stored."""

    name: str
    kind: str
    passed: bool
    evidence: str

    def __post_init__(self) -> None:
        object.__setattr__(
            self, "name", _bounded_text(self.name, field="check name", max_length=96)
        )
        if self.kind not in CHECK_KINDS:
            raise ValueError(f"unsupported capability check kind: {self.kind}")
        if not isinstance(self.passed, bool):
            raise ValueError("check passed must be a boolean")
        object.__setattr__(
            self,
            "evidence",
            _bounded_text(self.evidence, field="check evidence", max_length=240),
        )

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> CapabilityCheck:
        unknown = set(value) - {"name", "kind", "passed", "evidence"}
        if unknown:
            raise ValueError(f"unsupported capability check fields: {', '.join(sorted(unknown))}")
        return cls(
            name=value.get("name"),
            kind=value.get("kind"),
            passed=value.get("passed"),
            evidence=value.get("evidence"),
        )


@dataclass(frozen=True)
class CapabilityCasePolicy:
    case_id: str
    evidence_tier: str
    repetitions: int = 3
    minimum_passes: int = 2
    critical: bool = False

    def __post_init__(self) -> None:
        object.__setattr__(
            self, "case_id", _bounded_text(self.case_id, field="case_id", max_length=128)
        )
        if self.evidence_tier not in EVIDENCE_TIERS:
            raise ValueError(f"unsupported evidence tier: {self.evidence_tier}")
        if isinstance(self.repetitions, bool) or not isinstance(self.repetitions, int):
            raise ValueError("repetitions must be an integer")
        if not 1 <= self.repetitions <= 10:
            raise ValueError("repetitions must be between 1 and 10")
        if isinstance(self.minimum_passes, bool) or not isinstance(self.minimum_passes, int):
            raise ValueError("minimum_passes must be an integer")
        if not 1 <= self.minimum_passes <= self.repetitions:
            raise ValueError("minimum_passes must be between 1 and repetitions")
        if not isinstance(self.critical, bool):
            raise ValueError("critical must be a boolean")
        if self.critical and self.minimum_passes != self.repetitions:
            raise ValueError("critical capability cases must pass every repetition")

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> CapabilityCasePolicy:
        unknown = set(value) - {
            "case_id",
            "evidence_tier",
            "repetitions",
            "minimum_passes",
            "critical",
        }
        if unknown:
            raise ValueError(f"unsupported capability policy fields: {', '.join(sorted(unknown))}")
        return cls(
            case_id=value.get("case_id"),
            evidence_tier=value.get("evidence_tier"),
            repetitions=value.get("repetitions", 3),
            minimum_passes=value.get("minimum_passes", 2),
            critical=value.get("critical", False),
        )


@dataclass(frozen=True)
class CapabilityTrialReceipt:
    case_id: str
    evidence_tier: str
    trial: int
    checks: tuple[CapabilityCheck, ...]

    def __post_init__(self) -> None:
        object.__setattr__(
            self, "case_id", _bounded_text(self.case_id, field="case_id", max_length=128)
        )
        if self.evidence_tier not in EVIDENCE_TIERS:
            raise ValueError(f"unsupported evidence tier: {self.evidence_tier}")
        if isinstance(self.trial, bool) or not isinstance(self.trial, int) or self.trial < 1:
            raise ValueError("trial must be a positive integer")
        if not isinstance(self.checks, tuple) or not self.checks:
            raise ValueError("trial checks must be a non-empty tuple")

    @property
    def has_substantive_check(self) -> bool:
        return any(
            check.kind in SUBSTANTIVE_CHECK_KINDS and check.name not in PROXY_ONLY_CHECK_NAMES
            for check in self.checks
        )

    @property
    def passed(self) -> bool:
        return self.has_substantive_check and all(check.passed for check in self.checks)

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> CapabilityTrialReceipt:
        unknown = set(value) - {"case_id", "evidence_tier", "trial", "checks"}
        if unknown:
            raise ValueError(f"unsupported capability trial fields: {', '.join(sorted(unknown))}")
        checks = value.get("checks")
        if not isinstance(checks, list):
            raise ValueError("trial checks must be a list")
        return cls(
            case_id=value.get("case_id"),
            evidence_tier=value.get("evidence_tier"),
            trial=value.get("trial"),
            checks=tuple(CapabilityCheck.from_dict(item) for item in checks),
        )


def _duplicates(values: Iterable[str]) -> list[str]:
    seen: set[str] = set()
    duplicates: set[str] = set()
    for value in values:
        if value in seen:
            duplicates.add(value)
        seen.add(value)
    return sorted(duplicates)


def evaluate_capability_suite(
    policies: Sequence[CapabilityCasePolicy],
    receipts: Sequence[CapabilityTrialReceipt],
    *,
    minimum_case_pass_rate: float = 0.9,
) -> dict[str, Any]:
    """Gate live evidence without accepting proxy-only success receipts."""

    if not policies:
        raise ValueError("capability suite requires at least one case policy")
    if isinstance(minimum_case_pass_rate, bool) or not isinstance(
        minimum_case_pass_rate, int | float
    ):
        raise ValueError("minimum_case_pass_rate must be numeric")
    if not 0.0 <= float(minimum_case_pass_rate) <= 1.0:
        raise ValueError("minimum_case_pass_rate must be in [0, 1]")

    duplicate_policies = _duplicates(policy.case_id for policy in policies)
    if duplicate_policies:
        raise ValueError(f"duplicate capability policies: {', '.join(duplicate_policies)}")

    policy_by_id = {policy.case_id: policy for policy in policies}
    validation_errors: list[str] = []
    unknown_receipts = sorted({receipt.case_id for receipt in receipts} - set(policy_by_id))
    if unknown_receipts:
        validation_errors.append("receipts without policies: " + ", ".join(unknown_receipts))

    by_case: dict[str, list[CapabilityTrialReceipt]] = {case_id: [] for case_id in policy_by_id}
    for receipt in receipts:
        if receipt.case_id in by_case:
            by_case[receipt.case_id].append(receipt)

    case_results: list[dict[str, Any]] = []
    passing_cases = 0
    critical_failures: list[str] = []
    tier_totals = {tier: {"cases": 0, "passed": 0, "trials": 0} for tier in EVIDENCE_TIERS}

    for policy in policies:
        case_receipts = sorted(by_case[policy.case_id], key=lambda receipt: receipt.trial)
        case_errors: list[str] = []
        trial_numbers = [receipt.trial for receipt in case_receipts]
        if len(case_receipts) != policy.repetitions:
            case_errors.append(
                f"expected {policy.repetitions} receipts, observed {len(case_receipts)}"
            )
        expected_trials = list(range(1, policy.repetitions + 1))
        if trial_numbers != expected_trials:
            case_errors.append(f"trial numbers must be exactly {expected_trials}")
        if any(receipt.evidence_tier != policy.evidence_tier for receipt in case_receipts):
            case_errors.append("receipt evidence tier does not match policy")
        proxy_only = [
            receipt.trial for receipt in case_receipts if not receipt.has_substantive_check
        ]
        if proxy_only:
            case_errors.append(
                "proxy-only trials cannot prove capability: "
                + ", ".join(str(value) for value in proxy_only)
            )

        observed_passes = sum(receipt.passed for receipt in case_receipts)
        case_passed = not case_errors and observed_passes >= policy.minimum_passes
        if case_passed:
            passing_cases += 1
        elif policy.critical:
            critical_failures.append(policy.case_id)

        tier = tier_totals[policy.evidence_tier]
        tier["cases"] += 1
        tier["passed"] += int(case_passed)
        tier["trials"] += len(case_receipts)
        case_results.append(
            {
                "case_id": policy.case_id,
                "evidence_tier": policy.evidence_tier,
                "critical": policy.critical,
                "repetitions": policy.repetitions,
                "minimum_passes": policy.minimum_passes,
                "observed_passes": observed_passes,
                "passed": case_passed,
                "validation_errors": case_errors,
                "trials": [
                    {
                        "trial": receipt.trial,
                        "passed": receipt.passed,
                        "checks": [
                            {
                                "name": check.name,
                                "kind": check.kind,
                                "passed": check.passed,
                                "evidence": check.evidence,
                            }
                            for check in receipt.checks
                        ],
                    }
                    for receipt in case_receipts
                ],
            }
        )

    case_pass_rate = passing_cases / len(policies)
    failures = list(validation_errors)
    if critical_failures:
        failures.append("critical capability failures: " + ", ".join(critical_failures))
    if case_pass_rate < float(minimum_case_pass_rate):
        failures.append(
            f"case pass rate {case_pass_rate:.4f} < {float(minimum_case_pass_rate):.4f}"
        )
    for result in case_results:
        if result["validation_errors"]:
            failures.append(f"{result['case_id']}: " + "; ".join(result["validation_errors"]))

    return {
        "schema_version": "assistant-capability-gate/v1",
        "status": "pass" if not failures else "fail",
        "passed": not failures,
        "case_count": len(policies),
        "passing_cases": passing_cases,
        "case_pass_rate": round(case_pass_rate, 4),
        "minimum_case_pass_rate": float(minimum_case_pass_rate),
        "critical_failures": critical_failures,
        "failures": failures,
        "evidence_tiers": {
            tier: values
            for tier, values in sorted(tier_totals.items())
            if values["cases"] or values["trials"]
        },
        "cases": case_results,
    }
