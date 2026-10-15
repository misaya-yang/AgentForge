from __future__ import annotations

import hashlib
from contextlib import asynccontextmanager
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException, Response
from knowledge_service.api.routes import knowledge as knowledge_routes
from knowledge_service.api.routes.knowledge import (
    DocumentAuthorizeRequest,
    VersionRestoreRequest,
    _source_metadata,
    authorize_gateway_documents,
    batch_create_documents,
    batch_reindex_documents,
    compare_document_versions,
    debug_dataset,
    force_complete_document,
    get_active_document_source,
    get_dataset_sources,
    get_document_pipeline_execution,
    get_document_version,
    get_historical_document_source,
    get_image_segment,
    hit_test,
    list_document_versions,
    list_documents,
    qa_query,
    qa_query_stream,
    recover_document,
    reindex_document,
    reprocess_document,
    restore_document_version,
    retrieve,
    retrieve_batch,
    retry_document,
)
from knowledge_service.api.schemas.knowledge import (
    BatchReindexSchema,
    BatchRetrieveRequestSchema,
    DocumentBatchCreateSchema,
    QAQuerySchema,
    RetrieveRequestSchema,
)
from knowledge_service.core.auth.user_resolver import UserContext
from knowledge_service.core.exceptions import PermissionDeniedError, ValidationFailedError
from knowledge_service.persistence.database import IndexLeaseUnavailableError
from starlette.requests import Request


def test_manual_segment_is_not_attributed_to_immutable_document_version() -> None:
    identities = {"document-a": (2, "a" * 64)}
    assert "source_version" not in _source_metadata(
        {"source_type": "manual"}, "document-a", identities,
    )
    assert "source_hash" not in _source_metadata(
        {"metadata": {"manual_source_override": True}},
        "document-a", identities,
    )


@pytest.mark.asyncio
async def test_sources_route_distinguishes_unconfigured_sync_from_zero_updates() -> None:
    service = SimpleNamespace(
        require_dataset_access=AsyncMock(return_value=DATASET),
        db=SimpleNamespace(count_documents_by_source_type=AsyncMock(return_value={
            "upload": 2, "url": 1,
        })),
    )
    request = Request({
        "type": "http", "headers": [],
        "app": SimpleNamespace(state=SimpleNamespace(confluence_sync_service=None)),
    })

    result = await get_dataset_sources(request, "dataset-a", svc=service, user=ADMIN)

    assert result["connector_status"] == "not_configured"
    assert result["confluence_bindings"] == []
    assert result["total_documents"] == 3


@pytest.mark.asyncio
async def test_sources_route_keeps_sync_failure_distinct_from_empty_success() -> None:
    connector = SimpleNamespace(list_bindings=AsyncMock(side_effect=RuntimeError("secret")))
    service = SimpleNamespace(
        require_dataset_access=AsyncMock(return_value=DATASET),
        db=SimpleNamespace(count_documents_by_source_type=AsyncMock(return_value={
            "upload": 1,
        })),
    )
    request = Request({
        "type": "http", "headers": [],
        "app": SimpleNamespace(state=SimpleNamespace(confluence_sync_service=connector)),
    })

    result = await get_dataset_sources(request, "dataset-a", svc=service, user=ADMIN)

    assert result["connector_status"] == "unavailable"
    assert result["confluence_bindings"] == []
    assert result["total_documents"] == 1


@pytest.mark.asyncio
async def test_sources_route_does_not_show_stale_error_after_successful_sync() -> None:
    connector = SimpleNamespace(list_bindings=AsyncMock(return_value=[{
        "binding_id": "binding-a", "space_name": "Space A",
        "synced_page_count": 3, "status": "completed",
        "last_sync_at": "2026-09-28T12:00:00Z", "last_error": "previous failure",
    }]))
    service = SimpleNamespace(
        require_dataset_access=AsyncMock(return_value=DATASET),
        db=SimpleNamespace(count_documents_by_source_type=AsyncMock(return_value={})),
    )
    request = Request({
        "type": "http", "headers": [],
        "app": SimpleNamespace(state=SimpleNamespace(confluence_sync_service=connector)),
    })

    result = await get_dataset_sources(request, "dataset-a", svc=service, user=ADMIN)

    assert result["confluence_bindings"][0]["has_problem"] is False
    assert result["confluence_bindings"][0]["page_count"] == 3

USER = UserContext(user_id="user-a", tenant_id="tenant-a")
ADMIN = UserContext(user_id="admin-a", tenant_id="tenant-a", user_tier="admin")
DATASET = {
    "dataset_id": "dataset-a",
    "tenant_id": "tenant-a",
    "name": "Dataset A",
    "index_config": {},
    "content_revision": 7,
}


@pytest.mark.asyncio
async def test_internal_document_authority_filters_live_rows_and_scope() -> None:
    calls: list[tuple[str, str, list[str]]] = []

    class Service:
        async def require_dataset_access(self, user, dataset_id, *, required):
            assert (user.user_id, user.tenant_id, dataset_id, required) == (
                "user-a", "tenant-a", "dataset-a", "viewer",
            )
            return DATASET

        db = SimpleNamespace()

    async def filter_active(dataset_id, tenant_id, document_ids):
        calls.append((dataset_id, tenant_id, document_ids))
        return {"document-a"}

    Service.db.filter_active_document_ids = filter_active
    request = Request({
        "type": "http",
        "headers": [(b"x-user-id", b"user-a"), (b"x-tenant-id", b"tenant-a")],
    })
    result = await authorize_gateway_documents(
        request,
        DocumentAuthorizeRequest(dataset_id="dataset-a", document_ids=["document-b", "document-a", "document-a"]),
        svc=Service(),  # type: ignore[arg-type]
    )
    assert result.allowed_document_ids == ["document-a"]
    assert calls == [("dataset-a", "tenant-a", ["document-a", "document-b"])]


def test_version_compare_path_is_not_captured_by_numeric_version_route() -> None:
    path = "/knowledge/dataset-a/documents/document-a/versions/compare"
    matches = [
        route.name for route in knowledge_routes.router.routes
        if getattr(route, "path_regex", None) and route.path_regex.match(path)
        and "GET" in (getattr(route, "methods", None) or set())
    ]
    assert matches == ["compare_document_versions"]


@pytest.mark.asyncio
async def test_internal_document_authority_requires_signed_identity_and_dataset_viewer() -> None:
    class Service:
        async def require_dataset_access(self, *_args, **_kwargs):
            raise PermissionDeniedError("private dataset")

        db = SimpleNamespace(filter_active_document_ids=AsyncMock())

    service = Service()
    body = DocumentAuthorizeRequest(dataset_id="dataset-a", document_ids=["document-a"])
    with pytest.raises(HTTPException) as missing:
        await authorize_gateway_documents(
            Request({"type": "http", "headers": []}), body,
            svc=service,  # type: ignore[arg-type]
        )
    assert missing.value.status_code == 401
    allowed = await authorize_gateway_documents(
        Request({"type": "http", "headers": [(b"x-user-id", b"user-a"), (b"x-tenant-id", b"tenant-a")]}),
        body, svc=service,  # type: ignore[arg-type]
    )
    assert allowed.allowed_document_ids == []
    service.db.filter_active_document_ids.assert_not_awaited()


