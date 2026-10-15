from __future__ import annotations

import hashlib
import json
import os
import time
from types import SimpleNamespace

import pytest
from ai_gateway_contracts.capability_proof import sign_capability_proof
from fastapi import HTTPException

from src.api.internal import agent_capabilities
from src.services.agent_runtime.capability_catalog import CapabilityCatalogService

PROOF_SECRET = "p" * 32


class _CatalogResponse:
    def __init__(self, payload: dict, status_code: int = 200) -> None:
        self.status_code = status_code
        self.content = json.dumps(payload).encode()
        self._payload = payload

    def json(self):
        return self._payload


class _CatalogClient:
    def __init__(self, response: _CatalogResponse) -> None:
        self.response = response
        self.calls: list[dict] = []

    async def post(self, url: str, **kwargs):
        self.calls.append({"url": url, **kwargs})
        return self.response


class _SearchClient:
    def __init__(self, response: _CatalogResponse) -> None:
        self.response = response
        self.calls: list[dict] = []

    async def post(self, url: str, **kwargs):
        self.calls.append({"url": url, **kwargs})
        return self.response


class _CatalogDatabase:
    enabled = True

    async def get_user_for_tenant(self, user_id: str, tenant_id: str):
        assert (user_id, tenant_id) == ("user-a", "tenant-a")
        return {"roles": ["user"], "permissions": [], "tier": "normal"}

    async def get_user_roles(self, _user_id: str):
        return ["user"]

    async def get_user_permissions(self, _user_id: str):
        return []

    async def fetchrow(self, _query: str, tenant_id: str):
        assert tenant_id == "tenant-a"
        return {"allowed_tools": None, "blocked_tools": [], "allowed_categories": []}


def _catalog_descriptor(name: str, *, record: dict) -> dict:
    return {
        "schema_version": "capability-descriptor/v2",
        "id": name,
        "name": name,
        "version": "null",
        "description": record["description"],
        "schema_hash": record["schema_hash"],
        "input_schema": record["input_schema"],
        "output_schema": {"type": "object"},
        "effect": record["effect"],
        "approval_policy": "never" if record["effect"] == "read" else "on_request",
        "execution_mode": "inline",
        "timeout_ms": record["timeout_ms"],
        "tags": [f"kind:{record['kind']}", f"category:{record['category']}"],
        "protocol": record["protocol"],
    }


def _catalog_request() -> agent_capabilities.CapabilityCatalogRequest:
    return agent_capabilities.CapabilityCatalogRequest(
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        model_id="qwen",
        capability_revision=1,
    )


def _catalog_request_context(client: _CatalogClient):
    database = _CatalogDatabase()
    return SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(
                database=database,
                agent_capability_worker_client=client,
                agent_capability_catalog_service=CapabilityCatalogService(
                    database=database,
                    worker_url="http://worker:8095",
                    internal_token="internal-token",
                    worker_client=client,
                    web_search_configured=bool(os.getenv("TAVILY_API_KEY", "").strip()),
                ),
            )
        )
    )


@pytest.mark.asyncio
async def test_catalog_broker_projects_worker_scope_and_runtime_shape(monkeypatch) -> None:
    _, records = agent_capabilities.load_assistant_capability_catalog()
    record = next(item for item in records if item["id"] == "search_knowledge_base")
    client = _CatalogClient(
        _CatalogResponse(
            {
                "schema_version": "capability-catalog/v2",
                "capability_revision": 1,
                "capabilities": [_catalog_descriptor(record["id"], record=record)],
            }
        )
    )
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "internal-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_URL", "http://worker:8095")
    result = await agent_capabilities.broker_agent_capability_catalog(
        _catalog_request(),
        _catalog_request_context(client),
        x_ai_platform_internal_token="internal-token",
        x_ai_tenant_id="tenant-a",
        x_ai_user_id="user-a",
        x_ai_session_id="session-a",
    )
    assert [item["name"] for item in result["tools"]] == ["search_knowledge_base"]
    assert result["tools"][0]["tenant_id"] == "tenant-a"
    assert result["tools"][0]["capability_revision"] == 1
    assert result["tools"][0]["output_schema"] == {"type": "object"}
    assert result["tools"][0]["approval_policy"] == "never"
    assert result["tools"][0]["execution_mode"] == "inline"
    assert result["tools"][0]["timeout_ms"] == record["timeout_ms"]
    assert result["tools"][0]["tags"] == [
        f"kind:{record['kind']}",
        f"category:{record['category']}",
    ]
    assert result["tools"][0]["protocol"] == record["protocol"]
    assert result["mcp"] == []
    assert result["deferred"] == []
    assert client.calls[0]["url"] == "http://worker:8095/internal/v2/capabilities/catalog"
    assert client.calls[0]["headers"]["x-ai-user-id"] == "user-a"
    assert client.calls[0]["json"]["schema_version"] == "capability-catalog/v2"


