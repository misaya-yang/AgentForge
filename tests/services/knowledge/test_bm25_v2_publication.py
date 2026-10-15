from __future__ import annotations

import contextlib
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.services.knowledge.ingestion_service import IngestionService
from knowledge_service.services.knowledge.knowledge_service import KnowledgeService
from knowledge_service.services.knowledge.lexical_config import LexicalConfig


class Connection:
    @contextlib.asynccontextmanager
    async def transaction(self):
        yield


class Database:
    def __init__(self, events: list[str]) -> None:
        self.events = events
        self.connection = Connection()

    @contextlib.asynccontextmanager
    async def dataset_index_publication_lease(self, *_args: Any, **_kwargs: Any):
        self.events.append("lease:begin")
        yield SimpleNamespace(
            connection=self.connection,
            revision=-1007,
            recovered=False,
        )
        self.events.append("lease:end")

    async def finish_index_publication(self, *_args: Any, **_kwargs: Any) -> int:
        self.events.append("pg:finish")
        return 1008

    async def abort_index_publication(self, *_args: Any, **_kwargs: Any) -> None:
        self.events.append("pg:abort")

    async def update_document_status(
        self,
        _document_id: str,
        *,
        status: str,
        progress: int,
        connection: Any,
    ) -> None:
        assert status == "completed"
        assert progress == 100
        assert connection is self.connection
        self.events.append("pg:document-completed")


class VectorStore:
    def __init__(self, events: list[str]) -> None:
        self.events = events

    async def delete_points(self, _collection: str, point_ids: list[str], **_kwargs: Any):
        self.events.append(f"qdrant:delete:{','.join(point_ids)}")


class Lifecycle:
    def __init__(self, events: list[str], *, fail_recertify: bool = False) -> None:
        self.events = events
        self.fail_recertify = fail_recertify

    async def active_publication_context(self, _dataset_id: str):
        self.events.append("active:preflight")
        return {
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
            "collection_name": "collection-a",
            "epoch": 3,
            "profile": object(),
        }

    async def recertify_active_publication(
        self, _context, *, publication_revision: int, connection: Any,
    ):
        self.events.append("active:full-scroll")
        assert publication_revision == -1007
        assert isinstance(connection, Connection)
        if self.fail_recertify:
            raise RuntimeError("injected cross-authority mismatch")
        return {
            "expected_epoch": 3,
            "target_revision": 1008,
            "manifest_sha256": "manifest-a",
            "post_evidence": {"verified": True},
        }

    async def settle_active_publication(self, _context, _certification, *, connection):
        assert isinstance(connection, Connection)
        self.events.append("active:cas")
        return 4


def _service(events: list[str], *, fail_recertify: bool = False) -> IngestionService:
    service = IngestionService.__new__(IngestionService)
    service.db = Database(events)
    service.vector_store = VectorStore(events)
    service._ks = SimpleNamespace(
        bm25_v2_lifecycle_service=Lifecycle(
            events,
            fail_recertify=fail_recertify,
        )
    )

    async def prepare(**_kwargs: Any):
        events.append("backup:ready")
        if _kwargs["rollback_point_ids"]:
            return {"point-a": object()}, ["backup-a"]
        return {}, []

    async def upsert(**_kwargs: Any):
        events.append("qdrant:dual-upsert")

    async def restore(**_kwargs: Any):
        events.append("qdrant:restore-old")

    service._prepare_durable_point_backups = prepare  # type: ignore[method-assign]
    service._upsert_with_ingestion_identity = upsert  # type: ignore[method-assign]
    service._restore_point_snapshot = restore  # type: ignore[method-assign]
    return service


@pytest.mark.asyncio
async def test_active_publication_orders_authority_receipt_revision_cas() -> None:
    events: list[str] = []
    service = _service(events)

    async def commit(_connection: Any, *, finish_publication: bool = True):
        assert finish_publication is False
        events.append("pg:authority")
        return "committed"

    result = await service._publish_points_atomically(
        collection="collection-a",
        points=[SimpleNamespace(id="point-a")],
        delete_point_ids=[],
        rollback_point_ids=["point-a"],
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        expected_ingestion_identity="identity-a",
        commit=commit,
    )
    assert result == "committed"
    assert events == [
        "active:preflight",
        "lease:begin",
        "active:preflight",
        "backup:ready",
        "qdrant:dual-upsert",
        "pg:authority",
        "active:full-scroll",
        "pg:finish",
        "active:cas",
        "qdrant:delete:backup-a",
        "lease:end",
    ]


