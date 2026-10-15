from __future__ import annotations

import asyncio
import contextlib
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException
from knowledge_service.api.routes.knowledge import dedupe_segments
from knowledge_service.auth.user_context import UserContext
from knowledge_service.persistence.database import (
    CONFLUENCE_SYNC_GENERATION_KEY,
    DOCUMENT_UPLOAD_FAILED_KEY,
    DOCUMENT_UPLOAD_GENERATION_KEY,
    dataset_index_deletion_fence,
    make_dataset_index_deletion_fence,
)
from knowledge_service.services.knowledge.document_service import (
    DocumentService,
    _require_dataset_index_readable,
)
from knowledge_service.services.knowledge.hierarchical_indexer import (
    HierarchicalIndexer,
    HierarchicalSegment,
    IndexLevel,
)
from knowledge_service.services.knowledge.vector_store import VectorStore, VectorStoreError
from qdrant_client.http import models as qmodels


@contextlib.asynccontextmanager
async def _allow_dataset_lifecycle(*_args: Any, **_kwargs: Any):
    yield


class RecordingVectorStore:
    def __init__(self, *, fail_collection: str | None = None) -> None:
        self.fail_collection = fail_collection
        self.deletes: list[tuple[str, list[str], str | None, str | None]] = []
        self.document_deletes: list[tuple[str, str, str]] = []
        self.upserts: list[tuple[str, list[Any]]] = []

    async def ensure_collection(
        self,
        dataset_id: str,
        dimension: int,
        collection_name: str | None = None,
        **_kwargs: Any,
    ) -> str:
        return collection_name or f"kb_{dataset_id}_{dimension}"

    async def upsert(self, collection_name: str, points: list[Any]) -> None:
        self.upserts.append((collection_name, list(points)))

    async def delete_points(
        self,
        collection_name: str,
        point_ids: list[str],
        tenant_id: str | None = None,
        dataset_id: str | None = None,
    ) -> None:
        self.deletes.append((collection_name, list(point_ids), tenant_id, dataset_id))
        if collection_name == self.fail_collection:
            raise RuntimeError("vector deletion failed")

    async def delete_document_points(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        lifecycle_lease_held: bool = False,
    ) -> list[str]:
        assert lifecycle_lease_held is True
        self.document_deletes.append((tenant_id, dataset_id, document_id))
        if self.fail_collection == "document-sweep":
            raise RuntimeError("vector deletion failed")
        return ["custom-base", "custom-base_sections", "custom-base_summary"]


