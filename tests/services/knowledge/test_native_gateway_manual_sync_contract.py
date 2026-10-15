"""Frozen native production requests -> Python signed middleware -> manual lifecycle.

Fixtures are emitted by native ``hybrid_contract_tests`` using the production
request constructor and signer. Clock, database, actor and provider are controlled;
no listener, live service, provider, Redis or PostgreSQL is used.
"""

import copy
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_gateway_core.auth import gateway_secret as signing
from ai_gateway_core.auth.gateway_secret import GatewaySecret, InMemoryReplayStore
from fastapi import FastAPI
from knowledge_service.api.routes.confluence_manual_sync import router
from knowledge_service.api.routes.knowledge import get_dataset_sources
from knowledge_service.auth import GatewaySecretAuthMiddleware

from tests.services.knowledge.test_confluence_manual_workflow import TASK, make_workflow, settle

FIXTURE = json.loads(
    (Path(__file__).parents[2] / "fixtures/knowledge_hybrid_manual_sync.json").read_text()
)


def application(monkeypatch):
    _, service, database, user, state = make_workflow(monkeypatch)
    app = FastAPI()
    app.include_router(router, prefix="/api/v1")
    app.add_api_route(
        "/api/v1/knowledge/{dataset_id}/sources", get_dataset_sources, methods=["GET"]
    )
    app.state.confluence_sync_service = service
    app.state.knowledge_service = service.knowledge_service
    app.state.knowledge_service.db = database
    database.count_documents_by_source_type = AsyncMock(return_value={"confluence": 2})
    app.state.settings = SimpleNamespace(app=SimpleNamespace(allow_anonymous=False))
    app.add_middleware(
        GatewaySecretAuthMiddleware,
        gateway_secret=GatewaySecret(
            secret=FIXTURE["secret"],
            version="v2",
            key_id="local",
            keys={"local": FIXTURE["secret"]},
            replay_store=InMemoryReplayStore(),
            caller_service="gateway",
            audience="knowledge-service",
            allowed_path_prefixes=("/api/v1",),
        ),
        allow_anonymous=False,
    )
    return app, service, database, user, state


async def send(client, request, monkeypatch):
    # Freeze to the actual native producer's timestamp, not a reimplemented HMAC.
    timestamp = int(request["headers"]["x-gateway-secret"].split(":")[3])
    monkeypatch.setattr(signing, "_epoch_ms", lambda: timestamp)
    return await client.request(
        request["method"],
        request["path"] + ("?" + request["query"] if request["query"] else ""),
        content=request["body"].encode(),
        headers=request["headers"],
    )


@pytest.mark.asyncio
async def test_native_signed_sources_submit_and_exact_terminal_readback(monkeypatch):
    app, service, database, _, _ = application(monkeypatch)
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="https://knowledge.example.invalid"
    ) as client:
        sources = await send(client, FIXTURE["native_requests"][0], monkeypatch)
        assert sources.status_code == 200, sources.text
        binding = next(b for b in sources.json()["confluence_bindings"] if b["binding_id"] == "first")
        assert binding["manual_sync_available"] is True
        assert binding["current_sync_task"] is None
        accepted = await send(client, FIXTURE["native_requests"][1], monkeypatch)
        assert accepted.status_code == 202, accepted.text
        assert accepted.json()["task_id"] == TASK
        await settle(service)
        readback = await send(client, FIXTURE["native_requests"][2], monkeypatch)
        assert readback.status_code == 200, readback.text
        assert readback.json()["task_id"] == TASK
        assert readback.json()["binding_id"] == "first"
        assert readback.json()["status"] == "completed"
        assert "first" not in database.pages and "second" in database.pages
        assert database.document is not None
        replay = await send(client, FIXTURE["native_requests"][1], monkeypatch)
        assert replay.status_code == 401
        assert len(database.tasks) == 1
    await service.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("ineligible", ["viewer", "inactive_connection", "other_owner"])
async def test_native_signed_sources_hide_ineligible_manual_action(monkeypatch, ineligible):
    app, service, database, _, state = application(monkeypatch)
    if ineligible == "viewer":
        state["editor"] = False
    elif ineligible == "inactive_connection":
        database.connection["status"] = "inactive"
    else:
        database.connection["created_by"] = "other"
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="https://knowledge.example.invalid"
    ) as client:
        response = await send(client, FIXTURE["native_requests"][0], monkeypatch)
    assert response.status_code == 200
    assert response.json()["connector_status"] == "available"
    assert not any(b["manual_sync_available"] for b in response.json()["confluence_bindings"])
    await service.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("tamper", ["body", "path", "method", "tenant", "roles", "query"])
async def test_native_signed_mutation_tamper_rejected_before_admission(monkeypatch, tamper):
    app, service, database, _, _ = application(monkeypatch)
    request = copy.deepcopy(FIXTURE["native_requests"][1])
    if tamper == "body":
        request["body"] = json.dumps({"request_id": "9f9e06b8-ab46-41a6-b2bc-372fb94634c9"})
    elif tamper == "path":
        request["path"] = request["path"].replace("/first/", "/second/")
    elif tamper == "method":
        request["method"] = "PUT"
    elif tamper == "query":
        request["query"] = "changed=1"
    else:
        request["headers"]["x-tenant-id" if tamper == "tenant" else "x-user-roles"] = "other"
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="https://knowledge.example.invalid"
    ) as client:
        response = await send(client, request, monkeypatch)
    assert response.status_code == 401
    assert not database.tasks
    await service.close()
