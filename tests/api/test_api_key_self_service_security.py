from __future__ import annotations

from unittest.mock import AsyncMock, Mock

import pytest
from fastapi import HTTPException

import src.api.v1.api_keys as api_keys_module
from src.api.v1.api_keys import (
    CreateAPIKeyRequest,
    create_api_key,
    delete_api_key,
    get_api_key_detail,
    list_api_keys,
    revoke_api_key,
)


def _current_user(*, tier: str = "normal") -> dict[str, object]:
    return {
        "user_id": "user-1",
        "tenant_id": "tenant-1",
        "roles": ["user"],
        "tier": tier,
    }


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "scopes",
    [
        pytest.param(["admin"], id="admin-role"),
        pytest.param(["admin:*"], id="admin-wildcard"),
        pytest.param(["console:services:edit"], id="management-capability"),
    ],
)
async def test_self_service_api_key_rejects_privileged_or_management_scopes(
    monkeypatch: pytest.MonkeyPatch,
    scopes: list[str],
) -> None:
    create_key = AsyncMock()
    monkeypatch.setattr(api_keys_module.APIKeyService, "create_api_key", create_key)

    with pytest.raises(HTTPException) as exc_info:
        await create_api_key(
            CreateAPIKeyRequest(name="unprivileged-key", scopes=scopes),
            db=object(),
            current_user=_current_user(),
        )

    assert exc_info.value.status_code == 403
    assert exc_info.value.detail["disallowed_scopes"] == scopes
    create_key.assert_not_awaited()


@pytest.mark.asyncio
async def test_self_service_api_key_ignores_forged_tier_and_uses_caller_tier(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    create_key = AsyncMock(return_value={"key_id": "ak_test"})
    clear_cache = Mock()
    monkeypatch.setattr(api_keys_module.APIKeyService, "create_api_key", create_key)
    monkeypatch.setattr(api_keys_module, "clear_service_access_constraint_cache", clear_cache)

    request = CreateAPIKeyRequest(
        name="unprivileged-key",
        scopes=["knowledge:read"],
        tier="admin",
    )
    result = await create_api_key(
        request,
        db=object(),
        current_user=_current_user(tier="normal"),
    )

    assert result == {"key_id": "ak_test"}
    assert create_key.await_args.kwargs["scopes"] == ["knowledge:read"]
    assert create_key.await_args.kwargs["tier"] == "normal"
    assert create_key.await_args.kwargs["tier"] != request.tier
    clear_cache.assert_called_once_with()


@pytest.mark.asyncio
async def test_api_key_management_routes_always_apply_authenticated_tenant(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    list_keys = AsyncMock(return_value=[])
    get_key = AsyncMock(
        return_value={"key_id": "ak_test", "user_id": "user-1", "tenant_id": "tenant-1"}
    )
    revoke_key = AsyncMock(return_value=True)
    delete_key = AsyncMock(return_value=True)
    monkeypatch.setattr(api_keys_module.APIKeyService, "list_api_keys", list_keys)
    monkeypatch.setattr(api_keys_module.APIKeyService, "get_api_key", get_key)
    monkeypatch.setattr(api_keys_module.APIKeyService, "revoke_api_key", revoke_key)
    monkeypatch.setattr(api_keys_module.APIKeyService, "delete_api_key", delete_key)
    monkeypatch.setattr(api_keys_module, "clear_service_access_constraint_cache", Mock())
    admin = {**_current_user(), "roles": ["admin"]}

    await list_api_keys(db=object(), current_user=admin)
    await get_api_key_detail("ak_test", db=object(), current_user=admin)
    await revoke_api_key("ak_test", db=object(), current_user=admin)
    await delete_api_key("ak_test", db=object(), current_user=admin)

    assert list_keys.await_args.kwargs == {
        "user_id": None,
        "tenant_id": "tenant-1",
        "include_inactive": False,
    }
    assert get_key.await_args.kwargs == {"tenant_id": "tenant-1"}
    assert revoke_key.await_args.kwargs == {
        "user_id": None,
        "tenant_id": "tenant-1",
    }
    assert delete_key.await_args.kwargs == {
        "user_id": None,
        "tenant_id": "tenant-1",
    }
