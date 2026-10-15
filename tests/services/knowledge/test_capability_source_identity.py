"""Actual signed capability handler, with controlled retrieval/database authorities."""
from __future__ import annotations

import asyncio
import time
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from ai_gateway_contracts.capability_proof import sign_capability_proof
from fastapi import HTTPException
from knowledge_service.api.routes import capability_plane, knowledge
from knowledge_service.core.auth.user_resolver import UserContext
from knowledge_service.core.exceptions import PermissionDeniedError
from starlette.datastructures import Headers

HASH = "a" * 64
SECRET = "synthetic-proof-only-" * 2
DATASET = {"dataset_id": "dataset-one", "tenant_id": "tenant-one", "content_revision": 7}
USER = UserContext(user_id="user-one", tenant_id="tenant-one", roles=[], user_tier="normal")


@pytest.fixture
def host(monkeypatch):
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "synthetic-internal-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", SECRET)
    monkeypatch.setattr(capability_plane, "resolve_runtime_actor", AsyncMock(return_value=USER))
    payload = capability_plane.CapabilityRetrieveRequest(query="question")
    path = "/internal/v2/capabilities/knowledge/dataset-one/retrieve"
    scope = {"tenant_id": "tenant-one", "user_id": "user-one", "session_id": "session-one",
             "execution_id": "11111111-1111-4111-8111-111111111111", "run_id": "run-one"}
    proof = sign_capability_proof(SECRET, method="POST", path=path,
                                  body=payload.model_dump(mode="json"), **scope,
                                  nonce="identity-test", now=int(time.time()))
    request = SimpleNamespace(headers=Headers({
        "x-ai-platform-internal-token": "synthetic-internal-token",
        "x-ai-capability-proof": proof,
        **{f"x-ai-{key.replace('_', '-')}": value for key, value in scope.items()},
    }), body=AsyncMock(return_value=b"{}"))
    result = SimpleNamespace(segment_id="segment-one", document_id="document-one", score=0.8,
                             text="saved excerpt", content_type="text", metadata={
                                 "source_version": 99, "source_hash": "stale-index-hash",
                                 "dataset_id": "wrong-dataset", "document_id": "wrong-document",
                                 "segment_id": "wrong-segment", "source_type": "upload",
                             })
    service = SimpleNamespace(
        require_dataset_access=AsyncMock(return_value=dict(DATASET)),
        retrieve=AsyncMock(return_value=([result], {"trace_id": "trace-one"})),
        db=SimpleNamespace(published_source_identities=AsyncMock(
            return_value={"document-one": (2, HASH)},
        )),
    )

    async def call():
        return await capability_plane.retrieve_capability("dataset-one", request, payload, service)

    return SimpleNamespace(call=call, service=service, result=result, request=request,
                           payload=payload)


@pytest.mark.asyncio
async def test_capability_uses_published_identity(host):
    value = await host.call()
    row = value["results"][0]
    assert row["metadata"]["source_version"] == 2, "capability must use published source version"
    assert row["metadata"]["source_hash"] == HASH
    assert value["metadata"] == {"trace_id": "trace-one"} and row["text"] == "saved excerpt"
    host.service.db.published_source_identities.assert_awaited_once_with(
        "dataset-one", "tenant-one", ["document-one"],
    )
    assert host.service.require_dataset_access.await_count == 3
    assert all(call.args == (USER, "dataset-one") and call.kwargs == {"required": "viewer"}
               for call in host.service.require_dataset_access.await_args_list)


@pytest.mark.asyncio
async def test_missing_identity_strips_stale_metadata(host):
    host.service.db.published_source_identities.return_value = {}
    value = await host.call()
    metadata = value["results"][0]["metadata"]
    assert "source_version" not in metadata, "missing authority must not keep stale source identity"
    assert "source_hash" not in metadata
    assert value["results"][0]["text"] == "saved excerpt"


@pytest.mark.asyncio
async def test_ids_come_from_route_and_hit(host):
    metadata = (await host.call())["results"][0]["metadata"]
    assert (metadata["dataset_id"], metadata["document_id"], metadata["segment_id"]) == (
        "dataset-one", "document-one", "segment-one",
    )
    assert host.result.metadata["source_hash"] == "stale-index-hash"


@pytest.mark.asyncio
async def test_absent_lookup_retains_unversioned_excerpt(host):
    host.service.db = SimpleNamespace()
    row = (await host.call())["results"][0]
    assert "source_hash" not in row["metadata"] and "source_version" not in row["metadata"]
    assert row["text"] == "saved excerpt"
    host.service.require_dataset_access.assert_awaited_once()


