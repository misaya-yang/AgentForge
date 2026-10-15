"""Tests for service access control."""

import pytest

from src.core.auth.service_access import (
    ServiceAccessMode,
    ServiceAccessPolicy,
    evaluate_service_access,
    normalize_service_candidates,
    normalize_service_scope,
    service_access_policy_from_metadata,
    service_scope_matches,
)


class TestNormalizeServiceScope:
    def test_comma_separated(self):
        assert normalize_service_scope("agent, flash, gpt") == ["agent", "flash", "gpt"]

    def test_list(self):
        assert normalize_service_scope(["Agent", "Flash"]) == ["agent", "flash"]

    def test_tuple(self):
        assert normalize_service_scope(("A", "B")) == ["a", "b"]

    def test_none(self):
        assert normalize_service_scope(None) == []

    def test_empty_string(self):
        assert normalize_service_scope("") == []

    def test_deduplication(self):
        assert normalize_service_scope("a,a,b") == ["a", "b"]

    def test_non_iterable(self):
        assert normalize_service_scope(42) == []


class TestServiceScopeMatches:
    def test_exact_match(self):
        assert service_scope_matches(["agent"], ["agent"])

    def test_no_match(self):
        assert not service_scope_matches(["agent"], ["flash"])

    def test_global_wildcard(self):
        assert service_scope_matches(["*"], ["any-service"])

    def test_prefix_wildcard(self):
        assert service_scope_matches(["agent-*"], ["agent-prod", "agent-dev"])

    def test_prefix_wildcard_no_match(self):
        assert not service_scope_matches(["agent-*"], ["flash"])

    def test_empty_scope_matches_all(self):
        assert service_scope_matches([], ["any-service"])

    def test_empty_candidates_no_match(self):
        assert not service_scope_matches(["agent"], [])


class TestServiceAccessPolicyFromMetadata:
    def test_no_metadata(self):
        policy = service_access_policy_from_metadata(None)
        assert policy.mode == ServiceAccessMode.ALL

    def test_allowlist_auto_detected(self):
        metadata = {"service_access": {"allowed_services": "agent,flash"}}
        policy = service_access_policy_from_metadata(metadata)
        assert policy.mode == ServiceAccessMode.ALLOWLIST
        assert "agent" in policy.allowed_services

    def test_explicit_mode(self):
        metadata = {"service_access": {"mode": "all"}}
        policy = service_access_policy_from_metadata(metadata)
        assert policy.mode == ServiceAccessMode.ALL

    def test_denied_services(self):
        metadata = {"service_access": {"denied_services": "dangerous"}}
        policy = service_access_policy_from_metadata(metadata)
        assert "dangerous" in policy.denied_services


class TestEvaluateServiceAccess:
    def test_all_mode_allows(self):
        policy = ServiceAccessPolicy(mode=ServiceAccessMode.ALL)
        allowed, reason = evaluate_service_access(policy, ["agent"])
        assert allowed
        assert reason == "allowed"

    def test_denied_by_policy(self):
        policy = ServiceAccessPolicy(
            mode=ServiceAccessMode.ALL, denied_services=("agent",)
        )
        allowed, reason = evaluate_service_access(policy, ["agent"])
        assert not allowed
        assert reason == "denied_by_user_policy"

    def test_allowlist_allows_matching(self):
        policy = ServiceAccessPolicy(
            mode=ServiceAccessMode.ALLOWLIST, allowed_services=("agent", "flash")
        )
        allowed, reason = evaluate_service_access(policy, ["agent"])
        assert allowed

    def test_allowlist_denies_non_matching(self):
        policy = ServiceAccessPolicy(
            mode=ServiceAccessMode.ALLOWLIST, allowed_services=("agent",)
        )
        allowed, reason = evaluate_service_access(policy, ["flash"])
        assert not allowed
        assert reason == "not_in_user_allowlist"

    def test_empty_allowlist_denies(self):
        policy = ServiceAccessPolicy(
            mode=ServiceAccessMode.ALLOWLIST, allowed_services=()
        )
        allowed, reason = evaluate_service_access(policy, ["agent"])
        assert not allowed
        assert reason == "user_allowlist_empty"


class TestNormalizeServiceCandidates:
    def test_normalizes_and_dedupes(self):
        result = normalize_service_candidates(["Agent", "agent", "Flash"])
        assert result == ("agent", "flash")

    def test_empty(self):
        assert normalize_service_candidates([]) == ()
