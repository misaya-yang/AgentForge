"""Current actor lookup: signed caller, disabled users and revoked roles."""
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_gateway_core.auth.gateway_secret import GatewaySecret
from fastapi import FastAPI

from src.api.internal.agent_capabilities import router
from src.api.internal.knowledge_actor import _verifier


@pytest.mark.parametrize("status,roles,expected", [("active", ["admin"], 200), ("active", [], 200), ("disabled", ["admin"], 403), (None, [], 403)])
async def test_actor_lookup_uses_current_roles_not_legacy_array(monkeypatch, status, roles, expected):
    token = "test-gateway-actor-internal-secret"
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", token)
    _verifier.cache_clear()
    app = FastAPI()
    app.include_router(router)
    db = SimpleNamespace(get_user_for_tenant=AsyncMock(return_value={
        "user_id": "u", "tenant_id": "t", "status": status, "roles": ["stale_admin"],
        "password_hash": "must-not-leave-gateway", "tier": "normal",
    } if status else None), get_user_roles=AsyncMock(return_value=roles))
    app.state.database = db
    path = "/internal/v2/agent-capabilities/knowledge-actor"
    body = json.dumps({"tenant_id": "t", "user_id": "u"}).encode()
    signer = GatewaySecret(secret=token, caller_service="knowledge-service", audience="gateway")
    headers = {"Content-Type": "application/json", "X-Gateway-Secret": signer.sign(method="POST", path=path, body=body)}
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app), base_url="http://gateway") as client:
        denied = await client.post(path, content=body, headers={"Content-Type": "application/json"})
        assert denied.status_code == 403
        db.get_user_for_tenant.assert_not_awaited()
        response = await client.post(path, content=body, headers=headers)
        assert response.status_code == expected
        if expected == 200:
            assert response.json()["roles"] == roles
            assert set(response.json()) == {"user_id", "tenant_id", "roles", "tier", "status"}
        db.get_user_for_tenant.assert_awaited_once_with("u", "t")
        replay = await client.post(path, content=body, headers=headers)
        assert replay.status_code == 403
    _verifier.cache_clear()