@pytest.mark.asyncio
async def test_catalog_hides_tavily_search_until_its_key_is_configured(monkeypatch) -> None:
    _, records = agent_capabilities.load_assistant_capability_catalog()
    record = next(item for item in records if item["id"] == "search_web")
    client = _CatalogClient(
        _CatalogResponse(
            {
                "schema_version": "capability-catalog/v2",
                "capability_revision": 1,
                "capabilities": [_catalog_descriptor(record["id"], record=record)],
            }
        )
    )
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "internal-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_URL", "http://worker:8095")
    monkeypatch.delenv("TAVILY_API_KEY", raising=False)
    kwargs = {
        "x_ai_platform_internal_token": "internal-token",
        "x_ai_tenant_id": "tenant-a",
        "x_ai_user_id": "user-a",
        "x_ai_session_id": "session-a",
    }
    unavailable = await agent_capabilities.broker_agent_capability_catalog(
        _catalog_request(), _catalog_request_context(client), **kwargs
    )
    assert unavailable["tools"] == []

    monkeypatch.setenv("TAVILY_API_KEY", "provider-secret")
    available = await agent_capabilities.broker_agent_capability_catalog(
        _catalog_request(), _catalog_request_context(client), **kwargs
    )
    assert [item["name"] for item in available["tools"]] == ["search_web"]


@pytest.mark.asyncio
async def test_catalog_broker_applies_tenant_policy_before_runtime_projection(monkeypatch) -> None:
    _, records = agent_capabilities.load_assistant_capability_catalog()
    first = next(item for item in records if item["id"] == "search_knowledge_base")
    second = next(item for item in records if item["id"] == "web_fetch")
    client = _CatalogClient(
        _CatalogResponse(
            {
                "schema_version": "capability-catalog/v2",
                "capability_revision": 1,
                "capabilities": [
                    _catalog_descriptor(first["id"], record=first),
                    _catalog_descriptor(second["id"], record=second),
                ],
            }
        )
    )
    context = _catalog_request_context(client)
    context.app.state.database.fetchrow = _policy_row
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "internal-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_URL", "http://worker:8095")
    result = await agent_capabilities.broker_agent_capability_catalog(
        _catalog_request(),
        context,
        x_ai_platform_internal_token="internal-token",
        x_ai_tenant_id="tenant-a",
        x_ai_user_id="user-a",
        x_ai_session_id="session-a",
    )
    assert [item["name"] for item in result["tools"]] == ["search_knowledge_base"]


async def _policy_row(_query: str, _tenant_id: str):
    return {"allowed_tools": ["search_knowledge_base"], "blocked_tools": [], "allowed_categories": []}


@pytest.mark.asyncio
async def test_catalog_broker_rejects_scope_before_worker_call(monkeypatch) -> None:
    client = _CatalogClient(_CatalogResponse({}))
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "internal-token")
    with pytest.raises(HTTPException) as error:
        await agent_capabilities.broker_agent_capability_catalog(
            _catalog_request(),
            _catalog_request_context(client),
            x_ai_platform_internal_token="internal-token",
            x_ai_tenant_id="tenant-other",
            x_ai_user_id="user-a",
            x_ai_session_id="session-a",
        )
    assert error.value.status_code == 403
    assert client.calls == []