class DeleteDatabase:
    def __init__(self) -> None:
        self.dataset = {
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
            "collection_name": "custom-base",
            "index_config": {},
        }
        self.segments = [
            {
                "segment_id": "paragraph-row",
                "vector_id": "paragraph-vector",
                "level": int(IndexLevel.PARAGRAPH),
            },
            {
                "segment_id": "section-row",
                "vector_id": "section-vector",
                "level": int(IndexLevel.SECTION),
            },
            {
                "segment_id": "legacy-summary-row",
                "vector_id": "legacy-summary-vector",
                "level": int(IndexLevel.DOCUMENT),
            },
        ]
        self.summary = {"document_id": "document-a", "vector_id": "summary-vector"}
        self.deleted = False

    @contextlib.asynccontextmanager
    async def dataset_index_delete_lease(self, dataset_id: str):
        assert dataset_id == "dataset-a"
        yield self

    async def set_dataset_index_deletion_fence(
        self,
        dataset_id: str,
        *,
        operation: str,
        target_id: str,
        connection: Any,
    ) -> tuple[dict[str, Any], bool]:
        assert (dataset_id, operation, target_id, connection) == (
            "dataset-a",
            "document_delete",
            "document-a",
            self,
        )
        return self.dataset, True

    async def clear_dataset_index_deletion_fence(
        self,
        dataset_id: str,
        *,
        operation: str,
        target_id: str,
        connection: Any,
    ) -> bool:
        assert (dataset_id, operation, target_id, connection) == (
            "dataset-a",
            "document_delete",
            "document-a",
            self,
        )
        return True

    async def get_dataset(
        self,
        dataset_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        assert connection in (None, self)
        return self.dataset if dataset_id == "dataset-a" else None

    async def get_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        assert connection in (None, self)
        if document_id != "document-a":
            return None
        return {
            "document_id": document_id,
            "dataset_id": "dataset-a",
            "status": "completed",
            "enabled": True,
            "archived": False,
            "metadata": {},
        }

    async def list_segments(
        self,
        *,
        dataset_id: str,
        document_id: str,
        limit: int,
        offset: int,
    ) -> list[dict[str, Any]]:
        assert dataset_id == "dataset-a"
        assert document_id == "document-a"
        return self.segments[offset : offset + limit]

    async def get_document_summary(self, document_id: str) -> dict[str, Any] | None:
        return self.summary if document_id == "document-a" else None

    async def delete_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        assert document_id == "document-a"
        assert connection is self
        self.deleted = True
        return True


class FencedDeleteDatabase(DeleteDatabase):
    def __init__(self) -> None:
        super().__init__()
        self.document_exists = True
        self.document_dataset_id = "dataset-a"
        self.document_status = "completed"
        self.document_enabled = True
        self.document_archived = False
        self.document_metadata: dict[str, Any] = {}
        self.delete_result = True
        self.delete_error: Exception | None = None
        self.fail_clear_once = False
        self.clear_calls = 0

    async def set_dataset_index_deletion_fence(
        self,
        dataset_id: str,
        *,
        operation: str,
        target_id: str,
        connection: Any,
    ) -> tuple[dict[str, Any], bool]:
        assert dataset_id == "dataset-a"
        assert connection is self
        requested = make_dataset_index_deletion_fence(operation, target_id)
        existing = dataset_index_deletion_fence(self.dataset)
        if existing is not None and existing != requested:
            raise RuntimeError("another dataset index deletion target is already pending")
        created = existing is None
        if created:
            self.dataset["index_config"] = {
                "retrieval": {"_index_deletion_fence": requested}
            }
        return dict(self.dataset), created

    async def clear_dataset_index_deletion_fence(
        self,
        dataset_id: str,
        *,
        operation: str,
        target_id: str,
        connection: Any,
    ) -> bool:
        assert dataset_id == "dataset-a"
        assert connection is self
        self.clear_calls += 1
        if self.fail_clear_once and self.clear_calls == 1:
            raise RuntimeError("clear failed")
        requested = make_dataset_index_deletion_fence(operation, target_id)
        if dataset_index_deletion_fence(self.dataset) != requested:
            return False
        self.dataset["index_config"] = {"retrieval": {}}
        return True

    async def get_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        assert connection in (None, self)
        if document_id != "document-a" or not self.document_exists:
            return None
        return {
            "document_id": document_id,
            "dataset_id": self.document_dataset_id,
            "status": self.document_status,
            "enabled": self.document_enabled,
            "archived": self.document_archived,
            "metadata": dict(self.document_metadata),
        }

    async def delete_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        assert document_id == "document-a"
        assert connection is self
        if self.delete_error is not None:
            raise self.delete_error
        if not self.delete_result:
            return False
        self.document_exists = False
        self.deleted = True
        return True


class PartialDocumentSweepStore(RecordingVectorStore):
    def __init__(self) -> None:
        super().__init__()
        self.touched: list[str] = []

    async def delete_document_points(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        lifecycle_lease_held: bool = False,
    ) -> list[str]:
        assert lifecycle_lease_held is True
        self.document_deletes.append((tenant_id, dataset_id, document_id))
        self.touched.append("custom-base")
        raise RuntimeError("second collection delete failed")


class FencedSegmentDatabase(FencedDeleteDatabase):
    def __init__(self) -> None:
        super().__init__()
        self.segment_exists = True
        self.segment_delete_result = True
        self.segment_delete_error: Exception | None = None
        self.segment_delete_calls = 0
        self.refresh_calls: list[str] = []
        self.segment_content_type = "text"
        self.segment_updates: list[dict[str, Any]] = []

    async def get_segment(
        self,
        segment_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        assert connection in (None, self)
        if segment_id != "segment-a" or not self.segment_exists:
            return None
        return {
            "segment_id": "segment-a",
            "vector_id": "point-a",
            "document_id": "document-a",
            "dataset_id": "dataset-a",
            "content_type": self.segment_content_type,
            "enabled": True,
        }

    async def update_segment_fields(
        self,
        segment_id: str,
        fields: dict[str, Any],
        *,
        connection: Any | None = None,
    ) -> None:
        assert segment_id == "segment-a"
        assert connection is self
        self.segment_updates.append(dict(fields))

    async def delete_segment(
        self,
        segment_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        assert segment_id == "segment-a"
        assert connection is self
        self.segment_delete_calls += 1
        if self.segment_delete_error is not None:
            raise self.segment_delete_error
        if not self.segment_delete_result:
            return False
        self.segment_exists = False
        return True

    async def refresh_document_segment_count(self, document_id: str) -> None:
        self.refresh_calls.append(document_id)


class RecordingSegmentSweepStore(RecordingVectorStore):
    def __init__(self, *, failure: Exception | None = None) -> None:
        super().__init__()
        self.failure = failure
        self.segment_sweeps: list[dict[str, Any]] = []
        self.visibility_updates: list[dict[str, Any]] = []

    async def delete_segment_points(self, **kwargs: Any) -> list[str]:
        self.segment_sweeps.append(dict(kwargs))
        if self.failure is not None:
            raise self.failure
        return ["custom-base", "custom-base_sections", "old-generation"]

    async def set_segment_payload_enabled(self, **kwargs: Any) -> list[str]:
        self.visibility_updates.append(dict(kwargs))
        if self.failure is not None:
            raise self.failure
        return ["custom-base", "custom-base_sections", "old-generation"]


class DedupeKnowledgeService:
    def __init__(
        self,
        database: FencedSegmentDatabase,
        vector_store: RecordingSegmentSweepStore,
    ) -> None:
        self.db = database
        self.vector_store = vector_store
        self.document_service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
        self.document_service._ks = self  # type: ignore[assignment]

    async def require_dataset_access(
        self,
        _user: UserContext,
        dataset_id: str,
        *,
        required: str,
    ) -> dict[str, Any]:
        assert dataset_id == "dataset-a"
        assert required in {"owner", "editor"}
        return self.db.dataset

    async def delete_segment(
        self,
        user: UserContext,
        dataset_id: str,
        segment_id: str,
    ) -> bool:
        return await self.document_service.delete_segment(
            user,
            dataset_id,
            segment_id,
        )


def _install_dedupe_rows(database: FencedSegmentDatabase) -> None:
    async def list_segments(*_args: Any, **_kwargs: Any) -> list[dict[str, Any]]:
        rows = _dedupe_rows()
        return rows if database.segment_exists else rows[:1]

    database.list_segments = list_segments  # type: ignore[method-assign]


class LifecycleBarrier:
    """In-memory model of the PostgreSQL shared/exclusive advisory barrier."""

    def __init__(self) -> None:
        self._condition = asyncio.Condition()
        self._readers = 0
        self._writer = False
        self.exclusive_requested = asyncio.Event()
        self.exclusive_entered = asyncio.Event()

    @contextlib.asynccontextmanager
    async def shared(self):
        async with self._condition:
            if self._writer:
                raise RuntimeError(
                    "dataset index deletion is in progress; refusing a queued "
                    "vector write"
                )
            self._readers += 1
        try:
            yield
        finally:
            async with self._condition:
                self._readers -= 1
                self._condition.notify_all()

    @contextlib.asynccontextmanager
    async def exclusive(self):
        self.exclusive_requested.set()
        async with self._condition:
            while self._writer or self._readers:
                await self._condition.wait()
            self._writer = True
            self.exclusive_entered.set()
        try:
            yield
        finally:
            async with self._condition:
                self._writer = False
                self._condition.notify_all()


class ConcurrentSegmentDatabase(FencedSegmentDatabase):
    def __init__(self) -> None:
        super().__init__()
        self.barrier = LifecycleBarrier()

    @contextlib.asynccontextmanager
    async def dataset_index_write_lease(
        self,
        dataset_id: str,
        document_ids: list[str],
        **_kwargs: Any,
    ):
        assert dataset_id == "dataset-a"
        assert document_ids == ["document-a"]
        async with self.barrier.shared():
            yield

    @contextlib.asynccontextmanager
    async def dataset_index_delete_lease(self, dataset_id: str):
        assert dataset_id == "dataset-a"
        async with self.barrier.exclusive():
            yield self


class BarrierQdrantClient:
    def __init__(self) -> None:
        self.points = {
            "custom-base": {"point-a"},
            "custom-base_sections": {"point-a"},
            "old-generation": {"point-a"},
        }
        self.upsert_started = asyncio.Event()
        self.allow_upsert = asyncio.Event()
        self.block_upsert = False
        self.delete_started = asyncio.Event()
        self.allow_delete = asyncio.Event()
        self.block_delete = False
        self.upsert_calls = 0

    async def get_collections(self) -> SimpleNamespace:
        return SimpleNamespace(
            collections=[SimpleNamespace(name=name) for name in self.points]
        )

    async def get_collection(self, _collection_name: str) -> SimpleNamespace:
        return SimpleNamespace(
            config=SimpleNamespace(
                params=SimpleNamespace(
                    sparse_vectors={},
                    vectors=SimpleNamespace(size=2),
                ),
                metadata={
                    "knowledge_scope": {
                        "schema_version": 1,
                        "tenant_id": "tenant-a",
                        "dataset_id": "dataset-a",
                    }
                },
                strict_mode_config=None,
            ),
            payload_schema={},
        )

    async def upsert(self, **kwargs: Any) -> SimpleNamespace:
        self.upsert_calls += 1
        self.upsert_started.set()
        if self.block_upsert:
            await self.allow_upsert.wait()
        self.points[kwargs["collection_name"]].update(
            str(point.id) for point in kwargs["points"]
        )
        return SimpleNamespace(status="completed")

    async def delete(self, **kwargs: Any) -> SimpleNamespace:
        self.delete_started.set()
        if self.block_delete:
            await self.allow_delete.wait()
        point_filter = kwargs["points_selector"].filter
        ids = _positive_filter_point_ids(point_filter)
        self.points[kwargs["collection_name"]].difference_update(ids)
        return SimpleNamespace(status="completed")

    async def close(self) -> None:
        return None


class DocumentKnowledgeService:
    def __init__(self, database: DeleteDatabase, vector_store: RecordingVectorStore) -> None:
        self.database = database
        self.vector_store = vector_store

    async def require_dataset_access(
        self,
        _user: UserContext,
        dataset_id: str,
        *,
        required: str,
    ) -> dict[str, Any]:
        assert required in {"viewer", "editor"}
        dataset = await self.database.get_dataset(dataset_id)
        assert dataset is not None
        return dataset

    async def _effective_dataset_permission(self, _dataset, _user):
        return "viewer"


class StubEmbedder:
    dimension = 3

    async def embed_documents(self, texts: list[str]) -> list[list[float]]:
        return [[0.1, 0.2, 0.3] for _ in texts]


def _positive_filter_point_ids(point_filter: qmodels.Filter) -> set[str]:
    ids: set[str] = set()
    for condition in [*(point_filter.must or []), *(point_filter.should or [])]:
        if isinstance(condition, qmodels.HasIdCondition):
            ids.update(str(point_id) for point_id in condition.has_id)
        elif isinstance(condition, qmodels.Filter):
            ids.update(_positive_filter_point_ids(condition))
    return ids


def _matches_qdrant_condition(
    payload: dict[str, Any],
    condition: Any,
    *,
    point_id: str | None = None,
) -> bool:
    if isinstance(condition, qmodels.Filter):
        return _matches_qdrant_filter(payload, condition, point_id=point_id)
    if isinstance(condition, qmodels.FieldCondition):
        return payload.get(condition.key) == condition.match.value
    if isinstance(condition, qmodels.IsEmptyCondition):
        key = condition.is_empty.key
        return key not in payload or payload.get(key) in (None, "", [])
    if isinstance(condition, qmodels.HasIdCondition):
        return point_id is not None and point_id in {
            str(candidate) for candidate in condition.has_id
        }
    raise AssertionError(f"unsupported test filter condition: {condition!r}")


def _matches_qdrant_filter(
    payload: dict[str, Any],
    point_filter: qmodels.Filter,
    *,
    point_id: str | None = None,
) -> bool:
    must = list(point_filter.must or [])
    should = list(point_filter.should or [])
    must_not = list(point_filter.must_not or [])
    return (
        all(
            _matches_qdrant_condition(payload, item, point_id=point_id)
            for item in must
        )
        and (
            not should
            or any(
                _matches_qdrant_condition(payload, item, point_id=point_id)
                for item in should
            )
        )
        and not any(
            _matches_qdrant_condition(payload, item, point_id=point_id)
            for item in must_not
        )
    )


class StatefulQdrantClient:
    """Small stateful Qdrant double exercising real VectorStore control flow."""

    def __init__(self) -> None:
        self.collections: dict[str, dict[str, Any]] = {}

    def seed_collection(
        self,
        name: str,
        *,
        dimension: int = 2,
        metadata: dict[str, Any] | None = None,
        points: list[qmodels.PointStruct] | None = None,
    ) -> None:
        self.collections[name] = {
            "info": SimpleNamespace(
                config=SimpleNamespace(
                    params=SimpleNamespace(
                        vectors=qmodels.VectorParams(
                            size=dimension,
                            distance=qmodels.Distance.COSINE,
                        ),
                        sparse_vectors={
                            "bm25": qmodels.SparseVectorParams(
                                modifier=qmodels.Modifier.IDF
                            )
                        },
                    ),
                    metadata=dict(metadata or {}),
                    strict_mode_config=None,
                ),
                payload_schema={},
            ),
            "points": {str(point.id): point for point in points or []},
        }

    async def collection_exists(self, *, collection_name: str) -> bool:
        return collection_name in self.collections

    async def create_collection(self, **kwargs: Any) -> bool:
        name = kwargs["collection_name"]
        if name in self.collections:
            return False
        vectors = kwargs["vectors_config"]
        self.collections[name] = {
            "info": SimpleNamespace(
                config=SimpleNamespace(
                    params=SimpleNamespace(
                        vectors=vectors,
                        sparse_vectors=dict(kwargs.get("sparse_vectors_config") or {}),
                    ),
                    metadata=dict(kwargs.get("metadata") or {}),
                    strict_mode_config=None,
                ),
                payload_schema={},
            ),
            "points": {},
        }
        return True

    async def get_collection(self, collection_name: str) -> SimpleNamespace:
        return self.collections[collection_name]["info"]

    async def update_collection(self, **kwargs: Any) -> bool:
        state = self.collections[kwargs["collection_name"]]
        info = state["info"]
        info.config.metadata.update(kwargs.get("metadata") or {})
        info.config.params.sparse_vectors.update(
            kwargs.get("sparse_vectors_config") or {}
        )
        return True

    async def create_payload_index(self, **_kwargs: Any) -> bool:
        return True

    async def count(self, **kwargs: Any) -> SimpleNamespace:
        points = self.collections[kwargs["collection_name"]]["points"].values()
        point_filter = kwargs.get("count_filter")
        count = sum(
            1
            for point in points
            if point_filter is None
            or _matches_qdrant_filter(dict(point.payload or {}), point_filter)
        )
        return SimpleNamespace(count=count)

    async def upsert(self, **kwargs: Any) -> SimpleNamespace:
        stored = self.collections[kwargs["collection_name"]]["points"]
        for point in kwargs["points"]:
            stored[str(point.id)] = point
        return SimpleNamespace(status="completed")

    async def get_collections(self) -> SimpleNamespace:
        return SimpleNamespace(
            collections=[SimpleNamespace(name=name) for name in self.collections]
        )

    async def delete(self, **kwargs: Any) -> SimpleNamespace:
        stored = self.collections[kwargs["collection_name"]]["points"]
        point_filter = kwargs["points_selector"].filter
        for point_id, point in list(stored.items()):
            if _matches_qdrant_filter(
                dict(point.payload or {}),
                point_filter,
                point_id=point_id,
            ):
                del stored[point_id]
        return SimpleNamespace(status="completed")

    async def delete_collection(self, *, collection_name: str) -> bool:
        return self.collections.pop(collection_name, None) is not None

    async def close(self) -> None:
        return None


@pytest.mark.asyncio
async def test_document_delete_removes_base_section_and_summary_vectors() -> None:
    database = DeleteDatabase()
    vector_store = RecordingVectorStore()
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]
    user = UserContext(user_id="editor-a", tenant_id="tenant-a")

    assert await service.delete_document(user, "dataset-a", "document-a") is True

    assert vector_store.document_deletes == [
        ("tenant-a", "dataset-a", "document-a")
    ]
    assert database.deleted is True


@pytest.mark.asyncio
async def test_document_delete_keeps_database_row_when_derived_cleanup_fails() -> None:
    database = DeleteDatabase()
    vector_store = RecordingVectorStore(fail_collection="document-sweep")
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]
    user = UserContext(user_id="editor-a", tenant_id="tenant-a")

    with pytest.raises(RuntimeError, match="vector deletion failed"):
        await service.delete_document(user, "dataset-a", "document-a")

    assert database.deleted is False
    assert vector_store.document_deletes == [
        ("tenant-a", "dataset-a", "document-a")
    ]


@pytest.mark.asyncio
async def test_foreign_document_delete_rejects_before_fence_vector_or_storage() -> None:
    database = FencedDeleteDatabase()
    database.document_dataset_id = "dataset-b"
    vector_store = RecordingVectorStore()
    storage_delete = AsyncMock(return_value=0)
    knowledge = DocumentKnowledgeService(database, vector_store)
    knowledge.image_storage_service = SimpleNamespace(
        delete_document_assets=storage_delete,
    )
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = knowledge  # type: ignore[assignment]

    with pytest.raises(Exception, match="document not found"):
        await service.delete_document(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "document-a",
        )

    assert dataset_index_deletion_fence(database.dataset) is None
    assert vector_store.document_deletes == []
    storage_delete.assert_not_awaited()
    assert database.deleted is False


@pytest.mark.asyncio
async def test_first_missing_document_delete_rejects_without_creating_fence() -> None:
    database = FencedDeleteDatabase()
    database.document_exists = False
    vector_store = RecordingVectorStore()
    storage_delete = AsyncMock(return_value=0)
    knowledge = DocumentKnowledgeService(database, vector_store)
    knowledge.image_storage_service = SimpleNamespace(
        delete_document_assets=storage_delete,
    )
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = knowledge  # type: ignore[assignment]

    with pytest.raises(Exception, match="document not found"):
        await service.delete_document(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "document-a",
        )

    assert dataset_index_deletion_fence(database.dataset) is None
    assert database.clear_calls == 0
    assert vector_store.document_deletes == []
    storage_delete.assert_not_awaited()


@pytest.mark.asyncio
async def test_late_collection_failure_retains_marker_and_hides_content() -> None:
    database = FencedDeleteDatabase()
    vector_store = PartialDocumentSweepStore()
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]

    with pytest.raises(RuntimeError, match="second collection"):
        await service.delete_document(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "document-a",
        )

    assert vector_store.touched == ["custom-base"]
    assert database.document_exists is True
    assert dataset_index_deletion_fence(database.dataset) == (
        make_dataset_index_deletion_fence("document_delete", "document-a")
    )
    with pytest.raises(Exception, match="indexed content is unavailable"):
        _require_dataset_index_readable(database.dataset)


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", [False, RuntimeError("document db failed")])
async def test_final_document_db_failure_retains_exact_marker(failure: object) -> None:
    database = FencedDeleteDatabase()
    if isinstance(failure, Exception):
        database.delete_error = failure
        expected = "document db failed"
    else:
        database.delete_result = False
        expected = "database deletion failed"
    vector_store = RecordingVectorStore()
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]

    with pytest.raises(Exception, match=expected):
        await service.delete_document(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "document-a",
        )

    assert dataset_index_deletion_fence(database.dataset) == (
        make_dataset_index_deletion_fence("document_delete", "document-a")
    )


