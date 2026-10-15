"""Generic session detail exposes only the owned canonical service binding."""
from datetime import datetime
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException

from src.api.v1.sessions import get_session
from src.core.auth.user_resolver import UserContext


@pytest.mark.parametrize("service_id", ["dify-service", "other-service", "", None])
async def test_detail_returns_authoritative_nullable_service_binding(service_id):
    now = datetime(2026, 10, 12)
    session = SimpleNamespace(session_id="s1", user_id="owner", tenant_id="tenant",
                              service_id=service_id, created_at=now, updated_at=now,
                              metadata={"service_id": "spoofed", "title": "kept"}, config={})
    manager = SimpleNamespace(get=AsyncMock(return_value=session))
    user = UserContext(user_id="owner", tenant_id="tenant", is_authenticated=True)
    result = await get_session("s1", session_manager=manager, user=user)
    assert result == {"session_id": "s1", "service_id": service_id, "user_id": "owner",
                      "tenant_id": "tenant", "created_at": now, "updated_at": now,
                      "metadata": {"service_id": "spoofed", "title": "kept"}, "config": {}}
    manager.get.assert_awaited_once_with("s1")


@pytest.mark.parametrize("owner,tenant", [("other", "tenant"), ("owner", "other"), (None, None)])
async def test_detail_service_binding_preserves_ownership_denial(owner, tenant):
    session = None if owner is None else SimpleNamespace(user_id=owner, tenant_id=tenant,
                                                        service_id="private-service")
    manager = SimpleNamespace(get=AsyncMock(return_value=session))
    user = UserContext(user_id="owner", tenant_id="tenant", is_authenticated=True)
    with pytest.raises(HTTPException) as error:
        await get_session("s1", session_manager=manager, user=user)
    assert error.value.status_code == 404
    assert error.value.detail == "session not found"
