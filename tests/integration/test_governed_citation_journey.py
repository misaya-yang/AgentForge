"""Offline Python HTTP composition; Runtime projection and storage remain controlled."""
from __future__ import annotations

import hashlib
import json
import time
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_assistant import AssistantClient
from ai_assistant.exceptions import AssistantError
from ai_gateway_contracts.capability_proof import sign_capability_proof
from ai_gateway_contracts.replay import InMemoryReplayStore
from ai_gateway_core.auth.gateway_secret import GatewaySecret
from ai_gateway_core.auth.gateway_secret_middleware import GatewaySecretAuthMiddleware
from ai_gateway_core.comm.retry import RetryPolicy
from ai_gateway_core.knowledge.proxy_client import KBProxyClient
from ai_gateway_core.proxy import ServiceProxy, ServiceProxyConfig
from fastapi import FastAPI, Request
from knowledge_service.api.routes import capability_plane
from knowledge_service.api.routes import knowledge as knowledge_routes
from knowledge_service.auth.user_context import UserContext as KnowledgeUser
from knowledge_service.core.exceptions import PermissionDeniedError

from src.api import deps
from src.api.v1 import _proxy_utils
from src.api.v1 import knowledge as gateway_knowledge
from src.api.v1._assistant_routes import chat
from src.core.auth.user_resolver import UserContext

SECRET = "synthetic-citation-wire-secret-" * 2
PROOF_SECRET = "synthetic-worker-proof-secret-" * 2
TENANT, OWNER, SESSION = "tenant-one", "owner-one", "session-one"
DATASET, DOCUMENT, SEGMENT = "dataset-one", "document-one", "segment-one"
RUN = "11111111-1111-4111-8111-111111111111"
THREAD = "22222222-2222-4222-8222-222222222222"
CONTENT = "Full immutable document. This is the cited excerpt. Remaining source text."
HASH = hashlib.sha256(CONTENT.encode()).hexdigest()


class RecordedTransport(httpx.AsyncBaseTransport):
    def __init__(self, app, records):
        self.inner = httpx.ASGITransport(app=app)
        self.records = records

    async def handle_async_request(self, request):
        body = await request.aread()
        row = {"method": request.method, "path": request.url.path,
               "query": str(request.url.query.decode()), "body": body,
               "owner": request.headers.get("x-user-id"),
               "tenant": request.headers.get("x-tenant-id"),
               "signed": bool(request.headers.get("x-gateway-secret")),
               "proof": bool(request.headers.get("x-ai-capability-proof")),
               "capability_scope": {key: request.headers.get("x-ai-" + key)
                                    for key in ("tenant-id", "user-id", "session-id", "run-id")}}
        self.records.append(row)
        response = await self.inner.handle_async_request(request)
        row["status"] = response.status_code
        if response.status_code >= 400:
            await response.aread()
            row["error"] = response.json()
        return response

    async def aclose(self):
        await self.inner.aclose()


