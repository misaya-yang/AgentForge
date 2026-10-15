from __future__ import annotations

import json
from types import SimpleNamespace
from typing import Any

import httpx
import pytest
from ai_gateway_contracts.agent_runtime_lease import RuntimeModelLeaseSigner

from src.api.internal import agent_capabilities
from src.services.agent_runtime.capability_catalog import (
    CapabilityCatalogError,
    CapabilityCatalogQuery,
    CapabilityCatalogService,
    HttpCapabilityCatalogClient,
    LocalCapabilityCatalogClient,
)
from src.services.agent_runtime.capability_leases import canonical_json_hash
from src.services.agent_runtime.control_plane import (
    AgentRuntimeControlError,
    AgentRuntimeControlPlane,
)


class _Response:
    def __init__(self, payload: dict[str, Any], status_code: int = 200) -> None:
        self.status_code = status_code
        self.content = json.dumps(payload).encode()
        self._payload = payload

    def json(self) -> dict[str, Any]:
        return self._payload


class _WorkerClient:
    def __init__(self, response: _Response) -> None:
        self.response = response
        self.calls: list[dict[str, Any]] = []

    async def post(self, url: str, **kwargs: Any) -> _Response:
        self.calls.append({"url": url, **kwargs})
        return self.response


class _Database:
    enabled = True

    async def get_user_for_tenant(self, user_id: str, tenant_id: str):
        assert (user_id, tenant_id) == ("user-a", "tenant-a")
        return {"roles": ["user"], "permissions": []}

    async def get_user_roles(self, _user_id: str):
        return ["user"]

    async def get_user_permissions(self, _user_id: str):
        return []

    async def fetchrow(self, _query: str, tenant_id: str):
        assert tenant_id == "tenant-a"
        return {"allowed_tools": None, "blocked_tools": [], "allowed_categories": []}


def _record() -> dict[str, Any]:
    schema = {
        "type": "object",
        "properties": {"query": {"type": "string"}},
        "required": ["query"],
        "additionalProperties": False,
    }
    return {
        "id": "search_knowledge_base",
        "name": "search_knowledge_base",
        "version": None,
        "description": "Search tenant-scoped knowledge.",
        "input_schema": schema,
        "schema_hash": canonical_json_hash(schema),
        "effect": "read",
        "approval": "never",
        "timeout_ms": 10_000,
        "protocol": "internal",
        "kind": "knowledge",
        "category": "retrieval",
        "required_permissions": [],
    }


def _descriptor(record: dict[str, Any] | None = None) -> dict[str, Any]:
    record = record or _record()
    return {
        "schema_version": "capability-descriptor/v2",
        "id": record["id"],
        "name": record["name"],
        "version": "null",
        "description": record["description"],
        "schema_hash": record["schema_hash"],
        "input_schema": record["input_schema"],
        "output_schema": {"type": "object"},
        "effect": record["effect"],
        "approval_policy": record["approval"],
        "execution_mode": "inline",
        "timeout_ms": record["timeout_ms"],
        "tags": [f"kind:{record['kind']}", f"category:{record['category']}"],
        "protocol": record["protocol"],
    }


def _query() -> CapabilityCatalogQuery:
    return CapabilityCatalogQuery.create(
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        model_id="qwen3.7-plus",
        capability_revision=7,
    )


def _service(worker: _WorkerClient) -> CapabilityCatalogService:
    record = _record()
    return CapabilityCatalogService(
        database=_Database(),
        worker_url="http://worker.test:8095",
        internal_token="internal-token",
        worker_client=worker,
        catalog_loader=lambda: (1, (record,)),
    )


@pytest.mark.asyncio
async def test_local_service_validates_v2_and_projects_once_without_gateway_http() -> None:
    worker = _WorkerClient(
        _Response(
            {
                "schema_version": "capability-catalog/v2",
                "capability_revision": 7,
                "capabilities": [_descriptor()],
            }
        )
    )
    result = await LocalCapabilityCatalogClient(_service(worker)).fetch_catalog(_query())

    assert worker.calls[0]["url"] == (
        "http://worker.test:8095/internal/v2/capabilities/catalog"
    )
    assert worker.calls[0]["json"] == {
        "schema_version": "capability-catalog/v2",
        "tenant_id": "tenant-a",
        "user_id": "user-a",
        "session_id": "session-a",
        "capability_revision": 7,
    }
    assert [item["name"] for item in result["tools"]] == [
        "search_knowledge_base"
    ]
    assert result["tools"][0]["schema"] == _record()["input_schema"]
    assert result["mcp"] == []
    assert result["deferred"] == []


