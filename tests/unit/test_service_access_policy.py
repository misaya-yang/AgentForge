from __future__ import annotations

from src.core.auth.service_access import (
    ServiceAccessMode,
    evaluate_service_access,
    normalize_service_scope,
    service_access_policy_from_metadata,
    service_scope_matches,
)


def test_service_scope_matches_supports_wildcards() -> None:
    scope = normalize_service_scope(["agent-*", "flash"])
    assert service_scope_matches(scope, ["agent-v2"]) is True
    assert service_scope_matches(scope, ["flash"]) is True
    assert service_scope_matches(scope, ["other"]) is False


def test_policy_allowlist_denies_when_not_matched() -> None:
    policy = service_access_policy_from_metadata(
        {"service_access": {"mode": "allowlist", "allowed_services": ["flash"]}}
    )
    allowed, reason = evaluate_service_access(policy, ["agent"])
    assert allowed is False
    assert reason == "not_in_user_allowlist"


def test_policy_denylist_takes_precedence_over_allowlist() -> None:
    policy = service_access_policy_from_metadata(
        {
            "service_access": {
                "mode": "allowlist",
                "allowed_services": ["agent"],
                "denied_services": ["agent"],
            }
        }
    )
    allowed, reason = evaluate_service_access(policy, ["agent"])
    assert allowed is False
    assert reason == "denied_by_user_policy"


def test_policy_defaults_to_all_mode_without_config() -> None:
    policy = service_access_policy_from_metadata({})
    assert policy.mode == ServiceAccessMode.ALL
    allowed, reason = evaluate_service_access(policy, ["agent"])
    assert allowed is True
    assert reason == "allowed"
