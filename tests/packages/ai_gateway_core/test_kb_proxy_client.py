from __future__ import annotations

import json
from types import SimpleNamespace

import httpx
import pytest
from ai_gateway_core.auth.gateway_secret import GatewaySecret
from ai_gateway_core.comm.client import InternalServiceHTTPError
from ai_gateway_core.knowledge import KnowledgeClientLike
from ai_gateway_core.knowledge.proxy_client import KBProxyClient
from ai_gateway_core.proxy.request_id_middleware import REQUEST_ID_CTX


@pytest.mark.asyncio
async def test_kb_proxy_client_propagates_request_id_to_knowledge_service() -> None:
    seen_headers: dict[str, str] = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        seen_headers.update(dict(request.headers))
        return httpx.Response(200, json={"datasets": []})

    client = KBProxyClient(
        base_url="http://knowledge-service.test",
        transport=httpx.MockTransport(handler),
    )

    token = REQUEST_ID_CTX.set("req-kb-hop-123")
    try:
        await client.list_datasets(SimpleNamespace(user_id="u1", tenant_id="t1", tier="normal"))
    finally:
        REQUEST_ID_CTX.reset(token)
        await client.close()

    assert seen_headers["x-request-id"] == "req-kb-hop-123"


def test_kb_proxy_client_accepts_custom_timeout_and_limits() -> None:
    timeout = httpx.Timeout(connect=1.0, read=2.0, write=3.0, pool=4.0)
    limits = httpx.Limits(max_connections=7, max_keepalive_connections=3)

    client = KBProxyClient(
        base_url="http://knowledge-service.test",
        timeout=timeout,
        limits=limits,
        transport=httpx.MockTransport(lambda _request: httpx.Response(200, json={})),
    )

    http_client = client._get_client()

    assert http_client.timeout == timeout


def test_kb_proxy_client_reads_timeout_and_pool_limits_from_env(monkeypatch) -> None:
    monkeypatch.setenv("KB_PROXY_CONNECT_TIMEOUT_SECONDS", "1.5")
    monkeypatch.setenv("KB_PROXY_READ_TIMEOUT_SECONDS", "45")
    monkeypatch.setenv("KB_PROXY_WRITE_TIMEOUT_SECONDS", "6")
    monkeypatch.setenv("KB_PROXY_POOL_TIMEOUT_SECONDS", "8")
    monkeypatch.setenv("KB_PROXY_MAX_CONNECTIONS", "25")
    monkeypatch.setenv("KB_PROXY_MAX_KEEPALIVE_CONNECTIONS", "9")

    client = KBProxyClient(base_url="http://knowledge-service.test")

    assert client.timeout.connect == 1.5
    assert client.timeout.read == 45.0
    assert client.timeout.write == 6.0
    assert client.timeout.pool == 8.0
    assert client.limits == httpx.Limits(max_connections=25, max_keepalive_connections=9)


@pytest.mark.asyncio
async def test_kb_proxy_client_does_not_disguise_dataset_outage_as_empty() -> None:
    client = KBProxyClient(
        base_url="http://knowledge-service.test",
        transport=httpx.MockTransport(
            lambda _request: httpx.Response(503, json={"detail": "unavailable"})
        ),
    )
    try:
        with pytest.raises(InternalServiceHTTPError):
            await client.list_datasets(SimpleNamespace(user_id="u1", tenant_id="t1", tier="normal"))
    finally:
        await client.close()


@pytest.mark.asyncio
async def test_document_authorization_uses_signed_identity_and_rejects_unrequested_ids() -> None:
    requests: list[dict] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/api/v1/internal/knowledge/documents/authorize"
        assert request.headers["x-user-id"] == "u1"
        assert request.headers["x-tenant-id"] == "t1"
        body = json.loads(request.content)
        requests.append(body)
        return httpx.Response(200, json={"allowed_document_ids": ["document-a"]})

    client = KBProxyClient(
        base_url="http://knowledge-service.test",
        transport=httpx.MockTransport(handler),
    )
    actor = SimpleNamespace(user_id="u1", tenant_id="t1", tier="normal", roles=["user"])
    try:
        assert await client.authorize_documents(
            actor, "dataset-a", ["document-b", "document-a", "document-a"],
        ) == {"document-a"}
        assert requests == [{"dataset_id": "dataset-a", "document_ids": ["document-a", "document-b"]}]
        with pytest.raises(ValueError, match="unknown IDs"):
            await client.authorize_documents(actor, "dataset-a", ["document-b"])
    finally:
        await client.close()


@pytest.mark.asyncio
async def test_anonymous_kb_proxy_identity_cannot_retain_admin_headers() -> None:
    seen: dict[str, str] = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        seen.update(dict(request.headers))
        return httpx.Response(200, json={"allowed_document_ids": []})

    client = KBProxyClient(
        base_url="http://knowledge-service.test",
        gateway_secret=GatewaySecret(
            secret="anonymous-kb-proxy-test-secret",
            caller_service="gateway",
            audience="knowledge-service",
            allowed_path_prefixes=("/api/v1",),
        ),
        transport=httpx.MockTransport(handler),
    )
    actor = SimpleNamespace(
        user_id="agent-embed:opaque",
        tenant_id="tenant-a",
        tier="admin",
        roles=["admin"],
        user_type="user",
        is_authenticated=False,
    )
    try:
        assert await client.authorize_documents(actor, "dataset-a", ["doc-a"]) == set()
    finally:
        await client.close()

    assert seen["x-user-type"] == "anonymous"
    assert seen["x-user-tier"] == "anonymous"
    assert seen["x-user-roles"] == "guest"
    assert seen["x-gateway-secret"].startswith("v2:")