@pytest.mark.asyncio
async def test_same_target_retry_recovers_after_clear_failure() -> None:
    database = FencedDeleteDatabase()
    database.fail_clear_once = True
    vector_store = RecordingVectorStore()
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]
    user = UserContext(user_id="editor-a", tenant_id="tenant-a")

    with pytest.raises(RuntimeError, match="clear failed"):
        await service.delete_document(user, "dataset-a", "document-a")
    assert database.document_exists is False
    assert dataset_index_deletion_fence(database.dataset) is not None

    assert await service.delete_document(user, "dataset-a", "document-a") is True
    assert dataset_index_deletion_fence(database.dataset) is None
    assert database.clear_calls == 2


@pytest.mark.asyncio
async def test_different_document_cannot_overwrite_pending_target() -> None:
    database = FencedDeleteDatabase()
    database.dataset["index_config"] = {
        "retrieval": {
            "_index_deletion_fence": make_dataset_index_deletion_fence(
                "document_delete",
                "document-b",
            )
        }
    }
    vector_store = RecordingVectorStore()
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]

    with pytest.raises(Exception, match="indexed content is unavailable"):
        await service.delete_document(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "document-a",
        )

    assert dataset_index_deletion_fence(database.dataset) == (
        make_dataset_index_deletion_fence("document_delete", "document-b")
    )
    assert vector_store.document_deletes == []