@pytest.mark.asyncio
async def test_missing_generation_does_not_claim_version(host):
    host.service.require_dataset_access.return_value = {"dataset_id": "dataset-one"}
    row = (await host.call())["results"][0]
    assert "source_hash" not in row["metadata"]
    host.service.db.published_source_identities.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("manual", [
    {"source_type": "manual"}, {"manual_source_override": True},
    {"metadata": {"manual_source_override": True}},
], ids=["source-type", "top-override", "nested-override"])
async def test_manual_excerpt_never_borrows_published_hash(host, manual):
    host.result.metadata.update(manual)
    row = (await host.call())["results"][0]
    assert "source_version" not in row["metadata"] and "source_hash" not in row["metadata"]


@pytest.mark.asyncio
@pytest.mark.parametrize("when", ["before-lookup", "after-lookup"])
async def test_generation_race_never_labels_old_excerpt(host, when):
    changed = {**DATASET, "content_revision": 8}
    host.service.require_dataset_access.side_effect = (
        [DATASET, changed] if when == "before-lookup" else [DATASET, DATASET, changed]
    )
    with pytest.raises(HTTPException) as caught:
        await host.call()
    assert caught.value.status_code == 400
    assert caught.value.detail == "Invalid retrieval request"
    assert host.service.db.published_source_identities.await_count == (when == "after-lookup")


@pytest.mark.asyncio
@pytest.mark.parametrize("when", ["before-lookup", "after-lookup"])
async def test_acl_recheck_denial_never_returns_excerpt(host, when):
    denied = PermissionDeniedError("internal-secret")
    host.service.require_dataset_access.side_effect = (
        [DATASET, denied] if when == "before-lookup" else [DATASET, DATASET, denied]
    )
    with pytest.raises(HTTPException) as caught:
        await host.call()
    assert caught.value.status_code == 404 and caught.value.detail == "Dataset not found"


@pytest.mark.asyncio
@pytest.mark.parametrize("error", [RuntimeError, TimeoutError, ConnectionError],
                         ids=["runtime", "timeout", "connection"])
async def test_lookup_outage_is_safe_failure(host, error):
    host.service.db.published_source_identities.side_effect = error("internal-secret")
    with pytest.raises(HTTPException) as caught:
        await host.call()
    assert caught.value.status_code == 503
    assert caught.value.detail == "Source identity authority unavailable"


@pytest.mark.asyncio
@pytest.mark.parametrize("invalid", [
    [], {"other-document": (2, HASH)}, {"document-one": (True, HASH)},
    {"document-one": (0, HASH)}, {"document-one": (2, "stale")},
    {"document-one": (2, "A" * 64)}, {"document-one": None},
], ids=["not-map", "other-document", "bool-version", "zero-version", "bad-hash", "noncanonical-hash", "null"])
async def test_invalid_authority_reply_fails_closed(host, invalid):
    host.service.db.published_source_identities.return_value = invalid
    with pytest.raises(HTTPException) as caught:
        await host.call()
    assert caught.value.status_code == 503
    assert "internal-secret" not in str(caught.value.detail)


def test_public_helpers_are_same_shared_implementation():
    from knowledge_service.services.knowledge import source_identity

    assert knowledge._source_metadata is source_identity._source_metadata
    assert knowledge._source_identity is source_identity._source_identity
    assert knowledge._published_source_identity_map is source_identity._published_source_identity_map
    value = {"source_hash": "stale", "source_version": 99, "other": "preserved"}
    assert knowledge._source_metadata(value, "document-one", {"document-one": (2, HASH)}) == {
        "source_version": 2, "source_hash": HASH, "other": "preserved",
    }
    assert value["source_hash"] == "stale"


@pytest.mark.asyncio
async def test_proof_failure_prevents_retrieval_and_identity_lookup(host):
    host.request.headers = Headers({**dict(host.request.headers), "x-ai-capability-proof": "invalid"})
    with pytest.raises(HTTPException) as caught:
        await host.call()
    assert caught.value.status_code == 401
    host.service.retrieve.assert_not_awaited()
    host.service.db.published_source_identities.assert_not_awaited()


@pytest.mark.asyncio
async def test_empty_results_do_not_lookup_source_identity(host):
    host.service.retrieve.return_value = ([], {"retrieved_count": 0})
    assert await host.call() == {"results": [], "metadata": {"retrieved_count": 0}}
    host.service.db.published_source_identities.assert_not_awaited()


def test_identity_sql_remains_bound_to_active_verified_current_version():
    import inspect

    from knowledge_service.persistence.database import DatabaseStorage

    source = inspect.getsource(DatabaseStorage.published_source_identities)
    for clause in (
        "ds.tenant_id = $2", "ds.is_deleted = FALSE", "d.enabled", "d.archived",
        "_document_lifecycle_reindex", "v.version_number = d.current_version",
        "pending_restore", "pending_before_restore", "v.content = d.content",
        "sha256(convert_to(v.content, 'UTF8'))", "lower(v.content_hash)",
    ):
        assert clause in source


@pytest.mark.asyncio
async def test_lookup_cancellation_is_not_ordinary_failure(host):
    host.service.db.published_source_identities.side_effect = asyncio.CancelledError()
    with pytest.raises(asyncio.CancelledError):
        await host.call()