@pytest.mark.asyncio
async def test_citation_source_read_rechecks_document_active_even_for_editor() -> None:
    body = {"document_id": "document-a", "dataset_id": "dataset-a",
            "content": "private old body", "status": "completed"}
    database = SimpleNamespace(
        get_document=AsyncMock(return_value=body),
        filter_active_document_ids=AsyncMock(return_value=set()),
    )

    class Service(_BaseService):
        async def get_document(self, *_args):
            return body  # Editor management read is allowed.

    service = Service(database)
    with pytest.raises(HTTPException) as denied:
        await get_active_document_source(
            "dataset-a", "document-a", svc=service, user=USER,  # type: ignore[arg-type]
        )
    assert denied.value.status_code == 404
    database.filter_active_document_ids.return_value = {"document-a"}
    assert (await get_active_document_source(
        "dataset-a", "document-a", svc=service, user=USER,  # type: ignore[arg-type]
    ))["content"] == "private old body"


@pytest.mark.asyncio
async def test_pipeline_execution_receipt_is_scoped_to_dataset_and_document() -> None:
    class Database:
        async def get_pipeline_execution(self, execution_id: str):
            return {
                "execution_id": execution_id,
                "dataset_id": "dataset-a",
                "document_id": "document-a",
                "status": "running",
            }

    class Service:
        db = Database()

        async def require_dataset_access(self, _user, dataset_id: str, *, required: str):
            assert dataset_id == "dataset-a"
            assert required == "viewer"
            return DATASET

    receipt = await get_document_pipeline_execution(
        "dataset-a",
        "document-a",
        "exec-1",
        svc=Service(),  # type: ignore[arg-type]
        user=USER,
    )
    assert receipt["execution_id"] == "exec-1"

    with pytest.raises(HTTPException) as exc_info:
        await get_document_pipeline_execution(
            "dataset-a",
            "document-other",
            "exec-1",
            svc=Service(),  # type: ignore[arg-type]
            user=USER,
        )
    assert exc_info.value.status_code == 404


@pytest.mark.asyncio
async def test_retrieve_route_passes_stage_timings_through_untouched() -> None:
    # Phase 0 quick win: stage_timings must reach /retrieve callers inside
    # response metadata (single unified surface, not a side channel).
    class _Svc:
        async def retrieve(self, **_kwargs: Any):
            return (
                [],
                {
                    "timings_ms": {"total_ms": 12.5, "rerank_ms": 2.0},
                    "retrieval_cache_hit": False,
                    "pipeline_stages": ["Recall"],
                    "trace_id": "d04d53c8-acde-49d0-b3eb-49890dbd5673",
                    "query_fingerprint": "a" * 64,
                },
            )

    response = await retrieve(
        "dataset-a",
        payload=RetrieveRequestSchema(query="hello", top_k=5),
        svc=_Svc(),  # type: ignore[arg-type]
        user=USER,
    )

    assert response["metadata"]["timings_ms"] == {"total_ms": 12.5, "rerank_ms": 2.0}
    assert response["trace_id"] == "d04d53c8-acde-49d0-b3eb-49890dbd5673"
    assert response["query_fingerprint"] == "a" * 64
    assert response["results"] == []


@pytest.mark.asyncio
async def test_retrieve_route_forwards_relevance_options_verbatim() -> None:
    # Every relevance knob the schema accepts must reach the service layer
    # unchanged — silent drops here are invisible contract drift.
    captured: dict[str, Any] = {}

    class _Svc:
        async def retrieve(self, **kwargs: Any):
            captured.update(kwargs)
            return ([], {})

    payload = RetrieveRequestSchema(
        query="hello",
        top_k=7,
        mode="dense",
        score_threshold=0.42,
        dense_weight=0.75,
        bm25_weight=0.25,
        fusion_method="weighted",
        rerank=True,
        rerank_model="bge-reranker-v2-m3",
        rerank_top_n=9,
        mmr=True,
        mmr_lambda=0.6,
    )
    await retrieve("dataset-a", payload=payload, svc=_Svc(), user=USER)  # type: ignore[arg-type]

    assert captured["query"] == "hello"
    assert captured["top_k"] == 7
    assert captured["mode"] == "dense"
    assert captured["score_threshold"] == 0.42
    assert captured["dense_weight"] == 0.75
    assert captured["bm25_weight"] == 0.25
    assert captured["fusion_method"] == "weighted"
    assert captured["rerank"] is True
    assert captured["rerank_model"] == "bge-reranker-v2-m3"
    assert captured["rerank_top_n"] == 9
    assert captured["mmr"] is True
    assert captured["mmr_lambda"] == 0.6


@pytest.mark.asyncio
@pytest.mark.parametrize("route_name", ["retrieve", "retrieve_batch", "hit_test"])
async def test_retrieval_routes_map_publication_busy_to_retryable_conflict(
    route_name: str,
) -> None:
    class _BusyService:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict:
            return DATASET

        async def retrieve(self, **_kwargs: Any):
            raise IndexLeaseUnavailableError("dataset index publication is in progress")

        async def retrieve_batch(self, **_kwargs: Any):
            raise IndexLeaseUnavailableError("dataset index publication is in progress")

    service = _BusyService()
    with pytest.raises(HTTPException) as exc_info:
        if route_name == "retrieve":
            await retrieve(
                "dataset-a",
                payload=RetrieveRequestSchema(query="hello"),
                svc=service,  # type: ignore[arg-type]
                user=USER,
            )
        elif route_name == "retrieve_batch":
            await retrieve_batch(
                "dataset-a",
                payload=BatchRetrieveRequestSchema(queries=["hello"]),
                svc=service,  # type: ignore[arg-type]
                user=USER,
            )
        else:
            await hit_test(
                "dataset-a",
                payload=RetrieveRequestSchema(query="hello"),
                svc=service,  # type: ignore[arg-type]
                user=USER,
            )

    assert exc_info.value.status_code == 409
    assert exc_info.value.headers == {"Retry-After": "1"}


@pytest.mark.asyncio
async def test_hit_test_attaches_only_verified_published_source_identity() -> None:
    digest = "a" * 64

    class Service:
        db = SimpleNamespace(published_source_identities=AsyncMock(
            return_value={"document-a": (3, digest)},
        ))

        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict:
            return DATASET

        async def retrieve(self, **_kwargs: Any):
            return [
                SimpleNamespace(segment_id="segment-a", document_id="document-a",
                                score=0.9, text="published", metadata={}),
                SimpleNamespace(segment_id="segment-manual", document_id="document-a",
                                score=0.8, text="manual", metadata={"source_type": "manual"}),
            ], {"trace_id": "trace-a"}

    service = Service()
    result = await hit_test(
        "dataset-a", payload=RetrieveRequestSchema(query="published"),
        svc=service, user=ADMIN,
    )

    assert result["results"][0]["source_version"] == 3
    assert result["results"][0]["source_hash"] == digest
    assert result["results"][0]["metadata"]["source_hash"] == digest
    assert result["results"][1]["source_version"] is None
    assert result["results"][1]["source_hash"] is None
    assert "source_hash" not in result["results"][1]["metadata"]
    service.db.published_source_identities.assert_awaited_once()