@pytest.mark.asyncio
@pytest.mark.parametrize("method_name", ["list_documents", "get_document", "list_segments"])
async def test_pending_marker_blocks_public_content_service_before_db_read(
    method_name: str,
) -> None:
    database = FencedDeleteDatabase()
    database.dataset["index_config"] = {
        "retrieval": {
            "_index_deletion_fence": make_dataset_index_deletion_fence(
                "document_delete",
                "document-a",
            )
        }
    }
    database.list_documents = AsyncMock(return_value=[])
    database.get_document = AsyncMock(return_value=None)
    database.list_segments = AsyncMock(return_value=[])
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(  # type: ignore[assignment]
        database,
        RecordingVectorStore(),
    )
    user = UserContext(user_id="viewer-a", tenant_id="tenant-a")

    with pytest.raises(Exception, match="indexed content is unavailable"):
        if method_name == "list_documents":
            await service.list_documents(user, "dataset-a")
        elif method_name == "get_document":
            await service.get_document(user, "dataset-a", "document-a")
        else:
            await service.list_segments(user, "dataset-a")

    getattr(database, method_name).assert_not_awaited()


@pytest.mark.asyncio
async def test_content_read_discards_deletion_generation_overlap() -> None:
    database = FencedDeleteDatabase()
    database.dataset["content_revision"] = 10

    async def list_documents(**_kwargs: Any) -> list[dict[str, Any]]:
        database.dataset["content_revision"] += 2
        return [{"document_id": "stale-document"}]

    database.list_documents = list_documents  # type: ignore[method-assign]
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(  # type: ignore[assignment]
        database,
        RecordingVectorStore(),
    )

    with pytest.raises(Exception, match="generation changed during read"):
        await service.list_documents(
            UserContext(user_id="viewer-a", tenant_id="tenant-a"),
            "dataset-a",
        )


@pytest.mark.asyncio
async def test_segment_qdrant_failure_retains_row_and_exact_marker() -> None:
    database = FencedSegmentDatabase()
    vector_store = RecordingSegmentSweepStore(failure=RuntimeError("qdrant failed"))
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]

    with pytest.raises(RuntimeError, match="qdrant failed"):
        await service.delete_segment(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "segment-a",
        )

    assert database.segment_exists is True
    assert database.segment_delete_calls == 0
    assert dataset_index_deletion_fence(database.dataset) == (
        make_dataset_index_deletion_fence("segment_delete", "segment-a")
    )
    assert vector_store.segment_sweeps[0]["lifecycle_lease_held"] is True


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("status", "enabled", "archived", "metadata"),
    [
        ("queued", True, False, {}),
        ("processing", True, False, {}),
        ("completed", False, False, {}),
        ("completed", True, True, {}),
        ("completed", True, False, {DOCUMENT_UPLOAD_GENERATION_KEY: "generation-a"}),
        ("completed", True, False, {DOCUMENT_UPLOAD_FAILED_KEY: {"generation": "a"}}),
        ("completed", True, False, {CONFLUENCE_SYNC_GENERATION_KEY: "generation-a"}),
    ],
)
async def test_segment_delete_rejects_inactive_owner_before_any_fence_or_write(
    status: str,
    enabled: bool,
    archived: bool,
    metadata: dict[str, Any],
) -> None:
    database = FencedSegmentDatabase()
    database.document_status = status
    database.document_enabled = enabled
    database.document_archived = archived
    database.document_metadata = metadata
    vector_store = RecordingSegmentSweepStore()
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]

    with pytest.raises(Exception, match="active completed document"):
        await service.delete_segment(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "segment-a",
        )

    assert dataset_index_deletion_fence(database.dataset) is None
    assert database.segment_delete_calls == 0
    assert database.segment_updates == []
    assert vector_store.segment_sweeps == []
    assert vector_store.visibility_updates == []


