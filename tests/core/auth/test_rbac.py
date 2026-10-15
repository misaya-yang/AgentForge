"""Tests for Role-Based Access Control."""

import pytest

from src.core.auth.rbac import RBAC
from ai_gateway_core.exceptions import PermissionDeniedError

ROLE_PERMISSIONS = {
    "guest": ["console:dashboard:view"],
    "user": ["console:dashboard:view", "conversation:playground:access"],
    "admin": ["admin:*"],
    "manager": ["console:dashboard:view", "console:services:view", "console:services:edit"],
}


@pytest.fixture
def rbac():
    return RBAC(ROLE_PERMISSIONS)


class TestPermissionsForRoles:
    def test_single_role(self, rbac):
        perms = rbac.permissions_for_roles(["guest"])
        assert perms == {"console:dashboard:view"}

    def test_multiple_roles_merged(self, rbac):
        perms = rbac.permissions_for_roles(["guest", "user"])
        assert "console:dashboard:view" in perms
        assert "conversation:playground:access" in perms

    def test_unknown_role_ignored(self, rbac):
        perms = rbac.permissions_for_roles(["nonexistent"])
        assert perms == set()

    def test_empty_roles(self, rbac):
        assert rbac.permissions_for_roles([]) == set()


class TestHasPermission:
    def test_exact_match(self, rbac):
        assert rbac.has_permission(["user"], "conversation:playground:access")

    def test_no_match(self, rbac):
        assert not rbac.has_permission(["guest"], "conversation:playground:access")

    def test_admin_wildcard_matches_anything(self, rbac):
        assert rbac.has_permission(["admin"], "console:services:edit")
        assert rbac.has_permission(["admin"], "some:random:permission")

    def test_hierarchical_wildcard(self, rbac):
        custom_rbac = RBAC({"editor": ["console:services:*"]})
        assert custom_rbac.has_permission(["editor"], "console:services:view")
        assert custom_rbac.has_permission(["editor"], "console:services:edit")
        assert not custom_rbac.has_permission(["editor"], "console:dashboard:view")

    def test_direct_permission_in_roles_list(self, rbac):
        assert rbac.has_permission(
            ["conversation:playground:access"], "conversation:playground:access"
        )

    def test_mixed_roles_and_permissions(self, rbac):
        assert rbac.has_permission(["guest", "conversation:playground:access"], "conversation:playground:access")


class TestRequire:
    def test_allowed(self, rbac):
        rbac.require(["user"], "conversation:playground:access")

    def test_denied_raises(self, rbac):
        with pytest.raises(PermissionDeniedError, match="Missing permission"):
            rbac.require(["guest"], "conversation:playground:access")