@pytest.mark.asyncio
async def test_source_version_authorization_rejects_unrequested_identity() -> None:
    source_hash = "a" * 64
    requested = {"document_id": "document-a", "source_version": 2, "source_hash": source_hash}
    responses = [[requested], [{**requested, "source_version": 3}]]

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/api/v1/internal/knowledge/document-sources/authorize"
        assert request.headers["x-user-id"] == "u1"
        assert json.loads(request.content)["references"] == [requested]
        return httpx.Response(200, json={"allowed_references": responses.pop(0)})

    client = KBProxyClient(
        base_url="http://knowledge-service.test",
        transport=httpx.MockTransport(handler),
    )
    actor = SimpleNamespace(user_id="u1", tenant_id="t1", tier="normal", roles=["user"])
    try:
        assert await client.authorize_document_sources(
            actor, "dataset-a", [requested],
        ) == {("document-a", 2, source_hash)}
        with pytest.raises(ValueError, match="unknown identity"):
            await client.authorize_document_sources(actor, "dataset-a", [requested])
    finally:
        await client.close()


@pytest.mark.asyncio
async def test_kb_proxy_client_forwards_supported_retrieval_options() -> None:
    seen_payload: dict[str, object] = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        seen_payload.update(json.loads(request.content))
        return httpx.Response(200, json={"results": [], "metadata": {}})

    client = KBProxyClient(
        base_url="http://knowledge-service.test",
        transport=httpx.MockTransport(handler),
    )

    try:
        await client.retrieve(
            SimpleNamespace(user_id="u1", tenant_id="t1", tier="normal"),
            "dataset-1",
            "query",
            candidate_top_k=40,
            rerank=False,
            mmr=True,
            include_images=False,
            include_associated_images=False,
            metadata_filter={"madhab": "hanafi"},
            ignored_option="not-forwarded",
        )
    finally:
        await client.close()

    assert isinstance(client, KnowledgeClientLike)
    assert seen_payload == {
        "query": "query",
        "top_k": 5,
        "mode": "hybrid",
        "candidate_top_k": 40,
        "rerank": False,
        "mmr": True,
        "include_images": False,
        "include_associated_images": False,
        "metadata_filter": {"madhab": "hanafi"},
    }


@pytest.mark.asyncio
async def test_kb_proxy_client_multimodal_wrappers_are_explicit() -> None:
    seen_payloads: list[dict[str, object]] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        seen_payloads.append(json.loads(request.content))
        return httpx.Response(
            200,
            json={
                "results": [
                    {
                        "text": "diagram",
                        "score": 0.9,
                        "content_type": "image",
                        "image_url": "https://example.test/image.png",
                        "associated_images": [{"image_segment_id": "img-1"}],
                    }
                ],
                "metadata": {},
            },
        )

    client = KBProxyClient(
        base_url="http://knowledge-service.test",
        transport=httpx.MockTransport(handler),
    )

    try:
        results, _ = await client.retrieve_with_images(
            SimpleNamespace(user_id="u1", tenant_id="t1", tier="normal"),
            "dataset-1",
            "diagram",
        )
        await client.retrieve_with_images_v2(
            SimpleNamespace(user_id="u1", tenant_id="t1", tier="normal"),
            "dataset-1",
            "policy",
            intent="find_document",
        )
    finally:
        await client.close()

    assert results[0].content_type == "image"
    assert results[0].associated_images == ({"image_segment_id": "img-1"},)
    assert seen_payloads[0]["include_images"] is True
    assert seen_payloads[0]["include_associated_images"] is True
    assert seen_payloads[1]["include_images"] is False
    assert seen_payloads[1]["include_associated_images"] is False


@pytest.mark.asyncio
async def test_kb_proxy_client_traverses_catalog_past_empty_acl_page_with_roles() -> None:
    seen_cursors: list[str | None] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        cursor = request.url.params.get("cursor")
        seen_cursors.append(cursor)
        assert request.headers["x-user-roles"] == "reviewer"
        if cursor is None:
            return httpx.Response(
                200,
                json=[{"dataset_id": str(i)} for i in range(200)],
                headers={"X-Next-Cursor": "second"},
            )
        if cursor == "second":
            return httpx.Response(200, json=[], headers={"X-Next-Cursor": "third"})
        assert cursor == "third"
        return httpx.Response(200, json=[{"dataset_id": "200"}])

    client = KBProxyClient(
        base_url="http://knowledge-service.test", transport=httpx.MockTransport(handler)
    )
    try:
        result = await client.list_datasets(
            SimpleNamespace(user_id="u", tenant_id="t", tier="normal", roles=["reviewer"])
        )
    finally:
        await client.close()
    assert len(result) == 201
    assert seen_cursors == [None, "second", "third"]


@pytest.mark.asyncio
async def test_kb_proxy_client_rejects_nonadvancing_cursor_instead_of_hanging() -> None:
    client = KBProxyClient(
        base_url="http://knowledge-service.test",
        transport=httpx.MockTransport(
            lambda _request: httpx.Response(200, json=[], headers={"X-Next-Cursor": "same"})
        ),
    )
    try:
        with pytest.raises(ValueError, match="pagination did not advance"):
            await client.list_datasets(SimpleNamespace(user_id="u", tenant_id="t", tier="normal"))
    finally:
        await client.close()