@pytest.mark.asyncio
async def test_image_segment_delete_is_disabled_before_any_fence_or_write() -> None:
    database = FencedSegmentDatabase()
    database.segment_content_type = "image"
    vector_store = RecordingSegmentSweepStore()
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]

    with pytest.raises(Exception, match="durable image receipt"):
        await service.delete_segment(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "segment-a",
        )

    assert dataset_index_deletion_fence(database.dataset) is None
    assert database.segment_exists is True
    assert database.segment_delete_calls == 0
    assert database.segment_updates == []
    assert vector_store.segment_sweeps == []
    assert vector_store.visibility_updates == []


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", [False, RuntimeError("segment db failed")])
async def test_segment_final_db_failure_retains_marker_for_retry(
    failure: object,
) -> None:
    database = FencedSegmentDatabase()
    if isinstance(failure, Exception):
        database.segment_delete_error = failure
        expected = "segment db failed"
    else:
        database.segment_delete_result = False
        expected = "database deletion failed"
    vector_store = RecordingSegmentSweepStore()
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]

    with pytest.raises(Exception, match=expected):
        await service.delete_segment(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "segment-a",
        )

    assert dataset_index_deletion_fence(database.dataset) == (
        make_dataset_index_deletion_fence("segment_delete", "segment-a")
    )
    assert len(vector_store.segment_sweeps) == 1


@pytest.mark.asyncio
async def test_segment_same_target_retry_after_db_false_completes_saga() -> None:
    database = FencedSegmentDatabase()
    database.segment_delete_result = False
    vector_store = RecordingSegmentSweepStore()
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]
    user = UserContext(user_id="editor-a", tenant_id="tenant-a")

    with pytest.raises(Exception, match="database deletion failed"):
        await service.delete_segment(user, "dataset-a", "segment-a")
    assert database.segment_exists is True
    assert dataset_index_deletion_fence(database.dataset) == (
        make_dataset_index_deletion_fence("segment_delete", "segment-a")
    )

    database.segment_delete_result = True
    assert await service.delete_segment(user, "dataset-a", "segment-a") is True
    assert database.segment_exists is False
    assert dataset_index_deletion_fence(database.dataset) is None
    assert database.segment_delete_calls == 2
    assert len(vector_store.segment_sweeps) == 2


@pytest.mark.asyncio
async def test_segment_same_target_retry_recovers_after_clear_failure() -> None:
    database = FencedSegmentDatabase()
    database.fail_clear_once = True
    vector_store = RecordingSegmentSweepStore()
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]
    user = UserContext(user_id="editor-a", tenant_id="tenant-a")

    with pytest.raises(RuntimeError, match="clear failed"):
        await service.delete_segment(user, "dataset-a", "segment-a")
    assert database.segment_exists is False
    assert dataset_index_deletion_fence(database.dataset) is not None

    assert await service.delete_segment(user, "dataset-a", "segment-a") is True
    assert dataset_index_deletion_fence(database.dataset) is None
    assert database.clear_calls == 2


def _segment_point() -> qmodels.PointStruct:
    return qmodels.PointStruct(
        id="point-a",
        vector=[0.1, 0.2],
        payload={
            "tenant_id": "tenant-a",
            "dataset_id": "dataset-a",
            "document_id": "document-a",
            "segment_id": "segment-a",
            "text": "stale segment",
        },
    )


@pytest.mark.asyncio
async def test_shared_upsert_finishes_before_exclusive_segment_sweep(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = BarrierQdrantClient()
    client.block_upsert = True
    database = ConcurrentSegmentDatabase()
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: client,
    )
    vector_store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        dataset_write_lease=database.dataset_index_write_lease,
    )
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]

    upsert_task = asyncio.create_task(
        vector_store.upsert("custom-base", [_segment_point()])
    )
    await asyncio.wait_for(client.upsert_started.wait(), timeout=1)
    delete_task = asyncio.create_task(
        service.delete_segment(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "segment-a",
        )
    )
    await asyncio.wait_for(database.barrier.exclusive_requested.wait(), timeout=1)
    assert database.barrier.exclusive_entered.is_set() is False

    client.allow_upsert.set()
    await asyncio.wait_for(upsert_task, timeout=1)
    await asyncio.wait_for(delete_task, timeout=1)

    assert database.segment_exists is False
    assert all("point-a" not in points for points in client.points.values())


@pytest.mark.asyncio
async def test_upsert_contending_with_exclusive_segment_delete_never_rebuilds_point(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = BarrierQdrantClient()
    client.block_delete = True
    database = ConcurrentSegmentDatabase()
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: client,
    )
    vector_store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        dataset_write_lease=database.dataset_index_write_lease,
    )
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = DocumentKnowledgeService(database, vector_store)  # type: ignore[assignment]

    delete_task = asyncio.create_task(
        service.delete_segment(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "segment-a",
        )
    )
    await asyncio.wait_for(client.delete_started.wait(), timeout=1)
    assert database.barrier.exclusive_entered.is_set() is True

    with pytest.raises(RuntimeError, match="refusing a queued vector write"):
        await vector_store.upsert("custom-base", [_segment_point()])
    assert client.upsert_calls == 0

    client.allow_delete.set()
    await asyncio.wait_for(delete_task, timeout=1)

    assert database.segment_exists is False
    assert all("point-a" not in points for points in client.points.values())


def _dedupe_rows() -> list[dict[str, Any]]:
    return [
        {
            "segment_id": "segment-oldest",
            "vector_id": "point-oldest",
            "content_hash": "same-content",
            "created_at": "2026-01-01T00:00:00Z",
        },
        {
            "segment_id": "segment-a",
            "vector_id": "point-a",
            "content_hash": "same-content",
            "created_at": "2026-01-02T00:00:00Z",
        },
    ]


@pytest.mark.asyncio
async def test_dedupe_uses_segment_saga_for_all_owned_generations() -> None:
    database = FencedSegmentDatabase()
    _install_dedupe_rows(database)
    vector_store = RecordingSegmentSweepStore()
    service = DedupeKnowledgeService(database, vector_store)

    result = await dedupe_segments(
        "dataset-a",
        request=SimpleNamespace(),
        dry_run=False,
        svc=service,  # type: ignore[arg-type]
        user=UserContext(user_id="owner-a", tenant_id="tenant-a"),
        settings=SimpleNamespace(),  # type: ignore[arg-type]
    )

    assert result["deleted_count"] == 1
    assert "errors" not in result
    assert database.segment_exists is False
    assert dataset_index_deletion_fence(database.dataset) is None
    assert vector_store.segment_sweeps == [
        {
            "tenant_id": "tenant-a",
            "dataset_id": "dataset-a",
            "document_id": "document-a",
            "segment_id": "point-a",
            "lifecycle_lease_held": True,
        }
    ]


@pytest.mark.asyncio
async def test_dedupe_qdrant_race_retains_marker_and_fails_closed() -> None:
    database = FencedSegmentDatabase()
    _install_dedupe_rows(database)
    vector_store = RecordingSegmentSweepStore(
        failure=RuntimeError("secondary generation delete failed")
    )
    service = DedupeKnowledgeService(database, vector_store)

    with pytest.raises(HTTPException) as caught:
        await dedupe_segments(
            "dataset-a",
            request=SimpleNamespace(),
            dry_run=False,
            svc=service,  # type: ignore[arg-type]
            user=UserContext(user_id="owner-a", tenant_id="tenant-a"),
            settings=SimpleNamespace(),  # type: ignore[arg-type]
        )

    assert caught.value.status_code == 400
    assert "indexed content is unavailable" in str(caught.value.detail)
    assert database.segment_exists is True
    assert database.segment_delete_calls == 0
    assert dataset_index_deletion_fence(database.dataset) == (
        make_dataset_index_deletion_fence("segment_delete", "segment-a")
    )

    vector_store.failure = None
    recovered = await dedupe_segments(
        "dataset-a",
        request=SimpleNamespace(),
        dry_run=False,
        svc=service,  # type: ignore[arg-type]
        user=UserContext(user_id="owner-a", tenant_id="tenant-a"),
        settings=SimpleNamespace(),  # type: ignore[arg-type]
    )

    assert recovered["deleted_count"] == 1
    assert "errors" not in recovered
    assert database.segment_exists is False
    assert dataset_index_deletion_fence(database.dataset) is None
    assert len(vector_store.segment_sweeps) == 2