@pytest.fixture
async def journey(monkeypatch):
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", SECRET)
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", PROOF_SECRET)
    monkeypatch.setenv("INTERNAL_AUTH_VERSION", "v2")
    state = SimpleNamespace(allowed=True, starts=0, closed=0, retrieved=0,
                            bad_proof=False, failed=False, revoke_on_close=False,
                            wires=[], gateway_wires=[], requests=[], authoritative=True)
    user = UserContext(user_id=OWNER, tenant_id=TENANT, is_authenticated=True)
    dataset = {"dataset_id": DATASET, "tenant_id": TENANT, "name": "Current source name",
               "content_revision": 7}

    async def access(actor, dataset_id, *, required):
        assert (actor.user_id, actor.tenant_id, dataset_id, required) == (OWNER, TENANT, DATASET, "viewer")
        if not state.allowed:
            raise PermissionDeniedError("private source denial")
        return dict(dataset)

    async def catalog(actor, **kwargs):
        assert (actor.user_id, actor.tenant_id) == (OWNER, TENANT)
        assert kwargs == {"limit": 200, "cursor": None}
        return {"items": [dict(dataset)] if state.allowed else [], "next_cursor": None}

    async def active(dataset_id, tenant_id, ids):
        assert (dataset_id, tenant_id) == (DATASET, TENANT)
        return {DOCUMENT} & set(ids) if state.allowed else set()

    async def sources(dataset_id, tenant_id, references):
        assert (dataset_id, tenant_id) == (DATASET, TENANT)
        return {(DOCUMENT, 3, HASH)} & set(references) if state.allowed else set()

    async def identities(dataset_id, tenant_id, ids):
        assert (dataset_id, tenant_id, ids) == (DATASET, TENANT, [DOCUMENT])
        return {DOCUMENT: (3, HASH)} if state.authoritative else {}

    async def document(document_id):
        assert document_id == DOCUMENT
        return {"document_id": DOCUMENT, "dataset_id": DATASET, "content": CONTENT}

    async def version(document_id, version_number):
        assert document_id == DOCUMENT
        if version_number != 3:
            return None
        return {"content": CONTENT, "content_hash": HASH, "change_type": "edit", "title": "Saved title"}

    async def retrieve(**kwargs):
        assert kwargs["user"].user_id == OWNER and kwargs["user"].tenant_id == TENANT
        assert kwargs["dataset_id"] == DATASET and kwargs["query"] == "What does the source say?"
        state.retrieved += 1
        return [SimpleNamespace(document_id=DOCUMENT, segment_id=SEGMENT,
                                text="This is the cited excerpt.", score=0.8, content_type="text",
                                metadata={"source_version": 99, "source_hash": "b" * 64,
                                          "dataset_id": "spoof", "document_id": "spoof",
                                          "private_token": "internal-secret"})], {}

    service = SimpleNamespace(require_dataset_access=access, list_datasets_page=catalog, retrieve=retrieve,
                              db=SimpleNamespace(filter_active_document_ids=active,
                                  authorize_document_source_versions=sources,
                                  published_source_identities=identities,
                                  get_document=document, get_document_version=version))
    kb_app = FastAPI()
    kb_app.state.knowledge_service = service
    kb_app.include_router(knowledge_routes.router, prefix="/api/v1")
    kb_app.include_router(capability_plane.router)
    verifier = GatewaySecret(secret=SECRET, version="v2", replay_store=InMemoryReplayStore(),
                             caller_service="gateway", audience="knowledge-service",
                             allowed_path_prefixes=("/api/v1",))
    kb_app.add_middleware(GatewaySecretAuthMiddleware, gateway_secret=verifier,
                         separately_authenticated_prefixes=frozenset({"/internal/v2/capabilities/"}))
    signer = GatewaySecret(secret=SECRET, version="v2", caller_service="gateway",
                           audience="knowledge-service", allowed_path_prefixes=("/api/v1",))
    kb_wire = RecordedTransport(kb_app, state.wires)
    kb_proxy = KBProxyClient(base_url="http://knowledge.test", transport=kb_wire,
                            gateway_secret=signer, retry_policy=RetryPolicy(max_attempts=1))
    source_proxy = ServiceProxy(ServiceProxyConfig(name="citation-test", base_url="http://knowledge.test",
                                                  retry_policy=RetryPolicy(max_attempts=1)),
                                signer=_proxy_utils._sign_request)
    source_proxy._client = httpx.AsyncClient(base_url="http://knowledge.test", transport=kb_wire)
    monkeypatch.setattr(_proxy_utils, "_signer", signer)
    monkeypatch.setattr(_proxy_utils, "_proxy", source_proxy)

    async def resolve_actor(*, tenant_id, user_id):
        assert (tenant_id, user_id) == (TENANT, OWNER)
        return KnowledgeUser(user_id=OWNER, tenant_id=TENANT)

    monkeypatch.setattr(capability_plane, "resolve_runtime_actor", resolve_actor)
    worker_client = httpx.AsyncClient(base_url="http://knowledge.test", transport=kb_wire)
    turn = SimpleNamespace(run_id=RUN, runtime_thread_id=THREAD)

    async def start(_request, actor, body, *, session_id, model_id):
        assert actor is user and session_id == SESSION and model_id == "model-one"
        assert body.message == "What does the source say?" and body.kb_dataset_ids == [DATASET]
        state.starts += 1
        state.requests.append(body)
        return turn

    async def frames(**kwargs):
        assert kwargs == {"turn": turn, "tenant_id": TENANT, "user_id": OWNER, "session_id": SESSION}
        payload = capability_plane.CapabilityRetrieveRequest(query=state.requests[-1].message).model_dump(mode="json")
        path = f"/internal/v2/capabilities/knowledge/{DATASET}/retrieve"
        scope = {"tenant_id": TENANT, "user_id": OWNER, "session_id": SESSION,
                 "execution_id": "33333333-3333-4333-8333-333333333333", "run_id": RUN}
        proof = sign_capability_proof(PROOF_SECRET, method="POST", path=path, body=payload,
                                      **scope, nonce="journey-call", now=int(time.time()))
        headers = {"x-ai-platform-internal-token": SECRET,
                   "x-ai-capability-proof": "invalid-proof" if state.bad_proof else proof,
                   **{f"x-ai-{key.replace('_', '-')}": value for key, value in scope.items()}}
        try:
            result = await worker_client.post(path, json=payload, headers=headers)
            # Controlled adapter mirrors capability_dispatch.rs; this is not a Rust execution.
            events = []
            if result.status_code == 200:
                chunks = [{"dataset_id": item["metadata"]["dataset_id"],
                           "document_id": item["document_id"], "segment_id": item["segment_id"],
                           "content": item["text"], "score": item["score"], "metadata": item["metadata"]}
                          for item in result.json()["results"]]
                events.append(("context_retrieved", {"tool_call_id": "call-one", "chunks": chunks}))
                events.append(("text_delta", {"content": "The source supports this answer."}))
            status = "failed" if state.failed or result.status_code != 200 else "succeeded"
            envelope = {"run_id": RUN, "session_id": SESSION, "thread_id": THREAD,
                        "status": status, "exit_reason": status}
            events.append(("run_error" if status == "failed" else "run_finished",
                           {"status": status, "terminal_envelope": envelope}))
            for sequence, (kind, data) in enumerate(events, 1):
                event = {"sequence": sequence, "event_type": kind,
                         "data": {"run_id": RUN, "session_id": SESSION, "thread_id": THREAD, **data}}
                yield ("data: " + json.dumps(event) + "\n\n").encode()
        finally:
            state.closed += 1
            if state.revoke_on_close:
                state.allowed = False

    app = FastAPI()
    app.include_router(chat.router, prefix="/api/v1/assistant")
    app.include_router(gateway_knowledge.router, prefix="/api/v1")
    app.state.kb_proxy = kb_proxy
    app.state.database = SimpleNamespace(fetch=AsyncMock(return_value=[]), fetchrow=AsyncMock(return_value=None))
    app.state.agent_runtime_control = SimpleNamespace(stream_events=frames)

    async def actor(request: Request):
        assert request.headers["x-api-key"] == "synthetic-sdk-key"
        assert request.headers["x-tenant-id"] == TENANT and request.headers["x-user-id"] == OWNER
        return user

    app.dependency_overrides[deps.get_user_context] = actor
    app.dependency_overrides[deps.get_rate_limiter] = lambda: None
    monkeypatch.setattr(deps, "enforce_rate_limit", AsyncMock())
    monkeypatch.setattr(chat, "effective_chat_model_id", lambda *_args: "model-one")
    monkeypatch.setattr(chat, "validate_chat_session_access", AsyncMock())
    monkeypatch.setattr(chat, "ensure_agent_runtime_session", AsyncMock())
    monkeypatch.setattr(chat, "_start_agent_runtime_turn", start)
    client = AssistantClient("synthetic-sdk-key", base_url="http://gateway.test",
                             tenant_id=TENANT, user_id=OWNER, max_retries=3)
    await client._transport._client.aclose()
    client._transport._client = httpx.AsyncClient(base_url="http://gateway.test",
                                                 transport=RecordedTransport(app, state.gateway_wires))

    async def ask():
        return await client.knowledge.ask("What does the source say?", [DATASET], session_id=SESSION)

    async def open_source(answer, *, version_number=None, source_hash=None):
        citation = answer.contexts[0]
        identity = citation["chunks"][0]["metadata"]
        version_number = identity["source_version"] if version_number is None else version_number
        source_hash = identity["source_hash"] if source_hash is None else source_hash
        return await client.knowledge.get_source(
            citation["dataset_id"], identity["document_id"],
            source_version=version_number, source_hash=source_hash,
        )

    state.ask, state.open_source, state.worker_client = ask, open_source, worker_client
    state.signer = signer
    try:
        yield state
    finally:
        await client.close()
        await kb_proxy.close()
        await source_proxy.aclose()
        await worker_client.aclose()


