"""Real signed envelope: body/actor/path tampering and replay cannot delete."""
import json
import time

import httpx
import pytest
from ai_gateway_core.auth.gateway_secret import GatewaySecret
from ai_gateway_core.auth.gateway_secret_middleware import GatewaySecretAuthMiddleware
from fastapi import FastAPI, Request
from knowledge_service.api.deletion_confirmation import require_deletion_confirmation
from knowledge_service.core.auth.user_resolver import UserContext


@pytest.mark.parametrize("failure", [None, "unsigned", "actor", "tenant", "dataset", "action", "expired", "body", "replay"])
async def test_confirmation_binding_and_replay(failure):
    secret = GatewaySecret(secret="test-internal-confirmation-secret", audience="knowledge-service")
    app = FastAPI()
    calls = []

    @app.delete("/api/v1/knowledge/datasets/{dataset_id}")
    async def delete(dataset_id: str, request: Request):
        payload = await request.json()
        user = UserContext(user_id="u", tenant_id="t", is_authenticated=True)
        require_deletion_confirmation(request, user, dataset_id, payload.get("_gateway_delete_confirmation"))
        calls.append(dataset_id)
        return {"deleted": True}

    app.add_middleware(GatewaySecretAuthMiddleware, gateway_secret=secret, allow_anonymous=True)
    claims = {"action": "dataset.delete", "dataset_id": "kb", "user_id": "u", "tenant_id": "t", "expires_at": int(time.time()) + 60}
    if failure in {"actor", "tenant", "dataset", "action", "expired"}:
        key = {"actor": "user_id", "tenant": "tenant_id", "dataset": "dataset_id", "action": "action", "expired": "expires_at"}[failure]
        claims[key] = 0 if failure == "expired" else "wrong"
    path = "/api/v1/knowledge/datasets/kb"
    body = json.dumps({"_gateway_delete_confirmation": claims}).encode()
    headers = {"Content-Type": "application/json"}
    if failure != "unsigned":
        headers["X-Gateway-Secret"] = secret.sign(method="DELETE", path=path, body=body)
    if failure == "body":
        body = body.replace(b'"kb"', b'"other"')
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app), base_url="http://ks") as client:
        response = await client.request("DELETE", path, content=body, headers=headers)
        if failure == "replay":
            assert response.status_code == 200
            response = await client.request("DELETE", path, content=body, headers=headers)
    assert response.status_code == (200 if failure is None else 401 if failure in {"body", "replay"} else 403)
    assert calls == (["kb"] if failure in {None, "replay"} else [])