@pytest.mark.asyncio
async def test_hierarchical_delete_api_uses_persisted_ids_and_custom_base_name() -> None:
    database = DeleteDatabase()
    vector_store = RecordingVectorStore()
    indexer = HierarchicalIndexer(
        vector_store=vector_store,
        database=database,
        embedder=StubEmbedder(),
    )

    results = await indexer.delete_document_index(
        "document-a",
        "dataset-a",
        max_retries=1,
    )

    assert results == {
        "custom-base": True,
        "custom-base_sections": True,
        "custom-base_summary": True,
    }
    assert vector_store.deletes == [
        ("custom-base", ["paragraph-vector"], "tenant-a", "dataset-a"),
        ("custom-base_sections", ["section-vector"], "tenant-a", "dataset-a"),
        (
            "custom-base_summary",
            ["legacy-summary-vector", "summary-vector"],
            "tenant-a",
            "dataset-a",
        ),
    ]


class FailingIndexDatabase(DeleteDatabase):
    async def insert_segments(self, _segments: list[dict[str, Any]]) -> None:
        raise RuntimeError("segment persistence failed")

    async def save_document_summary(self, _data: dict[str, Any]) -> bool:
        raise RuntimeError("summary persistence failed")


@pytest.mark.asyncio
async def test_hierarchical_compensation_uses_supported_scoped_delete_api() -> None:
    database = FailingIndexDatabase()
    vector_store = RecordingVectorStore()
    indexer = HierarchicalIndexer(
        vector_store=vector_store,
        database=database,
        embedder=StubEmbedder(),
    )
    paragraph = HierarchicalSegment(
        segment_id="paragraph-vector",
        document_id="document-a",
        dataset_id="dataset-a",
        level=IndexLevel.PARAGRAPH,
        text="paragraph",
    )
    section = HierarchicalSegment(
        segment_id="section-vector",
        document_id="document-a",
        dataset_id="dataset-a",
        level=IndexLevel.SECTION,
        text="section",
    )
    summary = HierarchicalSegment(
        segment_id="summary-vector",
        document_id="document-a",
        dataset_id="dataset-a",
        level=IndexLevel.DOCUMENT,
        text="summary",
        summary="summary",
    )

    with pytest.raises(RuntimeError, match="segment persistence failed"):
        await indexer._index_segments([paragraph], "dataset-a", 3, "custom-base")
    with pytest.raises(RuntimeError, match="segment persistence failed"):
        await indexer._index_sections([section], "dataset-a", 3, "custom-base")
    with pytest.raises(RuntimeError, match="summary persistence failed"):
        await indexer._index_summary(summary, "dataset-a", 3, "custom-base")

    assert vector_store.deletes == [
        ("custom-base", ["paragraph-vector"], "tenant-a", "dataset-a"),
        ("custom-base_sections", ["section-vector"], "tenant-a", "dataset-a"),
        ("custom-base_summary", ["summary-vector"], "tenant-a", "dataset-a"),
    ]


@pytest.mark.asyncio
async def test_document_filter_sweep_covers_every_owned_collection(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    deleted: list[tuple[str, qmodels.Filter]] = []
    scopes = {
        "custom-base": ("tenant-a", "dataset-a"),
        "custom-base_sections": ("tenant-a", "dataset-a"),
        "kb_dataset-a_1024": ("tenant-a", "dataset-a"),
        "old-generation": ("tenant-a", "dataset-a"),
        "foreign": ("tenant-b", "dataset-b"),
    }

    class Client:
        async def get_collections(self) -> SimpleNamespace:
            return SimpleNamespace(
                collections=[SimpleNamespace(name=name) for name in scopes]
            )

        async def get_collection(self, collection_name: str) -> SimpleNamespace:
            tenant_id, dataset_id = scopes[collection_name]
            return SimpleNamespace(
                config=SimpleNamespace(
                    metadata={
                        "knowledge_scope": {
                            "schema_version": 1,
                            "tenant_id": tenant_id,
                            "dataset_id": dataset_id,
                        }
                    }
                )
            )

        async def delete(self, **kwargs: Any) -> SimpleNamespace:
            deleted.append(
                (
                    kwargs["collection_name"],
                    kwargs["points_selector"].filter,
                )
            )
            return SimpleNamespace(status="completed")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        dataset_write_lease=_allow_dataset_lifecycle,
    )

    owned = await store.delete_document_points(
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        document_id="document-a",
    )

    assert owned == [
        "custom-base",
        "custom-base_sections",
        "kb_dataset-a_1024",
        "old-generation",
    ]
    assert [name for name, _filter in deleted] == owned
    for _name, deletion_filter in deleted:
        assert _matches_qdrant_filter(
            {
                "tenant_id": "tenant-a",
                "dataset_id": "dataset-a",
                "document_id": "document-a",
            },
            deletion_filter,
        )
        assert _matches_qdrant_filter(
            {"dataset_id": "dataset-a", "document_id": "document-a"},
            deletion_filter,
        )
        assert not _matches_qdrant_filter(
            {
                "tenant_id": "tenant-b",
                "dataset_id": "dataset-a",
                "document_id": "document-a",
            },
            deletion_filter,
        )


@pytest.mark.asyncio
async def test_segment_sweep_waits_and_rejects_late_collection_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    delete_calls: list[dict[str, Any]] = []

    class Client:
        async def get_collections(self) -> SimpleNamespace:
            return SimpleNamespace(
                collections=[
                    SimpleNamespace(name="collection-a"),
                    SimpleNamespace(name="collection-b"),
                ]
            )

        async def get_collection(self, _collection_name: str) -> SimpleNamespace:
            return SimpleNamespace(
                config=SimpleNamespace(
                    metadata={
                        "knowledge_scope": {
                            "schema_version": 1,
                            "tenant_id": "tenant-a",
                            "dataset_id": "dataset-a",
                        }
                    }
                )
            )

        async def delete(self, **kwargs: Any) -> SimpleNamespace:
            delete_calls.append(dict(kwargs))
            status = (
                "completed"
                if kwargs["collection_name"] == "collection-a"
                else "failed"
            )
            return SimpleNamespace(status=status)

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)

    with pytest.raises(VectorStoreError, match="segment delete from collection-b"):
        await store.delete_segment_points(
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            document_id="document-a",
            segment_id="point-a",
            lifecycle_lease_held=True,
        )

    assert [call["collection_name"] for call in delete_calls] == [
        "collection-a",
        "collection-b",
    ]
    assert all(call["wait"] is True for call in delete_calls)
    for call in delete_calls:
        deletion_filter = call["points_selector"].filter
        fields = {
            condition.key
            for condition in deletion_filter.must or []
            if isinstance(condition, qmodels.FieldCondition)
        }
        assert {"dataset_id", "document_id"} <= fields
        assert "point-a" in _positive_filter_point_ids(deletion_filter)


