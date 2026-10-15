"""Tests for capability-based permission checks."""

from pathlib import Path

import pytest
from ai_gateway_core.exceptions import PermissionDeniedError

from src.core.auth.permissions import (
    Capability,
    CapabilityDecision,
    accepted_permissions,
    build_permission_denied_detail,
    canonical_permission,
    check_capability,
    require_capability,
)
from src.core.auth.rbac import RBAC

ROLE_PERMISSIONS = {
    "user": ["console:dashboard:view", "conversation:playground:access"],
    "viewer": ["console:dashboard:view", "console:services:view"],
    "admin": ["admin:*"],
}


@pytest.fixture
def rbac():
    return RBAC(ROLE_PERMISSIONS)


class TestCanonicalPermission:
    def test_agent_invoke(self):
        assert canonical_permission(Capability.AGENT_INVOKE) == "conversation:playground:access"

    def test_service_list_read(self):
        assert canonical_permission(Capability.SERVICE_LIST_READ) == "console:services:view"

    def test_service_config_write(self):
        assert canonical_permission(Capability.SERVICE_CONFIG_WRITE) == "console:services:edit"

    def test_all_canonical_permissions_are_seeded_by_migrations(self):
        repo_root = Path(__file__).resolve().parents[3]
        seed_sql = "\n".join(
            path.read_text()
            for path in [
                repo_root / "database/migrations/005_account_permission_system.sql",
                repo_root / "database/migrations/063_eval_console_permissions.sql",
                repo_root / "database/migrations/068_gateway_console_capability_permissions.sql",
            ]
        )

        for capability in Capability:
            assert canonical_permission(capability) in seed_sql


class TestAcceptedPermissions:
    def test_includes_canonical_and_legacy(self):
        perms = accepted_permissions(Capability.AGENT_INVOKE)
        assert "conversation:playground:access" in perms
        assert "service:invoke" in perms

    def test_canonical_is_first(self):
        perms = accepted_permissions(Capability.SERVICE_LIST_READ)
        assert perms[0] == "console:services:view"

    def test_metrics_read_excludes_dashboard_view_alias(self):
        perms = accepted_permissions(Capability.GATEWAY_METRICS_READ)
        assert "console:metrics:view" in perms
        assert "console:dashboard:view" not in perms

    @pytest.mark.parametrize(
        ("read_capability", "write_permission"),
        [
            (Capability.GATEWAY_PROVIDER_CONFIG_READ, "console:providers:edit"),
            (Capability.GATEWAY_SKILL_READ, "console:skills:edit"),
            (Capability.GATEWAY_MCP_READ, "console:mcp:edit"),
        ],
    )
    def test_gateway_write_permissions_imply_matching_read(
        self,
        read_capability: Capability,
        write_permission: str,
    ):
        assert write_permission in accepted_permissions(read_capability)


class TestCheckCapability:
    def test_allowed_via_role(self, rbac):
        decision = check_capability(
            rbac=rbac, roles=["user"], permissions=None, capability=Capability.AGENT_INVOKE
        )
        assert decision.allowed
        assert decision.matched_permission == "conversation:playground:access"

    def test_denied(self, rbac):
        decision = check_capability(
            rbac=rbac, roles=["viewer"], permissions=None, capability=Capability.AGENT_INVOKE
        )
        assert not decision.allowed
        assert decision.matched_permission is None

    def test_allowed_via_legacy_alias(self, rbac):
        decision = check_capability(
            rbac=rbac,
            roles=["service:invoke"],
            permissions=None,
            capability=Capability.AGENT_INVOKE,
        )
        assert decision.allowed
        assert decision.matched_permission == "service:invoke"

    def test_allowed_via_admin_wildcard(self, rbac):
        decision = check_capability(
            rbac=rbac, roles=["admin"], permissions=None, capability=Capability.SERVICE_CONFIG_WRITE
        )
        assert decision.allowed

    def test_permissions_param_merged(self, rbac):
        decision = check_capability(
            rbac=rbac,
            roles=["viewer"],
            permissions=["conversation:playground:access"],
            capability=Capability.AGENT_INVOKE,
        )
        assert decision.allowed

    def test_metrics_read_denied_via_dashboard_view_role(self, rbac):
        decision = check_capability(
            rbac=rbac,
            roles=["user"],
            permissions=None,
            capability=Capability.GATEWAY_METRICS_READ,
        )
        assert not decision.allowed
        assert decision.matched_permission is None


class TestRequireCapability:
    def test_allowed_returns_decision(self, rbac):
        decision = require_capability(
            rbac=rbac, roles=["user"], permissions=None, capability=Capability.AGENT_INVOKE
        )
        assert isinstance(decision, CapabilityDecision)
        assert decision.allowed

    def test_denied_raises(self, rbac):
        with pytest.raises(PermissionDeniedError, match="Missing capability"):
            require_capability(
                rbac=rbac, roles=["viewer"], permissions=None, capability=Capability.AGENT_INVOKE
            )


class TestBuildPermissionDeniedDetail:
    def test_structure(self):
        detail = build_permission_denied_detail(
            capability=Capability.AGENT_INVOKE, trace_id="abc"
        )
        assert detail["required_capability"] == "AgentInvoke"
        assert detail["trace_id"] == "abc"
        assert "conversation:playground:access" in detail["accepted_permissions"]

    def test_custom_message(self):
        detail = build_permission_denied_detail(
            capability=Capability.AGENT_INVOKE, message="Custom denied"
        )
        assert detail["message"] == "Custom denied"