def _artifact(text: str = "hello 世界") -> tuple[SimpleNamespace, bytes]:
    raw = text.encode()
    return SimpleNamespace(
        source="tool_output_spill",
        turn_id="receipt-1",
        metadata={
            "schema_version": "assistant-tool-output-artifact/v1",
            "redacted": True,
            "complete_redacted": True,
            "content_kind": "text",
            "content_sha256": hashlib.sha256(raw).hexdigest(),
            "content_chars": len(text),
            "host_receipt_id": "receipt-1",
        },
    ), raw


def _headers(
    payload: agent_capabilities.ArtifactReadRequest,
    *,
    token: str = "secret-token",
    **overrides: str,
) -> dict[str, str]:
    body = payload.model_dump(mode="json")
    now = int(time.time())
    values = {
        "tenant_id": "tenant-a",
        "user_id": "user-a",
        "session_id": "session-a",
        "execution_id": "11111111-1111-4111-8111-111111111111",
        "run_id": "run-a",
    }
    proof = sign_capability_proof(
        PROOF_SECRET,
        method="POST",
        path="/internal/v2/agent-capabilities/artifacts/art_12345678/read",
        body=body,
        **values,
        nonce="nonce-a",
        now=now,
    )
    return {
        "x_ai_platform_internal_token": token,
        "x_ai_tenant_id": values["tenant_id"],
        "x_ai_user_id": values["user_id"],
        "x_ai_session_id": values["session_id"],
        "x_ai_capability_proof": proof,
        "x_ai_execution_id": values["execution_id"],
        "x_ai_run_id": values["run_id"],
        **overrides,
    }


def _web_headers(payload: agent_capabilities.WebSearchRequest) -> dict[str, str]:
    values = {
        "tenant_id": "tenant-a",
        "user_id": "user-a",
        "session_id": "session-a",
        "execution_id": "11111111-1111-4111-8111-111111111111",
        "run_id": "run-a",
    }
    proof = sign_capability_proof(
        PROOF_SECRET,
        method="POST",
        path="/internal/v2/agent-capabilities/web-search",
        body=payload.model_dump(mode="json"),
        **values,
        nonce="nonce-web",
        now=int(time.time()),
    )
    return {
        "x_ai_platform_internal_token": "secret-token",
        "x_ai_tenant_id": values["tenant_id"],
        "x_ai_user_id": values["user_id"],
        "x_ai_session_id": values["session_id"],
        "x_ai_capability_proof": proof,
        "x_ai_execution_id": values["execution_id"],
        "x_ai_run_id": values["run_id"],
    }


@pytest.mark.asyncio
async def test_web_search_is_scope_bound_bounded_and_keeps_key_in_gateway(monkeypatch) -> None:
    client = _SearchClient(
        _CatalogResponse(
            {
                "results": [
                    {
                        "title": "Result",
                        "url": "https://example.com/source",
                        "content": "bounded content",
                        "score": 0.8,
                        "raw_content": "must not escape",
                    }
                ]
            }
        )
    )
    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(agent_web_search_client=client))
    )
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "secret-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", PROOF_SECRET)
    monkeypatch.setenv("TAVILY_API_KEY", "provider-secret")
    payload = agent_capabilities.WebSearchRequest(
        queries=["agent runtime"], max_results=3
    )

    result = await agent_capabilities.search_agent_web(
        request,
        payload=payload,
        **_web_headers(payload),
    )

    assert result["schema_version"] == "agent-web-search-result/v1"
    assert result["queries"][0]["results"] == [
        {
            "title": "Result",
            "url": "https://example.com/source",
            "content": "bounded content",
            "score": 0.8,
        }
    ]
    assert client.calls[0]["url"] == agent_capabilities._TAVILY_SEARCH_URL
    assert client.calls[0]["headers"]["authorization"] == "Bearer provider-secret"
    assert "api_key" not in client.calls[0]["json"]