def assert_once(state):
    assert state.starts == state.closed == 1
    assert sum(row["method"] == "POST" and row["path"] == "/api/v1/assistant/chat"
               for row in state.gateway_wires) == 1
    assert sum(row["path"].startswith("/internal/v2/capabilities/") for row in state.wires) == 1


@pytest.mark.asyncio
async def test_question_to_exact_source_over_real_python_http_layers(journey):
    answer = await journey.ask()
    citation = answer.contexts[0]
    identity = citation["chunks"][0]["metadata"]
    assert identity == {"document_id": DOCUMENT, "segment_id": SEGMENT,
                        "source_version": 3, "source_hash": HASH}
    assert citation["dataset_id"] == DATASET and citation["dataset_name"] == "Current source name"
    assert answer.content == "The source supports this answer."
    assert answer.usage == {} and answer.run_id == RUN
    assert "internal-secret" not in json.dumps(answer.contexts)
    response = await journey.open_source(answer)
    assert response["content"] == CONTENT
    assert response["source_hash"] == hashlib.sha256(response["content"].encode()).hexdigest()
    assert response["version_number"] == 3
    public = [row for row in journey.wires if row["path"].startswith("/api/v1/")]
    assert public and all(row["signed"] and row["owner"] == OWNER and row["tenant"] == TENANT for row in public)
    assert any(row["path"].endswith("/document-sources/authorize") for row in public)
    assert public[-1]["query"] == f"hash={HASH}"
    capability = next(row for row in journey.wires if row["proof"])
    assert capability["capability_scope"] == {"tenant-id": TENANT, "user-id": OWNER,
                                               "session-id": SESSION, "run-id": RUN}
    assert json.loads(capability["body"])["query"] == "What does the source say?"
    assert journey.retrieved == 1
    assert_once(journey)