@pytest.mark.asyncio
async def test_active_publication_failure_restores_backup_and_keeps_negative_revision() -> None:
    events: list[str] = []
    service = _service(events, fail_recertify=True)

    async def commit(_connection: Any, *, finish_publication: bool = True):
        assert finish_publication is False
        events.append("pg:authority")
        return "committed"

    with pytest.raises(RuntimeError, match="rollback was incomplete; retrieval remains fenced"):
        await service._publish_points_atomically(
            collection="collection-a",
            points=[SimpleNamespace(id="point-a")],
            delete_point_ids=[],
            rollback_point_ids=["point-a"],
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            expected_ingestion_identity="identity-a",
            commit=commit,
        )
    assert "qdrant:restore-old" in events
    assert "pg:abort" not in events
    assert "pg:finish" not in events
    assert "qdrant:delete:backup-a" not in events


@pytest.mark.asyncio
async def test_active_segment_mutation_uses_the_same_publication_protocol() -> None:
    events: list[str] = []
    lifecycle = Lifecycle(events)
    service = KnowledgeService.__new__(KnowledgeService)
    service.db = Database(events)
    service.vector_store = SimpleNamespace(bm25_v2_enabled=True)
    service._bm25_v2_lifecycle_service = lifecycle
    service._bm25_v2_lifecycle_store = object()
    dataset = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "index_config": {
            "retrieval": {
                "lexical": {
                    "active_version": "bm25_v2",
                    "bm25_v2": {"shadow_write_enabled": True},
                }
            }
        },
    }

    async def require_access(_user, _dataset_id: str, *, required: str):
        assert required == "editor"
        return dataset

    service.require_dataset_access = require_access  # type: ignore[method-assign]

    async def operation():
        events.append("segment:pg-and-qdrant")
        return {"segment_id": "segment-a"}

    result = await service._run_segment_mutation_with_bm25_publication(
        SimpleNamespace(),
        "dataset-a",
        operation,
    )
    assert result == {"segment_id": "segment-a"}
    assert events == [
        "active:preflight",
        "lease:begin",
        "active:preflight",
        "segment:pg-and-qdrant",
        "active:full-scroll",
        "pg:finish",
        "active:cas",
        "lease:end",
    ]


@pytest.mark.asyncio
async def test_active_segment_kill_switch_refuses_before_publication_or_operation() -> None:
    events: list[str] = []
    service = KnowledgeService.__new__(KnowledgeService)
    service.db = Database(events)
    service.vector_store = SimpleNamespace(bm25_v2_enabled=False)
    dataset = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "index_config": {
            "retrieval": {
                "lexical": {
                    "active_version": "bm25_v2",
                    "bm25_v2": {"shadow_write_enabled": True},
                }
            }
        },
    }

    async def require_access(_user, _dataset_id: str, *, required: str):
        assert required == "editor"
        return dataset

    service.require_dataset_access = require_access  # type: ignore[method-assign]

    async def operation():
        events.append("segment:mutated")

    with pytest.raises(Exception) as error:
        await service._run_segment_mutation_with_bm25_publication(
            SimpleNamespace(),
            "dataset-a",
            operation,
        )
    assert getattr(error.value, "http_status", None) == 503
    assert events == []


@pytest.mark.asyncio
async def test_active_document_completion_is_certified_before_positive_revision() -> None:
    events: list[str] = []
    service = _service(events)
    active = LexicalConfig.from_index_config(
        {
            "retrieval": {
                "lexical": {
                    "active_version": "bm25_v2",
                    "bm25_v2": {"shadow_write_enabled": True},
                }
            }
        }
    )
    await service._complete_document_generation(
        collection="collection-a",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        document_id="document-a",
        expected_ingestion_identity="identity-a",
        lexical_config=active,
    )
    assert events == [
        "active:preflight",
        "lease:begin",
        "active:preflight",
        "backup:ready",
        "pg:document-completed",
        "active:full-scroll",
        "pg:finish",
        "active:cas",
        "lease:end",
    ]
