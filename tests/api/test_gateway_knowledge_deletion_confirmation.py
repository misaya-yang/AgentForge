"""Account confirmation stays in Gateway across both public Knowledge aliases."""
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from fastapi import FastAPI
from starlette.responses import JSONResponse

from src.api.deps import get_rate_limiter, get_user_context
from src.api.v1 import _proxy_utils, kb_tools, knowledge
from src.core.auth.password import hash_password
from src.core.auth.user_resolver import UserContext


@pytest.mark.parametrize("alias", ["knowledge", "kb-tools"])
async def test_gateway_checks_tenant_account_and_strips_password(monkeypatch, alias):
    app = FastAPI()
    app.include_router(knowledge.router, prefix="/api/v1")
    app.include_router(kb_tools.router, prefix="/api/v1")
    user = UserContext(user_id="owner", tenant_id="tenant-a", is_authenticated=True)
    app.dependency_overrides[get_user_context] = lambda: user
    app.dependency_overrides[get_rate_limiter] = lambda: None
    database = SimpleNamespace(get_user_for_tenant=AsyncMock(return_value={
        "password_hash": hash_password("test-account-password"),
    }))
    app.state.database = database
    forward = AsyncMock(return_value=JSONResponse({"status": "success"}))
    monkeypatch.setattr(_proxy_utils._proxy, "forward", forward)
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app), base_url="http://gateway") as client:
        response = await client.request("DELETE", f"/api/v1/{alias}/datasets/kb", json={
            "password": "wrong", "reason": "test",
        })
        assert response.status_code == 403
        forward.assert_not_awaited()
        response = await client.request("DELETE", f"/api/v1/{alias}/datasets/kb", json={
            "password": "test-account-password", "reason": "test",
        })
    assert response.status_code == 200
    database.get_user_for_tenant.assert_awaited_with("owner", "tenant-a")
    body = json.loads(forward.call_args.kwargs["body"])
    assert "password" not in body
    assert body["reason"] == "test"
    claims = body["_gateway_delete_confirmation"]
    assert claims["dataset_id"] == "kb"
    assert claims["tenant_id"] == "tenant-a"
    assert claims["user_id"] == "owner"


@pytest.mark.parametrize("method,path", [("DELETE", "datasets/kb"), ("POST", "datasets"), ("PATCH", "datasets/kb")])
async def test_reserved_confirmation_cannot_be_signed(monkeypatch, method, path):
    app = FastAPI()
    app.include_router(knowledge.router, prefix="/api/v1")
    app.dependency_overrides[get_user_context] = lambda: UserContext(user_id="u", tenant_id="t")
    app.dependency_overrides[get_rate_limiter] = lambda: None
    forward = AsyncMock()
    monkeypatch.setattr(_proxy_utils._proxy, "forward", forward)
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app), base_url="http://gateway") as client:
        response = await client.request(method, f"/api/v1/knowledge/{path}", json={
            "_gateway_delete_confirmation": {"action": "dataset.delete"},
        })
    assert response.status_code == 400
    forward.assert_not_awaited()