@pytest.mark.asyncio
@pytest.mark.parametrize("change", ["hash", "version"], ids=["hash", "version"])
async def test_tampered_citation_source_is_unavailable(journey, change):
    answer = await journey.ask()
    kwargs = {"source_hash": "b" * 64} if change == "hash" else {"version_number": 99}
    with pytest.raises(AssistantError) as error:
        await journey.open_source(answer, **kwargs)
    assert error.value.status_code == 404
    assert journey.gateway_wires[-1]["error"]["detail"]["code"] == "SOURCE_VERSION_UNAVAILABLE"
    assert CONTENT not in str(error.value)
    assert_once(journey)


@pytest.mark.asyncio
async def test_revocation_before_source_open_denies_saved_citation(journey):
    answer = await journey.ask()
    journey.allowed = False
    with pytest.raises(AssistantError) as error:
        await journey.open_source(answer)
    assert error.value.status_code == 404
    assert journey.gateway_wires[-1]["error"]["detail"]["code"] == "SOURCE_VERSION_UNAVAILABLE"
    assert_once(journey)


@pytest.mark.asyncio
async def test_cleanup_revocation_denies_answer_over_real_authorization_hop(journey):
    journey.revoke_on_close = True
    with pytest.raises(AssistantError) as error:
        await journey.ask()
    assert error.value.status_code == 403
    assert journey.gateway_wires[-1]["error"]["detail"]["code"] == "ASSISTANT_SOURCE_ACCESS_REVOKED"
    assert "cited excerpt" not in str(error.value)
    assert_once(journey)


@pytest.mark.asyncio
async def test_failed_runtime_adapter_terminal_never_returns_citations(journey):
    journey.failed = True
    with pytest.raises(AssistantError) as error:
        await journey.ask()
    assert error.value.status_code == 409
    assert journey.gateway_wires[-1]["error"]["detail"]["code"] == "ASSISTANT_CHAT_FAILED"
    assert_once(journey)


@pytest.mark.asyncio
async def test_invalid_capability_proof_rejected_before_retrieval(journey):
    journey.bad_proof = True
    with pytest.raises(AssistantError) as error:
        await journey.ask()
    assert error.value.status_code == 409
    assert journey.gateway_wires[-1]["error"]["detail"]["code"] == "ASSISTANT_CHAT_FAILED"
    assert journey.retrieved == 0
    assert any(row["status"] == 401 and row["error"]["detail"] == "capability proof invalid"
               for row in journey.wires if row["proof"])
    assert_once(journey)


@pytest.mark.asyncio
async def test_unsigned_private_knowledge_request_is_rejected(journey):
    response = await journey.worker_client.get("/api/v1/knowledge/datasets", headers={
        "x-user-id": OWNER, "x-tenant-id": TENANT})
    assert response.status_code == 401 and journey.retrieved == journey.starts == 0
    assert response.json()["code"] == "AUTH_DENIED"


@pytest.mark.asyncio
async def test_unresolved_source_identity_stays_unversioned(journey):
    journey.authoritative = False
    answer = await journey.ask()
    identity = answer.contexts[0]["chunks"][0]["metadata"]
    assert identity == {"document_id": DOCUMENT, "segment_id": SEGMENT}
    assert "b" * 64 not in json.dumps(answer.contexts)
    assert_once(journey)


@pytest.mark.asyncio
async def test_signed_identity_tampering_is_rejected_by_real_middleware(journey):
    path = "/api/v1/knowledge/datasets"
    headers = {"X-User-Id": OWNER, "X-Tenant-Id": TENANT, "X-User-Type": "user"}
    signature = journey.signer.sign(method="GET", path=path, query="", body=None,
                                    identity_headers=headers)
    response = await journey.worker_client.get(path, headers={
        **headers, "X-User-Id": "another-owner", "X-Gateway-Secret": signature})
    assert response.status_code == 401 and journey.starts == journey.retrieved == 0
    assert response.json()["code"] == "AUTH_DENIED"