@pytest.mark.asyncio
async def test_read_is_constant_time_scoped_and_bounded(monkeypatch: pytest.MonkeyPatch) -> None:
    artifact, raw = _artifact("abcdef")

    class Storage:
        async def read_artifact_scoped(self, artifact_id: str, **scope: str):
            assert artifact_id == "art_12345678"
            assert scope["tenant_id"] == "tenant-a"
            return artifact, raw

    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "secret-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", PROOF_SECRET)
    monkeypatch.setattr(agent_capabilities, "get_artifact_storage", lambda: Storage())
    payload = agent_capabilities.ArtifactReadRequest(offset=1, limit=3)
    response = await agent_capabilities.read_agent_capability_artifact(
        "art_12345678",
        payload=payload,
        **_headers(payload),
    )
    assert response["content"] == "bcd"
    assert response["next_offset"] == 4


@pytest.mark.asyncio
async def test_wrong_token_scope_and_foreign_artifact_are_non_enumerating(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "secret-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", PROOF_SECRET)
    monkeypatch.setattr(agent_capabilities, "get_artifact_storage", lambda: None)
    payload = agent_capabilities.ArtifactReadRequest()
    headers = _headers(payload)
    with pytest.raises(HTTPException) as wrong:
        await agent_capabilities.read_agent_capability_artifact(
            "art_12345678",
            payload=payload,
            **{**headers, "x_ai_platform_internal_token": "wrong"},
        )
    assert wrong.value.status_code == 401
    with pytest.raises(HTTPException) as foreign:
        await agent_capabilities.read_agent_capability_artifact(
            "art_12345678",
            payload=payload,
            **headers,
        )
    assert foreign.value.status_code == 404


@pytest.mark.asyncio
async def test_unredacted_or_invalid_artifact_never_reads_content(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    artifact, raw = _artifact()
    artifact.metadata["redacted"] = False

    class Storage:
        async def read_artifact_scoped(self, *_args, **_kwargs):
            return artifact, raw

    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "secret-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", PROOF_SECRET)
    monkeypatch.setattr(agent_capabilities, "get_artifact_storage", lambda: Storage())
    payload = agent_capabilities.ArtifactReadRequest()
    with pytest.raises(HTTPException) as exc:
        await agent_capabilities.read_agent_capability_artifact(
            "art_12345678",
            payload=payload,
            **_headers(payload),
        )
    assert exc.value.status_code == 404


@pytest.mark.asyncio
async def test_proof_binds_body_scope_and_expiry(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "secret-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", PROOF_SECRET)
    monkeypatch.setattr(agent_capabilities, "get_artifact_storage", lambda: None)
    payload = agent_capabilities.ArtifactReadRequest()
    headers = _headers(payload)
    altered = agent_capabilities.ArtifactReadRequest(limit=123)
    with pytest.raises(HTTPException) as body_error:
        await agent_capabilities.read_agent_capability_artifact(
            "art_12345678",
            payload=altered,
            **headers,
        )
    assert body_error.value.status_code == 401
    with pytest.raises(HTTPException) as scope_error:
        await agent_capabilities.read_agent_capability_artifact(
            "art_12345678",
            payload=payload,
            **{**headers, "x_ai_tenant_id": "tenant-b"},
        )
    assert scope_error.value.status_code == 401
    now = int(time.time())
    expired = sign_capability_proof(
        PROOF_SECRET,
        method="POST",
        path="/internal/v2/agent-capabilities/artifacts/art_12345678/read",
        body=payload.model_dump(mode="json"),
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        execution_id="11111111-1111-4111-8111-111111111111",
        run_id="run-a",
        expires_at=now - 1,
        nonce="nonce-expired",
        now=now - 100,
    )
    with pytest.raises(HTTPException) as expiry_error:
        await agent_capabilities.read_agent_capability_artifact(
            "art_12345678",
            payload=payload,
            **{**headers, "x_ai_capability_proof": expired},
        )
    assert expiry_error.value.status_code == 401
    with pytest.raises(HTTPException) as path_error:
        await agent_capabilities.read_agent_capability_artifact(
            "art_87654321",
            payload=payload,
            **headers,
        )
    assert path_error.value.status_code == 401
