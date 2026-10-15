from __future__ import annotations

import pytest

from src.config.settings import Settings
from src.core.auth.permissions import Capability, check_capability, require_capability
from src.core.auth.rbac import RBAC
from ai_gateway_core.exceptions import PermissionDeniedError


def test_agent_invoke_capability_accepts_canonical_permission() -> None:
    rbac = RBAC(role_permissions={})
    decision = check_capability(
        rbac=rbac,
        roles=["conversation:playground:access"],
        permissions=[],
        capability=Capability.AGENT_INVOKE,
    )
    assert decision.allowed is True
    assert decision.matched_permission == "conversation:playground:access"


def test_agent_invoke_capability_accepts_legacy_alias() -> None:
    rbac = RBAC(role_permissions={})
    decision = check_capability(
        rbac=rbac,
        roles=["service:invoke"],
        permissions=[],
        capability=Capability.AGENT_INVOKE,
    )
    assert decision.allowed is True
    assert decision.matched_permission == "service:invoke"


def test_service_config_write_accepts_legacy_manage_alias() -> None:
    rbac = RBAC(role_permissions={})
    decision = check_capability(
        rbac=rbac,
        roles=["service:manage"],
        permissions=[],
        capability=Capability.SERVICE_CONFIG_WRITE,
    )
    assert decision.allowed is True
    assert decision.required_permission == "console:services:edit"


def test_require_capability_raises_when_missing() -> None:
    rbac = RBAC(role_permissions={})
    with pytest.raises(PermissionDeniedError):
        require_capability(
            rbac=rbac,
            roles=["user"],
            permissions=[],
            capability=Capability.AGENT_INVOKE,
        )


def test_manager_role_from_settings_has_service_list_capability() -> None:
    settings = Settings()
    rbac = RBAC(role_permissions=settings.rbac.roles)
    decision = check_capability(
        rbac=rbac,
        roles=["manager"],
        permissions=[],
        capability=Capability.SERVICE_LIST_READ,
    )
    assert decision.allowed is True
