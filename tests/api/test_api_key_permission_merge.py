from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from src.api.deps import get_auth_context, get_user_context
from src.config.settings import Settings


def _build_request() -> SimpleNamespace:
    request = SimpleNamespace()
    request.headers = {"X-API-Key": "gw_test_key_123"}
    request.client = SimpleNamespace(host="127.0.0.1")
    request.app = SimpleNamespace()
    request.app.state = SimpleNamespace()
    request.app.state.database = SimpleNamespace(
        enabled=True,
        get_user=AsyncMock(return_value=None),
        get_api_key=AsyncMock(
            return_value={
                "tenant_id": "tenant_test",
                "user_id": "api_user_1",
                "roles": ["user"],
                "permissions": ["conversation:playground:access"],
                "allowed_services": ["agent"],
                "tier": "normal",
            }
        ),
        get_user_permissions=AsyncMock(return_value=[]),
    )
    request.app.state.redis = None
    request.state = SimpleNamespace()
    return request


@pytest.mark.asyncio
async def test_api_key_permissions_are_merged_into_auth_context() -> None:
    request = _build_request()
    settings = Settings()
    settings.authentication.api_key.enabled = True
    settings.authentication.api_key.header_name = "X-API-Key"
    settings.authentication.jwt.enabled = False

    user = await get_user_context(request, settings=settings)
    auth = await get_auth_context(request, settings=settings)

    assert "conversation:playground:access" in user.roles
    assert "conversation:playground:access" in auth.permissions
    assert "conversation:playground:access" in auth.roles