@pytest.mark.asyncio
async def test_default_web_catalog_hides_internal_todo_approval_loop() -> None:
    todo = {
        **_record(),
        "id": "todo_write",
        "name": "todo_write",
        "description": "Replace the session task list.",
        "effect": "write",
        "approval": "always",
        "kind": "tool",
        "category": "utility",
    }
    worker = _WorkerClient(
        _Response(
            {
                "schema_version": "capability-catalog/v2",
                "capability_revision": 7,
                "capabilities": [_descriptor(), _descriptor(todo)],
            }
        )
    )
    service = CapabilityCatalogService(
        database=_Database(),
        worker_url="http://worker.test:8095",
        internal_token="internal-token",
        worker_client=worker,
        catalog_loader=lambda: (1, (_record(), todo)),
    )

    default_catalog = await service.resolve(_query())
    explicit_catalog = await service.resolve(
        CapabilityCatalogQuery.create(
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            model_id="qwen3.7-plus",
            capability_revision=7,
            capability_allowlist=[{"id": "todo_write"}],
        )
    )

    assert default_catalog["deferred"] == []
    assert [item["id"] for item in explicit_catalog["deferred"]] == ["todo_write"]


@pytest.mark.asyncio
async def test_internal_route_and_local_client_share_exact_catalog_service(monkeypatch) -> None:
    worker = _WorkerClient(
        _Response(
            {
                "schema_version": "capability-catalog/v2",
                "capability_revision": 7,
                "capabilities": [_descriptor()],
            }
        )
    )
    service = _service(worker)
    local = await LocalCapabilityCatalogClient(service).fetch_catalog(_query())
    request = SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(agent_capability_catalog_service=service)
        )
    )
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "internal-token")
    routed = await agent_capabilities.broker_agent_capability_catalog(
        agent_capabilities.CapabilityCatalogRequest(
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            model_id="qwen3.7-plus",
            capability_revision=7,
        ),
        request,
        x_ai_platform_internal_token="internal-token",
        x_ai_tenant_id="tenant-a",
        x_ai_user_id="user-a",
        x_ai_session_id="session-a",
    )
    assert routed == local


@pytest.mark.asyncio
async def test_catalog_policy_is_identical_for_all_launch_entrypoints() -> None:
    worker = _WorkerClient(
        _Response(
            {
                "schema_version": "capability-catalog/v2",
                "capability_revision": 7,
                "capabilities": [_descriptor()],
            }
        )
    )
    client = LocalCapabilityCatalogClient(_service(worker))
    results = {
        entrypoint: await client.fetch_catalog(_query())
        for entrypoint in (
            "assistant",
            "responses",
            "studio_preview",
            "published_agent",
        )
    }
    assert len({json.dumps(value, sort_keys=True) for value in results.values()}) == 1


@pytest.mark.asyncio
async def test_service_fails_closed_on_identity_worker_and_schema_errors() -> None:
    class DisabledDatabase:
        enabled = False

    worker = _WorkerClient(_Response({}, status_code=500))
    unavailable = CapabilityCatalogService(
        database=DisabledDatabase(),
        worker_url="http://worker.test:8095",
        internal_token="internal-token",
        worker_client=worker,
        catalog_loader=lambda: (1, (_record(),)),
    )
    with pytest.raises(CapabilityCatalogError, match="IDENTITY_UNAVAILABLE"):
        await unavailable.resolve(_query())
    assert worker.calls == []

    service = _service(worker)
    with pytest.raises(CapabilityCatalogError, match="WORKER_UNAVAILABLE"):
        await service.resolve(_query())

    tampered = _descriptor()
    tampered["input_schema"] = {"type": "object"}
    worker.response = _Response(
        {
            "schema_version": "capability-catalog/v2",
            "capability_revision": 7,
            "capabilities": [tampered],
        }
    )
    with pytest.raises(CapabilityCatalogError, match="CATALOG_INVALID"):
        await service.resolve(_query())


@pytest.mark.parametrize(
    "base_url",
    [
        "http://gateway:8080/internal/v2/agent-capabilities",
        "http://localhost:8080/internal/v2/agent-capabilities",
        "http://127.0.0.1:8080/internal/v2/agent-capabilities",
    ],
)
def test_http_catalog_adapter_rejects_gateway_and_loopback(base_url: str) -> None:
    with pytest.raises(ValueError, match="external capability catalog"):
        HttpCapabilityCatalogClient(
            base_url=base_url,
            internal_token="internal-token",
            http_client=SimpleNamespace(),
        )


@pytest.mark.asyncio
async def test_control_ignores_legacy_gateway_env_and_fails_named_degraded(
    monkeypatch,
) -> None:
    calls: list[str] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        calls.append(str(request.url))
        return httpx.Response(500, request=request)

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    monkeypatch.setenv(
        "AI_PLATFORM_CAPABILITY_PLANE_URL",
        "http://gateway:8080/internal/v2/agent-capabilities",
    )
    plane = AgentRuntimeControlPlane(
        database=SimpleNamespace(),
        model_service=SimpleNamespace(),
        provider_service=SimpleNamespace(),
        assignment_store=SimpleNamespace(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=client,
    )
    readonly = plane._readonly_capability_payload(
        None, tenant_id="tenant-a", capability_revision=7
    )
    try:
        with pytest.raises(
            AgentRuntimeControlError,
            match="AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_DEGRADED",
        ):
            await plane._fetch_capability_catalog(
                readonly,
                tenant_id="tenant-a",
                user_id="user-a",
                session_id="session-a",
                model_id="qwen3.7-plus",
                capability_revision=7,
            )
    finally:
        await client.aclose()
    assert plane.capability_plane_url == ""
    assert calls == []