@pytest.mark.asyncio
async def test_hit_test_refuses_version_attribution_after_generation_change() -> None:
    calls = 0

    class Service:
        db = SimpleNamespace(published_source_identities=AsyncMock())

        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict:
            nonlocal calls
            calls += 1
            return {**DATASET, "content_revision": 8 if calls > 1 else 7}

        async def retrieve(self, **_kwargs: Any):
            return [SimpleNamespace(
                segment_id="segment-a", document_id="document-a",
                score=0.9, text="older", metadata={},
            )], {}

    service = Service()
    with pytest.raises(HTTPException) as exc_info:
        await hit_test(
            "dataset-a", payload=RetrieveRequestSchema(query="published"),
            svc=service, user=ADMIN,
        )

    assert exc_info.value.status_code == 400
    service.db.published_source_identities.assert_not_awaited()


@pytest.mark.asyncio
async def test_qa_stream_preflight_maps_publication_busy_to_conflict() -> None:
    class _BusyService:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict:
            return {**DATASET, "content_revision": -1}

    with pytest.raises(HTTPException) as exc_info:
        await qa_query_stream(
            request=None,  # type: ignore[arg-type]
            dataset_id="dataset-a",
            payload=QAQuerySchema(query="hello"),
            svc=_BusyService(),  # type: ignore[arg-type]
            user=ADMIN,
            settings=None,  # type: ignore[arg-type]
        )

    assert exc_info.value.status_code == 409
    assert exc_info.value.headers == {"Retry-After": "1"}


@pytest.mark.asyncio
async def test_qa_query_maps_retrieval_publication_busy_to_conflict(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _BusyService:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict:
            return DATASET

    class _BusyQAService:
        def __init__(self, *_args: Any, **_kwargs: Any) -> None:
            pass

        async def query(self, **_kwargs: Any):
            raise IndexLeaseUnavailableError("dataset index publication is in progress")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.qa_service.QAService",
        _BusyQAService,
    )
    monkeypatch.setattr(
        knowledge_routes,
        "_build_server_qa_llm_config",
        lambda *_args: None,
    )

    with pytest.raises(HTTPException) as exc_info:
        await qa_query(
            request=None,  # type: ignore[arg-type]
            dataset_id="dataset-a",
            payload=QAQuerySchema(query="hello"),
            svc=_BusyService(),  # type: ignore[arg-type]
            user=ADMIN,
            settings=None,  # type: ignore[arg-type]
        )

    assert exc_info.value.status_code == 409
    assert exc_info.value.headers == {"Retry-After": "1"}