@pytest.mark.asyncio
async def test_vector_upsert_holds_document_write_lease(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    events: list[str] = []

    @contextlib.asynccontextmanager
    async def write_lease(
        dataset_id: str,
        document_ids: list[str],
        *,
        expected_ingestion_identity: str,
    ):
        assert dataset_id == "dataset-a"
        assert document_ids == ["document-a"]
        assert expected_ingestion_identity == "captured-generation"
        events.append("lease-enter")
        yield
        events.append("lease-exit")

    class Client:
        async def get_collection(self, _collection_name: str) -> SimpleNamespace:
            events.append("collection-read")
            return SimpleNamespace(
                config=SimpleNamespace(
                    params=SimpleNamespace(
                        sparse_vectors={},
                        vectors=SimpleNamespace(size=2),
                    ),
                    metadata={
                        "knowledge_scope": {
                            "schema_version": 1,
                            "tenant_id": "tenant-a",
                            "dataset_id": "dataset-a",
                        }
                    },
                )
            )

        async def upsert(self, **_kwargs: Any) -> SimpleNamespace:
            events.append("qdrant-upsert")
            return SimpleNamespace(status="completed")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        dataset_write_lease=write_lease,
    )
    await store.upsert(
        "custom-base",
        [
            qmodels.PointStruct(
                id="point-a",
                vector=[0.1, 0.2],
                payload={
                    "tenant_id": "tenant-a",
                    "dataset_id": "dataset-a",
                    "document_id": "document-a",
                    "text": "alpha",
                },
            )
        ],
        expected_ingestion_identity="captured-generation",
    )

    assert events == [
        "collection-read",
        "lease-enter",
        "collection-read",
        "qdrant-upsert",
        "lease-exit",
    ]


@pytest.mark.asyncio
async def test_existing_dataset_collection_setup_waits_for_lifecycle_lease(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = StatefulQdrantClient()
    lease_requested = asyncio.Event()
    allow_shared_lease = asyncio.Event()
    events: list[str] = []

    @contextlib.asynccontextmanager
    async def write_lease(dataset_id: str, document_ids: list[str]):
        assert dataset_id == "dataset-a"
        assert document_ids == []
        events.append("lease-request")
        lease_requested.set()
        await allow_shared_lease.wait()
        events.append("lease-enter")
        try:
            yield
        finally:
            events.append("lease-exit")

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: client,
    )
    store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        dataset_write_lease=write_lease,
    )

    ensure_task = asyncio.create_task(
        store.ensure_collection(
            dataset_id="dataset-a",
            dimension=2,
            collection_name="collection-a",
            tenant_id="tenant-a",
        )
    )
    await asyncio.wait_for(lease_requested.wait(), timeout=1)
    assert client.collections == {}

    allow_shared_lease.set()
    assert await ensure_task == "collection-a"
    assert "collection-a" in client.collections
    assert events == ["lease-request", "lease-enter", "lease-exit"]


@pytest.mark.asyncio
async def test_unbound_dataset_bootstrap_is_the_only_collection_lease_bypass(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = StatefulQdrantClient()

    @contextlib.asynccontextmanager
    async def unexpected_lease(*_args: Any, **_kwargs: Any):
        raise AssertionError("unbound dataset bootstrap cannot acquire an existing-row lease")
        yield

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: client,
    )
    store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        dataset_write_lease=unexpected_lease,
    )

    created = await store.ensure_collection(
        dataset_id="dataset-new",
        dimension=2,
        collection_name="collection-new",
        tenant_id="tenant-a",
        allow_existing=False,
        bootstrap_unbound_dataset=True,
    )

    assert created == "collection-new"
    assert client.collections["collection-new"]["info"].config.metadata[
        "knowledge_scope"
    ] == {
        "schema_version": 1,
        "dataset_id": "dataset-new",
        "tenant_id": "tenant-a",
    }


@pytest.mark.asyncio
async def test_point_delete_waits_for_owning_dataset_lifecycle_lease(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class Client:
        def __init__(self) -> None:
            self.mutated = False

        async def get_collection(self, _collection_name: str) -> SimpleNamespace:
            return SimpleNamespace(
                config=SimpleNamespace(
                    params=SimpleNamespace(sparse_vectors={}),
                    metadata={
                        "knowledge_scope": {
                            "schema_version": 1,
                            "dataset_id": "dataset-a",
                            "tenant_id": "tenant-a",
                        }
                    },
                )
            )

        async def delete(self, **_kwargs: Any) -> SimpleNamespace:
            self.mutated = True
            return SimpleNamespace(status="completed")

        async def close(self) -> None:
            return None

    client = Client()
    lease_requested = asyncio.Event()
    allow_shared_lease = asyncio.Event()

    @contextlib.asynccontextmanager
    async def write_lease(dataset_id: str, document_ids: list[str]):
        assert dataset_id == "dataset-a"
        assert document_ids == []
        lease_requested.set()
        await allow_shared_lease.wait()
        yield

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: client,
    )
    store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        dataset_write_lease=write_lease,
    )

    delete_task = asyncio.create_task(
        store.delete_points(
            "collection-a",
            ["point-a"],
            tenant_id="tenant-a",
            dataset_id="dataset-a",
        )
    )
    await asyncio.wait_for(lease_requested.wait(), timeout=1)
    assert client.mutated is False

    allow_shared_lease.set()
    await delete_task
    assert client.mutated is True


@pytest.mark.asyncio
async def test_v1_scope_and_document_sweep_cover_hierarchy_image_and_legacy(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = StatefulQdrantClient()
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: client,
    )
    store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        dataset_write_lease=_allow_dataset_lifecycle,
    )

    target_collections = {
        "custom-base": 2,
        "custom-base_sections": 2,
        "custom-base_summary": 2,
        "kb_dataset-a_3": 3,
        "old-generation": 2,
    }
    for name, dimension in target_collections.items():
        await store.ensure_collection(
            dataset_id="dataset-a",
            dimension=dimension,
            collection_name=name,
            tenant_id="tenant-a",
        )
        metadata = client.collections[name]["info"].config.metadata
        assert metadata["knowledge_scope"] == {
            "schema_version": 1,
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
        }
        await store.upsert(
            name,
            [
                qmodels.PointStruct(
                    id=f"target-{name}",
                    vector=[0.1] * dimension,
                    payload={"document_id": "document-a", "text": "alpha"},
                )
            ],
        )

    normalized = client.collections["custom-base"]["points"]["target-custom-base"]
    assert normalized.payload["dataset_id"] == "dataset-a"
    assert normalized.payload["tenant_id"] == "tenant-a"

    await store.ensure_collection(
        dataset_id="dataset-b",
        dimension=2,
        collection_name="foreign",
        tenant_id="tenant-b",
    )
    await store.upsert(
        "foreign",
        [
            qmodels.PointStruct(
                id="foreign-point",
                vector=[0.2, 0.3],
                payload={"document_id": "document-a", "text": "foreign"},
            )
        ],
    )

    client.seed_collection(
        "legacy-images",
        points=[
            qmodels.PointStruct(
                id="legacy-target",
                vector=[0.1, 0.2],
                payload={
                    "dataset_id": "dataset-a",
                    "document_id": "document-a",
                    "content_type": "image",
                },
            ),
            qmodels.PointStruct(
                id="legacy-foreign-dataset",
                vector=[0.2, 0.3],
                payload={
                    "dataset_id": "dataset-b",
                    "document_id": "document-a",
                },
            ),
            qmodels.PointStruct(
                id="legacy-foreign-tenant",
                vector=[0.3, 0.4],
                payload={
                    "tenant_id": "tenant-b",
                    "dataset_id": "dataset-a",
                    "document_id": "document-a",
                },
            ),
        ],
    )

    assert await store.document_has_points(
        tenant_id="tenant-a", dataset_id="dataset-a", document_id="document-a",
    )
    assert await store.document_has_specialized_points(
        tenant_id="tenant-a", dataset_id="dataset-a", document_id="document-a",
    )
    assert not await store.document_has_points(
        tenant_id="tenant-a", dataset_id="dataset-a", document_id="missing-document",
    )

    swept = await store.delete_document_points(
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        document_id="document-a",
    )

    assert set(swept) == {*target_collections, "legacy-images"}
    for name in target_collections:
        assert client.collections[name]["points"] == {}
    assert set(client.collections["legacy-images"]["points"]) == {
        "legacy-foreign-dataset",
        "legacy-foreign-tenant",
    }
    assert set(client.collections["foreign"]["points"]) == {"foreign-point"}
    assert not await store.document_has_points(
        tenant_id="tenant-a", dataset_id="dataset-a", document_id="document-a",
    )
    assert not await store.document_has_specialized_points(
        tenant_id="tenant-a", dataset_id="dataset-a", document_id="document-a",
    )


