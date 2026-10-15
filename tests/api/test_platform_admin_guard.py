from __future__ import annotations

from types import SimpleNamespace

import pytest
from fastapi import HTTPException

from src.api.deps import AuthContext, require_platform_admin
from src.core.auth.permissions import Capability


def _request() -> SimpleNamespace:
    return SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(dispatcher=None, settings=None)),
        state=SimpleNamespace(request_id="trace"),
    )


def test_tenant_admin_is_not_platform_admin() -> None:
    auth = AuthContext(
        user_id="admin-a",
        tenant_id="tenant-a",
        roles=["admin"],
        permissions=["admin:*", "console:metrics:view"],
        is_authenticated=True,
    )
    with pytest.raises(HTTPException) as exc_info:
        require_platform_admin(_request(), auth, Capability.GATEWAY_METRICS_READ)
    assert exc_info.value.status_code == 403
    assert exc_info.value.detail["reason"] == "platform_admin_required"


def test_platform_admin_passes_guard() -> None:
    auth = AuthContext(
        user_id="ops",
        tenant_id="platform",
        roles=["platform_admin"],
        permissions=["console:metrics:view"],
        is_authenticated=True,
    )
    require_platform_admin(_request(), auth, Capability.GATEWAY_METRICS_READ)