@pytest.mark.asyncio
async def test_document_list_maps_generation_race_to_retryable_conflict() -> None:
    class _RaceService:
        async def list_documents_page(
            self, _user: UserContext, _dataset_id: str, **_kwargs: Any
        ) -> dict[str, Any]:
            raise ValidationFailedError(
                "dataset content generation changed during read; retry the request"
            )

    with pytest.raises(HTTPException) as exc_info:
        await list_documents(
            "dataset-a",
            response=Response(),
            svc=_RaceService(),  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 409
    assert exc_info.value.headers == {"Retry-After": "1"}


@pytest.mark.asyncio
async def test_document_list_maps_other_validation_failures_without_500() -> None:
    class _InvalidService:
        async def list_documents_page(
            self, _user: UserContext, _dataset_id: str, **_kwargs: Any
        ) -> dict[str, Any]:
            raise ValidationFailedError("dataset not found")

    with pytest.raises(HTTPException) as exc_info:
        await list_documents(
            "dataset-a",
            response=Response(),
            svc=_InvalidService(),  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 400


class _BaseService:
    def __init__(self, database: Any) -> None:
        self.db = database
        self.image_storage_service = None

    async def require_dataset_access(
        self,
        user: UserContext,
        dataset_id: str,
        *,
        required: str,
    ) -> dict[str, Any]:
        assert user.tenant_id == "tenant-a"
        assert dataset_id == "dataset-a"
        assert required in {"viewer", "editor"}
        return dict(DATASET)

    async def get_dataset_statistics(
        self,
        _user: UserContext,
        dataset_id: str,
    ) -> dict[str, int]:
        assert dataset_id == "dataset-a"
        return {"segment_count": 3}


class _ImageDatabase:
    def __init__(self, *, active: bool) -> None:
        self.active = active
        self.tenant_calls: list[tuple[str, str]] = []
        self.scoped_calls: list[tuple[str, str, str]] = []

    async def get_active_segment_by_tenant(
        self,
        segment_id: str,
        tenant_id: str,
    ) -> dict[str, Any] | None:
        self.tenant_calls.append((segment_id, tenant_id))
        if not self.active:
            return None
        return {
            "segment_id": segment_id,
            "dataset_id": "dataset-a",
            "content_type": "image",
            "image_url": "https://images.example.test/a.png",
        }

    async def get_segment_scoped(
        self,
        segment_id: str,
        dataset_id: str,
        tenant_id: str,
    ) -> dict[str, Any] | None:
        self.scoped_calls.append((segment_id, dataset_id, tenant_id))
        if not self.active:
            return None
        return {
            "segment_id": segment_id,
            "dataset_id": dataset_id,
            "content_type": "image",
            "image_url": "https://images.example.test/a.png",
        }


@pytest.mark.asyncio
async def test_image_route_rejects_inactive_segment_after_exact_scope_authority() -> None:
    database = _ImageDatabase(active=False)
    service = _BaseService(database)

    with pytest.raises(HTTPException) as exc_info:
        await get_image_segment(
            "image-a",
            svc=service,  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 404
    assert database.tenant_calls == [("image-a", "tenant-a")]
    assert database.scoped_calls == []


@pytest.mark.asyncio
async def test_active_image_route_is_explicitly_unavailable() -> None:
    service = _BaseService(_ImageDatabase(active=True))

    with pytest.raises(HTTPException) as exc_info:
        await get_image_segment(
            "image-a",
            svc=service,  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 503
    assert "not enabled" in str(exc_info.value.detail)


class _DebugDatabase:
    async def list_segments(self, **kwargs: Any) -> list[dict[str, Any]]:
        assert kwargs == {"dataset_id": "dataset-a", "limit": 100, "offset": 0}
        return [
            {"segment_id": "disabled-a", "document_id": "doc-a", "text": "secret"},
            {"segment_id": "active-a", "document_id": "doc-b", "text": "visible"},
            {"segment_id": "pending-a", "document_id": "doc-c", "text": "pending"},
        ]

    async def filter_active_segment_ids(
        self,
        dataset_id: str,
        tenant_id: str,
        segment_ids: list[str],
    ) -> set[str]:
        assert dataset_id == "dataset-a"
        assert tenant_id == "tenant-a"
        assert segment_ids == ["active-a", "disabled-a", "pending-a"]
        return {"active-a"}


@pytest.mark.asyncio
async def test_debug_route_never_samples_inactive_or_lifecycle_pending_segments() -> None:
    service = _BaseService(_DebugDatabase())

    result = await debug_dataset(
        "dataset-a",
        svc=service,  # type: ignore[arg-type]
        user=USER,
    )

    assert [row["segment_id"] for row in result["sample_segments"]] == ["active-a"]
    assert result["has_segments"] is True
    assert "secret" not in str(result)
    assert "pending" not in str(result)


class _VersionDatabase:
    def __init__(self, *, active: bool) -> None:
        self.active = active
        self.version_reads = 0
        self.status_writes = 0

    async def filter_active_document_ids(
        self,
        dataset_id: str,
        tenant_id: str,
        document_ids: list[str],
    ) -> set[str]:
        assert dataset_id == "dataset-a"
        assert tenant_id == "tenant-a"
        assert document_ids == ["document-a"]
        return {"document-a"} if self.active else set()

    async def get_document(self, document_id: str) -> dict[str, Any]:
        assert document_id == "document-a"
        return {
            "document_id": document_id,
            "dataset_id": "dataset-a",
            "status": "completed",
            "enabled": True,
            "archived": False,
            "metadata": {},
            "current_version": 2,
        }

    async def list_document_versions(self, *_args: Any) -> list[dict[str, Any]]:
        self.version_reads += 1
        return []

    async def get_document_version_count(self, _document_id: str) -> int:
        self.version_reads += 1
        return 0

    async def get_document_version(
        self,
        document_id: str,
        version_number: int,
    ) -> dict[str, Any]:
        self.version_reads += 1
        return {
            "document_id": document_id,
            "version_number": version_number,
            "content": "version content",
        }

    async def update_document_status(self, *_args: Any, **_kwargs: Any) -> None:
        self.status_writes += 1


@pytest.mark.asyncio
@pytest.mark.parametrize("inactive_reason", ["foreign", "disabled", "lifecycle_pending"])
async def test_full_version_content_fails_closed_for_nonactive_document(
    inactive_reason: str,
) -> None:
    del inactive_reason
    database = _VersionDatabase(active=False)
    service = _BaseService(database)

    with pytest.raises(HTTPException) as exc_info:
        await get_document_version(
            "dataset-a",
            "document-a",
            1,
            svc=service,  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 404
    assert database.version_reads == 0


@pytest.mark.asyncio
async def test_historical_source_requires_exact_saved_content_hash() -> None:
    content = "Saved source text"
    source_hash = hashlib.sha256(content.encode("utf-8")).hexdigest()

    class _ExactVersionDatabase(_VersionDatabase):
        async def get_document_version(
            self, document_id: str, version_number: int,
        ) -> dict[str, Any]:
            self.version_reads += 1
            return {
                "document_id": document_id, "version_number": version_number,
                "content": content, "content_hash": source_hash,
                "change_type": "updated", "title": "saved.txt",
            }

    database = _ExactVersionDatabase(active=True)
    service = _BaseService(database)
    result = await get_historical_document_source(
        "dataset-a", "document-a", 2, source_hash=source_hash,
        svc=service, user=USER,  # type: ignore[arg-type]
    )
    assert result["content"] == content
    assert result["version_number"] == 2

    with pytest.raises(HTTPException) as denied:
        await get_historical_document_source(
            "dataset-a", "document-a", 2, source_hash="0" * 64,
            svc=service, user=USER,  # type: ignore[arg-type]
        )
    assert denied.value.status_code == 404


@pytest.mark.asyncio
async def test_version_list_and_compare_share_active_document_authority() -> None:
    database = _VersionDatabase(active=False)
    service = _BaseService(database)

    with pytest.raises(HTTPException) as list_error:
        await list_document_versions(
            "dataset-a",
            "document-a",
            limit=20,
            offset=0,
            svc=service,  # type: ignore[arg-type]
            user=USER,
        )
    with pytest.raises(HTTPException) as compare_error:
        await compare_document_versions(
            "dataset-a",
            "document-a",
            from_version=1,
            to_version=2,
            svc=service,  # type: ignore[arg-type]
            user=USER,
        )

    assert list_error.value.status_code == 404
    assert compare_error.value.status_code == 404
    assert database.version_reads == 0


@pytest.mark.asyncio
async def test_force_complete_cannot_activate_pending_or_incomplete_document() -> None:
    database = _VersionDatabase(active=False)
    service = _BaseService(database)

    with pytest.raises(HTTPException) as exc_info:
        await force_complete_document(
            "dataset-a",
            "document-a",
            svc=service,  # type: ignore[arg-type]
            user=ADMIN,
        )

    assert exc_info.value.status_code == 400
    assert database.status_writes == 0


class _ReindexWorker:
    def __init__(self, *, queued: bool) -> None:
        self.queued = queued
        self.calls: list[tuple[str, str]] = []
        self.kwargs: list[dict[str, Any]] = []

    async def enqueue(self, dataset_id: str, document_id: str, **kwargs: Any) -> bool:
        self.calls.append((dataset_id, document_id))
        self.kwargs.append(kwargs)
        return self.queued


@pytest.mark.asyncio
async def test_reindex_route_never_overwrites_durable_queued_state() -> None:
    database = _VersionDatabase(active=True)
    service = _BaseService(database)
    worker = _ReindexWorker(queued=True)

    result = await reindex_document(
        "dataset-a",
        "document-a",
        svc=service,  # type: ignore[arg-type]
        worker=worker,  # type: ignore[arg-type]
        user=USER,
    )

    assert result == {
        "status": "queuing",
        "document_id": "document-a",
        "execution_id": None,
        "job_url": None,
    }
    assert worker.calls == [("dataset-a", "document-a")]
    # The reindex route is the reembed verb landing point (PRD T1 item 3):
    # the claim must be pinned with the action, never a bare default ingest.
    assert worker.kwargs == [
        {"action": "reembed", "recover_stage": None, "execution_id": None}
    ]
    assert database.status_writes == 0


@pytest.mark.asyncio
async def test_reindex_route_reports_duplicate_or_ineligible_enqueue() -> None:
    service = _BaseService(_VersionDatabase(active=True))
    worker = _ReindexWorker(queued=False)

    with pytest.raises(HTTPException) as exc_info:
        await reindex_document(
            "dataset-a",
            "document-a",
            svc=service,  # type: ignore[arg-type]
            worker=worker,  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 409


@pytest.mark.asyncio
async def test_reindex_route_reports_missing_document_with_404() -> None:
    """The reindex pre-check resolves the document BEFORE opening a ledger
    row, so a missing document surfaces a 404 instead of orphaning an
    execution row behind a claim failure."""

    database = _VerbDatabase(None)
    service = _BaseService(database)
    worker = _ReindexWorker(queued=True)

    with pytest.raises(HTTPException) as exc_info:
        await reindex_document(
            "dataset-a",
            "document-a",
            svc=service,  # type: ignore[arg-type]
            worker=worker,  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 404
    assert worker.calls == []
    assert database.records == []
    assert database.completions == []


@pytest.mark.asyncio
async def test_reindex_route_rejects_queued_document_before_ledger() -> None:
    """A waiting row belongs to the durable queue: reindex must 409 before
    opening its ledger row, so the identical-verb re-claim can never re-pin a
    fresh execution row over the queued generation's."""

    database = _VerbDatabase(_verb_document(status="waiting"))
    service = _BaseService(database)
    worker = _ReindexWorker(queued=True)

    with pytest.raises(HTTPException) as exc_info:
        await reindex_document(
            "dataset-a",
            "document-a",
            svc=service,  # type: ignore[arg-type]
            worker=worker,  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 409
    assert worker.calls == []
    assert database.records == []
    assert database.completions == []


class _VerbDatabase:
    """Fake db for the reprocess/recover/retry routes (PRD T1 items 3/4)."""

    def __init__(
        self,
        document: dict[str, Any] | None,
        *,
        ledger_failure: Exception | None = None,
        rule_failure: Exception | None = None,
    ) -> None:
        self.document = document
        self.records: list[dict[str, Any]] = []
        self.completions: list[dict[str, Any]] = []
        self.rules: list[dict[str, Any]] = []
        self.pins: list[tuple[str, str]] = []
        self.ledger_failure = ledger_failure
        self.rule_failure = rule_failure

    async def get_dataset(self, dataset_id: str) -> dict[str, Any]:
        assert dataset_id == "dataset-a"
        return dict(DATASET)

    async def get_document(self, document_id: str) -> dict[str, Any] | None:
        assert document_id == "document-a"
        return dict(self.document) if self.document else None

    async def record_pipeline_execution(
        self,
        document_id: str,
        dataset_id: str,
        *,
        action: str,
        trigger_source: str = "api",
        triggered_by: str | None = None,
        process_rule_id: str | None = None,
        input_snapshot: dict[str, Any] | None = None,
        connection: Any | None = None,
    ) -> str:
        del triggered_by, connection
        if self.ledger_failure is not None:
            raise self.ledger_failure
        execution_id = f"exec-{len(self.records) + 1}"
        self.records.append(
            {
                "execution_id": execution_id,
                "document_id": document_id,
                "dataset_id": dataset_id,
                "action": action,
                "trigger_source": trigger_source,
                "process_rule_id": process_rule_id,
                "input_snapshot": input_snapshot or {},
            }
        )
        return execution_id

    async def record_process_rule(
        self,
        dataset_id: str,
        *,
        mode: str,
        rules: dict[str, Any],
        created_by: str | None = None,
    ) -> str:
        del created_by
        if self.rule_failure is not None:
            raise self.rule_failure
        rule_id = f"rule-{len(self.rules) + 1}"
        self.rules.append(
            {
                "id": rule_id,
                "dataset_id": dataset_id,
                "mode": mode,
                "rules": rules,
            }
        )
        return rule_id

    async def pin_document_process_rule(
        self,
        document_id: str,
        process_rule_id: str,
        *,
        idle_only: bool = False,
    ) -> bool:
        if self.document is None or self.document.get("document_id") != document_id:
            return False
        if idle_only and self.document.get("status") not in {"completed", "error"}:
            return False
        self.document["process_rule_id"] = process_rule_id
        self.pins.append((document_id, process_rule_id))
        return True

    async def complete_pipeline_execution(
        self,
        execution_id: str,
        *,
        status: str,
        error: str | None = None,
        manifest: dict[str, Any] | list[Any] | None = None,
        connection: Any | None = None,
    ) -> bool:
        del manifest, connection
        self.completions.append(
            {"execution_id": execution_id, "status": status, "error": error}
        )
        return True


class _VerbWorker:
    def __init__(self, *, queued: bool = True, reject_special: bool = False) -> None:
        self.queued = queued
        self.reject_special = reject_special
        self.calls: list[dict[str, Any]] = []

    async def require_safe_special_replay_admission(
        self, _task: Any, _document: dict[str, Any], _dataset: dict[str, Any],
        *, action: str,
    ) -> None:
        del action
        if self.reject_special:
            raise ValidationFailedError("specialized_rebuild_unavailable")

    async def enqueue(self, dataset_id: str, document_id: str, **kwargs: Any) -> bool:
        self.calls.append(
            {"dataset_id": dataset_id, "document_id": document_id, **kwargs}
        )
        return self.queued


def _verb_document(**overrides: Any) -> dict[str, Any]:
    document: dict[str, Any] = {
        "document_id": "document-a",
        "dataset_id": "dataset-a",
        "status": "completed",
        "metadata": {"processing_mode": "text_only"},
    }
    document.update(overrides)
    return document


@pytest.mark.asyncio
@pytest.mark.parametrize("route", [reprocess_document, recover_document, retry_document])
async def test_specialized_replay_rejected_before_execution_receipt(route: Any) -> None:
    database = _VerbDatabase(_verb_document())
    service = _BaseService(database)
    worker = _VerbWorker(reject_special=True)

    for _ in range(2):
        with pytest.raises(HTTPException) as exc_info:
            await route(
                "dataset-a", "document-a", svc=service, worker=worker, user=USER,
            )
        assert exc_info.value.status_code == 409
        assert exc_info.value.detail["code"] == "specialized_rebuild_unavailable"
    assert database.records == []
    assert database.rules == []
    assert worker.calls == []


@pytest.mark.asyncio
@pytest.mark.parametrize("route", [reprocess_document, recover_document, retry_document])
async def test_replay_rechecks_editor_after_specialized_admission(route: Any) -> None:
    database = _VerbDatabase(_verb_document())

    class RevokedService(_BaseService):
        checks = 0

        async def require_dataset_access(self, user, dataset_id, *, required):
            self.checks += 1
            if self.checks == 2:
                raise PermissionDeniedError("editor grant revoked")
            return await super().require_dataset_access(user, dataset_id, required=required)

    service = RevokedService(database)
    worker = _VerbWorker()
    with pytest.raises(HTTPException) as exc_info:
        await route(
            "dataset-a", "document-a", svc=service, worker=worker, user=USER,
        )
    assert exc_info.value.status_code == 403
    assert database.records == []
    assert database.rules == []
    assert worker.calls == []


@pytest.mark.asyncio
async def test_reprocess_route_pins_action_and_submission_snapshot() -> None:
    database = _VerbDatabase(_verb_document())
    service = _BaseService(database)
    worker = _VerbWorker()

    result = await reprocess_document(
        "dataset-a",
        "document-a",
        svc=service,  # type: ignore[arg-type]
        worker=worker,  # type: ignore[arg-type]
        user=USER,
    )

    assert result == {
        "status": "queuing",
        "document_id": "document-a",
        "action": "reprocess",
        "execution_id": "exec-1",
        "job_url": (
            "/api/v1/knowledge/dataset-a/documents/document-a/executions/exec-1"
        ),
    }
    assert worker.calls == [
        {
            "dataset_id": "dataset-a",
            "document_id": "document-a",
            "action": "reprocess",
            "recover_stage": None,
            "execution_id": "exec-1",
        }
    ]
    # The replay snapshot is captured at submission time (addendum §1-T1.3).
    assert database.records == [
        {
            "execution_id": "exec-1",
            "document_id": "document-a",
            "dataset_id": "dataset-a",
            "action": "reprocess",
            "trigger_source": "api",
            "process_rule_id": "rule-1",
            "input_snapshot": {
                "index_config": {},
                "chunking": {},
                "processing_mode": "text_only",
            },
        }
    ]
    assert database.pins == [("document-a", "rule-1")]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "route", [reprocess_document, recover_document, retry_document]
)
async def test_replay_ledger_failure_returns_503_without_enqueue(route: Any) -> None:
    database = _VerbDatabase(
        _verb_document(status="error"),
        ledger_failure=RuntimeError("ledger unavailable"),
    )
    service = _BaseService(database)
    worker = _VerbWorker()

    with pytest.raises(HTTPException) as exc_info:
        await route(
            "dataset-a",
            "document-a",
            svc=service,  # type: ignore[arg-type]
            worker=worker,  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 503
    assert worker.calls == []
    assert database.records == []
    assert database.pins == []


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "route", [reprocess_document, recover_document, retry_document]
)
async def test_replay_rule_failure_returns_503_without_enqueue(route: Any) -> None:
    database = _VerbDatabase(
        _verb_document(status="error"),
        rule_failure=RuntimeError("rule store unavailable"),
    )
    service = _BaseService(database)
    worker = _VerbWorker()

    with pytest.raises(HTTPException) as exc_info:
        await route(
            "dataset-a",
            "document-a",
            svc=service,  # type: ignore[arg-type]
            worker=worker,  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 503
    assert worker.calls == []
    assert database.records == []
    assert database.pins == []


@pytest.mark.asyncio
async def test_recover_route_derives_stage_from_furthest_timestamp() -> None:
    from datetime import datetime, timedelta, timezone

    now = datetime.now(timezone.utc)
    database = _VerbDatabase(
        _verb_document(
            status="error",
            parsing_started_at=now - timedelta(minutes=3),
            splitting_started_at=now - timedelta(minutes=2),
            indexing_started_at=now - timedelta(minutes=1),
        )
    )
    service = _BaseService(database)
    worker = _VerbWorker()

    result = await recover_document(
        "dataset-a",
        "document-a",
        svc=service,  # type: ignore[arg-type]
        worker=worker,  # type: ignore[arg-type]
        user=USER,
    )

    assert result == {
        "status": "queuing",
        "document_id": "document-a",
        "action": "recover",
        "recover_stage": "indexing",
        "execution_id": "exec-1",
        "job_url": (
            "/api/v1/knowledge/dataset-a/documents/document-a/executions/exec-1"
        ),
    }
    assert worker.calls[0]["action"] == "recover"
    assert worker.calls[0]["recover_stage"] == "indexing"


@pytest.mark.asyncio
async def test_recover_route_rejects_documents_still_in_flight() -> None:
    for status in ("parsing", "splitting", "indexing"):
        database = _VerbDatabase(_verb_document(status=status))
        service = _BaseService(database)
        worker = _VerbWorker()

        with pytest.raises(HTTPException) as exc_info:
            await recover_document(
                "dataset-a",
                "document-a",
                svc=service,  # type: ignore[arg-type]
                worker=worker,  # type: ignore[arg-type]
                user=USER,
            )

        assert exc_info.value.status_code == 409
        assert worker.calls == []
        assert database.records == []


@pytest.mark.asyncio
async def test_verb_routes_reject_documents_already_in_durable_queue() -> None:
    """A waiting row belongs to the durable queue: verb routes must 409
    before opening a ledger row, so a re-claim can never swap the pinned
    verb of a queued generation or orphan an execution row."""

    for route in (reprocess_document, recover_document, retry_document):
        database = _VerbDatabase(_verb_document(status="waiting"))
        service = _BaseService(database)
        worker = _VerbWorker()

        with pytest.raises(HTTPException) as exc_info:
            await route(
                "dataset-a",
                "document-a",
                svc=service,  # type: ignore[arg-type]
                worker=worker,  # type: ignore[arg-type]
                user=USER,
            )

        assert exc_info.value.status_code == 409
        assert worker.calls == []
        # No ledger row is opened for a request that can never enqueue.
        assert database.records == []
        assert database.completions == []


@pytest.mark.asyncio
async def test_retry_route_pins_retry_action_on_claim() -> None:
    database = _VerbDatabase(_verb_document(status="error"))
    service = _BaseService(database)
    worker = _VerbWorker()

    result = await retry_document(
        "dataset-a",
        "document-a",
        svc=service,  # type: ignore[arg-type]
        worker=worker,  # type: ignore[arg-type]
        user=USER,
    )

    assert result == {
        "status": "queuing",
        "document_id": "document-a",
        "action": "retry",
        "execution_id": "exec-1",
        "job_url": (
            "/api/v1/knowledge/dataset-a/documents/document-a/executions/exec-1"
        ),
    }
    assert worker.calls[0]["action"] == "retry"
    assert worker.calls[0]["execution_id"] == "exec-1"
    assert database.records[0]["process_rule_id"] == "rule-1"
    assert database.records[0]["input_snapshot"] == {
        "index_config": {},
        "chunking": {},
        "processing_mode": "text_only",
    }
    assert database.pins == [("document-a", "rule-1")]


@pytest.mark.asyncio
async def test_verb_routes_report_missing_document_with_404() -> None:
    for route in (reprocess_document, recover_document, retry_document):
        service = _BaseService(_VerbDatabase(None))
        worker = _VerbWorker()

        with pytest.raises(HTTPException) as exc_info:
            await route(
                "dataset-a",
                "document-a",
                svc=service,  # type: ignore[arg-type]
                worker=worker,  # type: ignore[arg-type]
                user=USER,
            )

        assert exc_info.value.status_code == 404
        assert worker.calls == []


@pytest.mark.asyncio
async def test_verb_claim_failure_closes_execution_row_as_error() -> None:
    for route in (reprocess_document, recover_document, retry_document):
        database = _VerbDatabase(_verb_document())
        service = _BaseService(database)
        worker = _VerbWorker(queued=False)

        with pytest.raises(HTTPException) as exc_info:
            await route(
                "dataset-a",
                "document-a",
                svc=service,  # type: ignore[arg-type]
                worker=worker,  # type: ignore[arg-type]
                user=USER,
            )

        assert exc_info.value.status_code == 409
        # The submission-time ledger row must never be left dangling running.
        assert database.completions == [
            {
                "execution_id": "exec-1",
                "status": "error",
                "error": database.completions[0]["error"],
            }
        ]
        assert database.completions[0]["error"]


@pytest.mark.asyncio
async def test_batch_reindex_persists_a_durable_selection(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service = _BaseService(_VersionDatabase(active=True))
    calls: list[dict[str, Any]] = []

    class Store:
        async def create_operation(self, **kwargs: Any) -> dict[str, Any]:
            calls.append(kwargs)
            return {"operation_id": "batch-a", "status": "pending", "total_count": 3}

    monkeypatch.setattr(knowledge_routes, "_document_batch_store", lambda _svc: Store())

    result = await batch_reindex_documents(
        "dataset-a",
        payload=BatchReindexSchema(document_ids=["document-a", "document-b", "document-c"]),
        svc=service,  # type: ignore[arg-type]
        user=USER,
    )

    assert result == {"operation_id": "batch-a", "status": "pending", "total_count": 3}
    assert calls[0]["document_ids"] == ["document-a", "document-b", "document-c"]
    assert calls[0]["all_documents"] is False


@pytest.mark.asyncio
async def test_batch_reindex_allows_active_bm25_v2(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class Service(_BaseService):
        async def require_dataset_access(
            self,
            _user: UserContext,
            _dataset_id: str,
            *,
            required: str,
        ) -> dict[str, Any]:
            assert required == "editor"
            return {
                **DATASET,
                "index_config": {
                    "retrieval": {
                        "lexical": {
                            "active_version": "bm25_v2",
                            "bm25_v2": {"shadow_write_enabled": True},
                        }
                    }
                },
            }

    class Store:
        async def create_operation(self, **_kwargs: Any) -> dict[str, Any]:
            return {"operation_id": "batch-a", "status": "pending", "total_count": 1}

    monkeypatch.setattr(knowledge_routes, "_document_batch_store", lambda _svc: Store())

    result = await batch_reindex_documents(
        "dataset-a",
        payload=BatchReindexSchema(document_ids=["document-a"]),
        svc=Service(_VersionDatabase(active=True)),  # type: ignore[arg-type]
        user=USER,
    )

    assert result == {"operation_id": "batch-a", "status": "pending", "total_count": 1}


@pytest.mark.asyncio
async def test_batch_reindex_all_documents_uses_server_side_selector(
    monkeypatch: pytest.MonkeyPatch,
) -> None:

    class Service(_BaseService):
        async def list_documents(self, *_args: Any, **_kwargs: Any) -> list[dict[str, Any]]:
            raise AssertionError(
                "all_documents must not use the capped single-page listing"
            )

    calls: list[dict[str, Any]] = []

    class Store:
        async def create_operation(self, **kwargs: Any) -> dict[str, Any]:
            calls.append(kwargs)
            return {"operation_id": "batch-all", "status": "pending", "total_count": 250}

    monkeypatch.setattr(knowledge_routes, "_document_batch_store", lambda _svc: Store())
    result = await batch_reindex_documents(
        "dataset-a",
        payload=BatchReindexSchema(all_documents=True),
        svc=Service(_VersionDatabase(active=True)),  # type: ignore[arg-type]
        user=USER,
    )

    assert result["total_count"] == 250
    assert calls[0]["document_ids"] == []
    assert calls[0]["all_documents"] is True


@pytest.mark.asyncio
async def test_batch_create_returns_accurate_queued_and_skipped_documents() -> None:
    class Service(_BaseService):
        async def batch_create_documents(self, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
            return {
                "documents": [
                    {"document_id": "document-a", "title": "A"},
                    {"document_id": "document-b", "title": "B"},
                ],
                "created_count": 2,
            }

    class Worker:
        async def enqueue(
            self, _dataset_id: str, document_id: str, **kwargs: Any
        ) -> bool:
            # First-generation ingest: no verb is pinned on batch create.
            assert kwargs.get("action") is None
            return document_id == "document-a"

    result = await batch_create_documents(
        "dataset-a",
        payload=DocumentBatchCreateSchema(
            documents=[
                {"title": "A", "content": "alpha"},
                {"title": "B", "content": "beta"},
            ]
        ),
        svc=Service(_VersionDatabase(active=True)),  # type: ignore[arg-type]
        worker=Worker(),  # type: ignore[arg-type]
        user=USER,
    )

    assert result["status"] == "partial"
    assert result["queued_count"] == 1
    assert result["documents"] == [{"document_id": "document-a", "title": "A"}]
    assert result["skipped_document_ids"] == ["document-b"]


class _Transaction:
    def __init__(self, events: list[str]) -> None:
        self.events = events

    async def __aenter__(self) -> None:
        self.events.append("transaction-enter")

    async def __aexit__(self, *_args: Any) -> None:
        self.events.append("transaction-exit")


class _LeaseConnection:
    def __init__(self, events: list[str]) -> None:
        self.events = events

    def transaction(self) -> _Transaction:
        return _Transaction(self.events)


class _RestoreDatabase:
    def __init__(self, *, lifecycle_pending: bool = False) -> None:
        metadata: dict[str, Any] = {}
        if lifecycle_pending:
            metadata["_document_lifecycle_reindex"] = {
                "status": "pending",
                "desired_enabled": True,
                "desired_archived": False,
            }
        self.document = {
            "document_id": "document-a",
            "dataset_id": "dataset-a",
            "enabled": True,
            "archived": False,
            "status": "completed",
            "progress": 100,
            "content": "current content",
            "current_version": 1,
            "title": "Document A",
            "metadata": metadata,
        }
        self.events: list[str] = []
        self.version_reads = 0
        self.created_versions: list[dict[str, Any]] = []
        self.restore_version_override: dict[str, Any] | None = None

    @asynccontextmanager
    async def document_index_update_lease(self, dataset_id: str, document_id: str):
        assert (dataset_id, document_id) == ("dataset-a", "document-a")
        self.events.append("lease-enter")
        try:
            yield _LeaseConnection(self.events)
        finally:
            self.events.append("lease-exit")

    async def get_dataset(
        self,
        dataset_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any]:
        assert dataset_id == "dataset-a"
        assert connection is not None
        self.events.append("dataset-read")
        return {**DATASET, "index_config": {"chunking": {"mode": "automatic"}}}

    async def get_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any]:
        assert document_id == "document-a"
        assert connection is not None
        self.events.append("document-read")
        return dict(self.document)

    async def get_document_version(
        self,
        document_id: str,
        version_number: int,
    ) -> dict[str, Any]:
        assert (document_id, version_number) == ("document-a", 1)
        self.version_reads += 1
        self.events.append("version-read")
        if self.restore_version_override is not None:
            return dict(self.restore_version_override)
        return {
            "document_id": document_id,
            "version_number": version_number,
            "content": "restored content",
            "title": "Restored title",
            "metadata": {"source": "version"},
        }

    async def create_document_version(self, **kwargs: Any) -> dict[str, Any]:
        assert kwargs.pop("connection") is not None
        self.created_versions.append(dict(kwargs))
        self.events.append(f"version-create:{kwargs['change_type']}")
        version_number = len(self.created_versions) + 1
        if kwargs.get("activate", True):
            self.document["current_version"] = version_number
        return {"version_number": version_number}

    async def set_pending_restore_version(
        self,
        document_id: str,
        dataset_id: str,
        previous_version_number: int,
        candidate_version_number: int,
        execution_id: str,
        *,
        connection: Any,
    ) -> bool:
        assert (document_id, dataset_id) == ("document-a", "dataset-a")
        assert connection is not None and self.document["status"] == "waiting"
        self.document["metadata"]["_document_pending_restore_version"] = {
            "previous_version": previous_version_number,
            "candidate_version": candidate_version_number,
        }
        self.document["metadata"]["_document_ingest_action"] = "reprocess"
        self.document["metadata"]["_document_pipeline_execution_id"] = execution_id
        self.events.append("candidate-bound")
        return True

    async def record_process_rule(self, *_args: Any, **kwargs: Any) -> str:
        assert kwargs["connection"] is not None
        assert kwargs["rules"]["processing_mode"] == "text_only"
        self.events.append("rule-recorded")
        return "rule-restore"

    async def pin_document_process_rule(self, _document_id: str, rule_id: str, **kwargs: Any) -> bool:
        assert kwargs["connection"] is not None
        self.document["process_rule_id"] = rule_id
        self.events.append("rule-pinned")
        return True

    async def record_pipeline_execution(self, *_args: Any, **kwargs: Any) -> str:
        assert kwargs["connection"] is not None
        assert kwargs["action"] == "reprocess"
        assert kwargs["process_rule_id"] == "rule-restore"
        self.events.append("execution-recorded")
        return "exec-restore"

    async def update_document_status(
        self,
        document_id: str,
        *,
        status: str,
        progress: int,
        error: str,
        connection: Any,
    ) -> None:
        assert document_id == "document-a"
        assert connection is not None
        self.document.update(status=status, progress=progress, error=error)
        self.events.append(f"status:{status}")

    async def update_document_content(
        self,
        document_id: str,
        content: str,
        *,
        connection: Any,
    ) -> None:
        assert document_id == "document-a"
        assert connection is not None
        self.document["content"] = content
        self.events.append("content-update")


class _RestoreWorker:
    def __init__(self, database: _RestoreDatabase, *, reject_special: bool = False) -> None:
        self.database = database
        self.reject_special = reject_special
        self.calls: list[tuple[str, str]] = []

    async def require_safe_restore_admission(
        self, _task: Any, _dataset: dict[str, Any], *, connection: Any,
    ) -> None:
        assert connection is not None
        self.database.events.append("specialized-check")
        if self.reject_special:
            raise ValidationFailedError("specialized_rebuild_unavailable")

    async def enqueue_claimed(self, dataset_id: str, document_id: str) -> None:
        assert self.database.document["status"] == "waiting"
        assert self.database.document["content"] == "current content"
        assert self.database.document["metadata"]["_document_pending_restore_version"] == {
            "previous_version": 2,
            "candidate_version": 3,
        }
        assert self.database.document["metadata"]["_document_pipeline_execution_id"] == "exec-restore"
        assert self.database.events[-1] == "lease-exit"
        self.calls.append((dataset_id, document_id))
        self.database.events.append("enqueue-claimed")


@pytest.mark.asyncio
async def test_version_restore_queues_hidden_candidate_without_changing_serving_content() -> None:
    database = _RestoreDatabase()
    service = _BaseService(database)
    worker = _RestoreWorker(database)

    result = await restore_document_version(
        "dataset-a",
        "document-a",
        1,
        payload=VersionRestoreRequest(reason="rollback"),
        svc=service,  # type: ignore[arg-type]
        worker=worker,  # type: ignore[arg-type]
        user=USER,
    )

    assert result["status"] == "pending"
    assert database.document["content"] == "current content"
    assert database.document["status"] == "waiting"
    assert database.document["current_version"] == 1
    assert database.document["metadata"]["_document_pending_restore_version"] == {
        "previous_version": 2,
        "candidate_version": 3,
    }
    assert [version["change_type"] for version in database.created_versions] == [
        "pending_before_restore",
        "pending_restore",
    ]
    assert database.events == [
        "lease-enter",
        "dataset-read",
        "transaction-enter",
        "document-read",
        "version-read",
        "specialized-check",
        "version-create:pending_before_restore",
        "status:waiting",
        "version-create:pending_restore",
        "rule-recorded",
        "rule-pinned",
        "execution-recorded",
        "candidate-bound",
        "transaction-exit",
        "lease-exit",
        "enqueue-claimed",
    ]


@pytest.mark.asyncio
async def test_version_restore_refuses_specialized_points_before_candidate_or_execution() -> None:
    database = _RestoreDatabase()
    service = _BaseService(database)
    worker = _RestoreWorker(database, reject_special=True)

    with pytest.raises(HTTPException) as exc_info:
        await restore_document_version(
            "dataset-a", "document-a", 1, payload=VersionRestoreRequest(),
            svc=service, worker=worker, user=USER,  # type: ignore[arg-type]
        )
    assert exc_info.value.status_code == 409
    assert exc_info.value.detail["code"] == "specialized_rebuild_unavailable"
    assert database.version_reads == 1
    assert database.created_versions == []
    assert database.document["content"] == "current content"
    assert worker.calls == []


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["text", "hierarchy"])
async def test_version_restore_queues_verified_special_source_without_text_fallback(kind: str) -> None:
    database = _RestoreDatabase()
    restored_content = "restored hierarchy content"
    digest = hashlib.sha256(restored_content.encode()).hexdigest()
    index_config = {"chunking": {"mode": "automatic"}}
    database.restore_version_override = {
        "document_id": "document-a", "version_number": 1,
        "content": restored_content, "content_hash": digest,
        "metadata": {"_special_source_manifest": {
            "source_kind": kind, "source_hash": digest,
            "content_hash": digest, "index_config": index_config,
            "original_source_key": "", "page_texts": [], "objects": {},
        }},
    }
    service = _BaseService(database)
    worker = _RestoreWorker(database, reject_special=True)

    result = await restore_document_version(
        "dataset-a", "document-a", 1, payload=VersionRestoreRequest(),
        svc=service, worker=worker, user=USER,  # type: ignore[arg-type]
    )

    assert result["status"] == "pending"
    assert "specialized-check" not in database.events
    assert database.created_versions[1]["metadata"] == database.restore_version_override["metadata"]
    assert worker.calls == [("dataset-a", "document-a")]


@pytest.mark.asyncio
async def test_version_restore_rejects_unverifiable_historical_special_source() -> None:
    database = _RestoreDatabase()
    database.restore_version_override = {
        "document_id": "document-a", "version_number": 1,
        "content": "restored content", "metadata": {
            "_special_source_manifest": {"source_kind": "vision"},
        },
    }
    worker = _RestoreWorker(database)

    with pytest.raises(HTTPException) as exc_info:
        await restore_document_version(
            "dataset-a", "document-a", 1, payload=VersionRestoreRequest(),
            svc=_BaseService(database), worker=worker, user=USER,  # type: ignore[arg-type]
        )

    assert exc_info.value.status_code == 409
    assert database.created_versions == []
    assert worker.calls == []


@pytest.mark.asyncio
async def test_version_restore_rejects_lifecycle_pending_document_before_content_read() -> None:
    database = _RestoreDatabase(lifecycle_pending=True)
    service = _BaseService(database)
    worker = _RestoreWorker(database)

    with pytest.raises(HTTPException) as exc_info:
        await restore_document_version(
            "dataset-a",
            "document-a",
            1,
            payload=VersionRestoreRequest(),
            svc=service,  # type: ignore[arg-type]
            worker=worker,  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 404
    assert database.version_reads == 0
    assert database.document["content"] == "current content"
    assert database.created_versions == []
    assert worker.calls == []


@pytest.mark.asyncio
async def test_version_restore_rechecks_editor_after_waiting_for_document_lease() -> None:
    database = _RestoreDatabase()

    class RevokedService(_BaseService):
        access_checks = 0

        async def require_dataset_access(self, user, dataset_id, *, required):
            self.access_checks += 1
            if self.access_checks == 2:
                raise PermissionDeniedError("editor grant was revoked")
            return await super().require_dataset_access(user, dataset_id, required=required)

    service = RevokedService(database)
    worker = _RestoreWorker(database)
    with pytest.raises(HTTPException) as exc_info:
        await restore_document_version(
            "dataset-a",
            "document-a",
            1,
            payload=VersionRestoreRequest(),
            svc=service,  # type: ignore[arg-type]
            worker=worker,  # type: ignore[arg-type]
            user=USER,
        )
    assert exc_info.value.status_code == 403
    assert database.version_reads == 0
    assert database.created_versions == []
    assert database.document["content"] == "current content"
    assert worker.calls == []