@pytest.mark.asyncio
async def test_existing_unscoped_collection_adoption_requires_exact_payload_scope(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = StatefulQdrantClient()
    client.seed_collection(
        "legacy-exact",
        points=[
            qmodels.PointStruct(
                id="owned",
                vector=[0.1, 0.2],
                payload={"tenant_id": "tenant-a", "dataset_id": "dataset-a"},
            )
        ],
    )
    client.seed_collection(
        "legacy-mixed",
        points=[
            qmodels.PointStruct(
                id="owned",
                vector=[0.1, 0.2],
                payload={"tenant_id": "tenant-a", "dataset_id": "dataset-a"},
            ),
            qmodels.PointStruct(
                id="foreign",
                vector=[0.2, 0.3],
                payload={"tenant_id": "tenant-b", "dataset_id": "dataset-b"},
            ),
        ],
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: client,
    )
    store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        dataset_write_lease=_allow_dataset_lifecycle,
    )

    assert await store.ensure_collection(
        dataset_id="dataset-a",
        dimension=2,
        collection_name="legacy-exact",
        tenant_id="tenant-a",
    ) == "legacy-exact"
    assert client.collections["legacy-exact"]["info"].config.metadata[
        "knowledge_scope"
    ] == {
        "schema_version": 1,
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
    }

    with pytest.raises(VectorStoreError, match="cannot be safely adopted"):
        await store.ensure_collection(
            dataset_id="dataset-a",
            dimension=2,
            collection_name="legacy-mixed",
            tenant_id="tenant-a",
        )
    assert client.collections["legacy-mixed"]["info"].config.metadata == {}


@pytest.mark.asyncio
async def test_legacy_adoption_marker_fails_closed_on_racing_foreign_write(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class RacingClient(StatefulQdrantClient):
        async def update_collection(self, **kwargs: Any) -> bool:
            result = await super().update_collection(**kwargs)
            scope = (kwargs.get("metadata") or {}).get("knowledge_scope") or {}
            if scope.get("status") == "adopting":
                self.collections[kwargs["collection_name"]]["points"]["racing-foreign"] = (
                    qmodels.PointStruct(
                        id="racing-foreign",
                        vector=[0.3, 0.4],
                        payload={
                            "tenant_id": "tenant-b",
                            "dataset_id": "dataset-b",
                        },
                    )
                )
            return result

    client = RacingClient()
    client.seed_collection(
        "legacy-race",
        points=[
            qmodels.PointStruct(
                id="owned",
                vector=[0.1, 0.2],
                payload={"tenant_id": "tenant-a", "dataset_id": "dataset-a"},
            )
        ],
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: client,
    )
    store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        dataset_write_lease=_allow_dataset_lifecycle,
    )

    with pytest.raises(VectorStoreError, match="changed during ownership adoption"):
        await store.ensure_collection(
            dataset_id="dataset-a",
            dimension=2,
            collection_name="legacy-race",
            tenant_id="tenant-a",
        )
    scope = client.collections["legacy-race"]["info"].config.metadata[
        "knowledge_scope"
    ]
    assert scope == {"schema_version": 0, "adoption_failed": True}


@pytest.mark.asyncio
async def test_dataset_delete_covers_empty_authoritative_and_scoped_generations(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = StatefulQdrantClient()
    client.seed_collection("legacy-empty-base")
    client.seed_collection(
        "legacy-mixed",
        points=[
            qmodels.PointStruct(
                id="owned-missing-tenant",
                vector=[0.1, 0.2],
                payload={"dataset_id": "dataset-a", "document_id": "document-a"},
            ),
            qmodels.PointStruct(
                id="foreign",
                vector=[0.2, 0.3],
                payload={"tenant_id": "tenant-b", "dataset_id": "dataset-b"},
            ),
        ],
    )
    client.seed_collection(
        "legacy-foreign",
        points=[
            qmodels.PointStruct(
                id="foreign-only",
                vector=[0.2, 0.3],
                payload={"tenant_id": "tenant-b", "dataset_id": "dataset-b"},
            )
        ],
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: client,
    )
    store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        dataset_write_lease=_allow_dataset_lifecycle,
    )

    for name in ("legacy-empty-base_sections", "old-image-generation"):
        await store.ensure_collection(
            dataset_id="dataset-a",
            dimension=2,
            collection_name=name,
            tenant_id="tenant-a",
        )
    await store.ensure_collection(
        dataset_id="dataset-b",
        dimension=2,
        collection_name="foreign-scoped",
        tenant_id="tenant-b",
    )

    touched = await store.delete_dataset_collections(
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        authoritative_collection_names=["legacy-empty-base"],
        lifecycle_lease_held=True,
    )

    assert set(touched) == {
        "legacy-empty-base",
        "legacy-empty-base_sections",
        "old-image-generation",
        "legacy-mixed",
    }
    assert "legacy-empty-base" not in client.collections
    assert "legacy-empty-base_sections" not in client.collections
    assert "old-image-generation" not in client.collections
    assert set(client.collections["legacy-mixed"]["points"]) == {"foreign"}
    assert "legacy-foreign" in client.collections
    assert "foreign-scoped" in client.collections


@pytest.mark.asyncio
async def test_destructive_vector_sweeps_fail_closed_without_lifecycle_admission(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = StatefulQdrantClient()
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: client,
    )
    store = VectorStore(url="http://qdrant", max_retries=1)

    with pytest.raises(VectorStoreError, match="document deletion requires"):
        await store.delete_document_points(
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            document_id="document-a",
        )
    with pytest.raises(VectorStoreError, match="exclusive lifecycle lease"):
        await store.delete_dataset_collections(
            tenant_id="tenant-a",
            dataset_id="dataset-a",
        )


@pytest.mark.asyncio
async def test_dataset_delete_rejects_authoritative_collection_scope_mismatch(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = StatefulQdrantClient()
    client.seed_collection(
        "db-authoritative",
        metadata={
            "knowledge_scope": {
                "schema_version": 1,
                "tenant_id": "tenant-b",
                "dataset_id": "dataset-b",
            }
        },
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: client,
    )
    store = VectorStore(url="http://qdrant", max_retries=1)

    with pytest.raises(VectorStoreError, match="immutable scope mismatch"):
        await store.delete_dataset_collections(
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            authoritative_collection_names=["db-authoritative"],
            lifecycle_lease_held=True,
        )
    assert "db-authoritative" in client.collections
