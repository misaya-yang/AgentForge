from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from copy import deepcopy
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import pytest
from knowledge_service.services.knowledge.processing_mode import ProcessingMode
from knowledge_service.services.knowledge.worker import (
    KnowledgeIngestTask,
    KnowledgeWorker,
)


class GenerationVectorStore:
    def __init__(self, *, fail_deletes: int = 0) -> None:
        self.fail_deletes = fail_deletes
        self.points: set[str] = {"old-point"}
        self.calls: list[dict[str, Any]] = []

    async def delete_document_points(self, **kwargs: Any) -> list[str]:
        self.calls.append(dict(kwargs))
        if self.fail_deletes:
            self.fail_deletes -= 1
            raise RuntimeError("second owned collection unavailable")
        self.points.clear()
        return ["base", "sections"]


class GenerationDatabase:
    def __init__(
        self,
        *,
        metadata: dict[str, Any] | None = None,
        source_type: str = "upload",
        status: str = "parsing",
        image_segments: list[dict[str, Any]] | None = None,
    ) -> None:
        self.dataset = {
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
            "index_config": {},
            "collection_name": "base",
        }
        self.document = {
            "document_id": "document-a",
            "dataset_id": "dataset-a",
            "source_type": source_type,
            "status": status,
            "enabled": True,
            "archived": False,
            "size_bytes": 1,
            "title": "document.pdf",
            "mime_type": "application/pdf",
            "metadata": deepcopy(metadata or {"processing_mode": "text_only"}),
            "segment_count": 1,
        }
        self.image_segments = deepcopy(image_segments or [])
        self.segments: set[str] = {"old-segment"}
        self.summary_exists = True
        self.events: list[str] = []
        self.mode_publication_result = True

    async def get_dataset(
        self,
        dataset_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        del connection
        assert dataset_id == "dataset-a"
        return deepcopy(self.dataset)

    async def get_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        del connection
        assert document_id == "document-a"
        return deepcopy(self.document)

    async def get_image_segments_by_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> list[dict[str, Any]]:
        del connection
        assert document_id == "document-a"
        return deepcopy(self.image_segments)

    async def delete_segments_by_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> int:
        assert document_id == "document-a"
        assert connection is not None
        self.events.append("db-segments-delete")
        count = len(self.segments)
        self.segments.clear()
        self.image_segments.clear()
        return count

    async def delete_document_summary(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        assert document_id == "document-a"
        assert connection is not None
        self.events.append("db-summary-delete")
        existed = self.summary_exists
        self.summary_exists = False
        return existed

    async def clear_document_legacy_image_receipts(
        self,
        document_id: str,
        dataset_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        assert (document_id, dataset_id) == ("document-a", "dataset-a")
        assert connection is not None
        self.events.append("db-legacy-image-receipt-clear")
        self.document["metadata"].pop("images_embedded", None)
        self.document["metadata"].pop("embedded_image_count", None)
        return True

    async def update_document_fields(
        self,
        document_id: str,
        fields: dict[str, Any],
        *,
        connection: Any | None = None,
        **_kwargs: Any,
    ) -> None:
        assert document_id == "document-a"
        assert connection is not None
        self.events.append("db-document-reset")
        if "metadata" in fields:
            current_marker = self.document["metadata"].get(
                "_document_lifecycle_reindex"
            )
            self.document["metadata"] = deepcopy(fields["metadata"])
            if current_marker is not None:
                self.document["metadata"]["_document_lifecycle_reindex"] = deepcopy(
                    current_marker
                )
        for key, value in fields.items():
            if key != "metadata":
                self.document[key] = deepcopy(value)

    async def update_document_status(
        self,
        document_id: str,
        status: str,
        progress: float | None = None,
        error: str | None = None,
        *,
        connection: Any | None = None,
    ) -> None:
        del progress, error, connection
        assert document_id == "document-a"
        self.document["status"] = status
        self.events.append(f"status:{status}")

    async def compare_and_swap_document_processing_mode(
        self,
        document_id: str,
        dataset_id: str,
        *,
        expected_mode: str,
        replacement_mode: str,
        detection_result: dict[str, Any],
        connection: Any | None = None,
    ) -> bool:
        del connection
        assert (document_id, dataset_id) == ("document-a", "dataset-a")
        self.events.append("mode-cas")
        if not self.mode_publication_result:
            return False
        if self.document["metadata"].get("processing_mode") != expected_mode:
            return False
        self.document["metadata"]["processing_mode"] = replacement_mode
        self.document["detection_result"] = deepcopy(detection_result)
        return True


class GenerationService:
    def __init__(
        self,
        database: GenerationDatabase,
        vector_store: GenerationVectorStore,
    ) -> None:
        self.db = database
        self.vector_store = vector_store
        self.settings = SimpleNamespace(
            knowledge=SimpleNamespace(
                large_file_threshold=100,
                pdf_split_enabled=True,
                pdf_split_max_size_bytes=100,
                pdf_split_min_pages_per_part=1,
                ocr_strategy="hybrid",
            )
        )
        self.image_storage_service = SimpleNamespace(
            download_original_file=AsyncMock(return_value=b"pdf")
        )
        self.ingest_calls: list[tuple[str, str]] = []
        self._worker: KnowledgeWorker | None = None

    async def ingest_document(
        self,
        dataset_id: str,
        document_id: str,
        *,
        chunking_config_override: dict[str, Any] | None = None,
        index_config_override: dict[str, Any] | None = None,
    ) -> list[str]:
        del chunking_config_override, index_config_override
        self.ingest_calls.append((dataset_id, document_id))
        self.db.document["status"] = "completed"
        return []


def make_worker(
    *,
    database: GenerationDatabase | None = None,
    vector_store: GenerationVectorStore | None = None,
    detector: Any | None = None,
    hierarchical: bool = False,
) -> tuple[KnowledgeWorker, GenerationDatabase, GenerationVectorStore, GenerationService]:
    database = database or GenerationDatabase()
    vector_store = vector_store or GenerationVectorStore()
    service = GenerationService(database, vector_store)
    worker = KnowledgeWorker(
        service,  # type: ignore[arg-type]
        detector=detector,
        hierarchical_indexer=object() if hierarchical else None,  # type: ignore[arg-type]
    )
    return worker, database, vector_store, service


@pytest.mark.asyncio
async def test_generation_preflight_preserves_prior_generation_for_incremental_upsert() -> None:
    worker, database, vector_store, _ = make_worker()
    task = KnowledgeIngestTask("dataset-a", "document-a")
    lease_connection = SimpleNamespace(name="owner")

    await worker._prepare_document_generation(
        task,
        connection=lease_connection,
    )

    # T1: preflight no longer sweeps. The prior serving generation must be
    # left intact for the incremental in-place upsert, and no Qdrant deletion
    # may happen at dispatch.
    assert database.segments == {"old-segment"}
    assert database.summary_exists is True
    assert database.document["segment_count"] == 1
    assert vector_store.calls == []
    assert database.events == []


@pytest.mark.asyncio
async def test_reindex_preflights_no_longer_sweep_prior_generations() -> None:
    worker, database, vector_store, _ = make_worker()
    task = KnowledgeIngestTask("dataset-a", "document-a")
    lease_connection = SimpleNamespace(name="owner")

    await worker._prepare_document_generation(task, connection=lease_connection)
    database.segments.add("generation-one-segment")
    database.summary_exists = True
    vector_store.points.add("generation-one-point")
    database.document["segment_count"] = 1
    database.document["status"] = "parsing"

    await worker._prepare_document_generation(task, connection=lease_connection)
    database.segments.add("generation-two-segment")
    vector_store.points.add("generation-two-point")

    # Both prior generations survive preflight; the incremental engine
    # reconciles rows/points during ingest instead of sweeping at dispatch.
    assert database.segments == {
        "old-segment",
        "generation-one-segment",
        "generation-two-segment",
    }
    assert vector_store.points == {
        "old-point",
        "generation-one-point",
        "generation-two-point",
    }
    assert database.summary_exists is True
    assert vector_store.calls == []


@pytest.mark.asyncio
async def test_preflight_accepts_pending_restore_generation_on_inactive_document() -> None:
    """PRD T1 item 6: a mid-restore document is still disabled/archived; the
    pending lifecycle marker is the authority that admits its reembed
    generation to preflight, and no sweep may run."""

    database = GenerationDatabase(
        metadata={
            "processing_mode": "text_only",
            "_document_ingest_action": "reembed",
            "_document_lifecycle_reindex": {
                "status": "pending",
                "desired_enabled": True,
                "desired_archived": False,
            },
        }
    )
    database.document["enabled"] = False
    database.document["archived"] = True
    worker, _, vector_store, _ = make_worker(database=database)

    await worker._prepare_document_generation(
        KnowledgeIngestTask("dataset-a", "document-a"),
        connection=SimpleNamespace(name="owner"),
    )

    assert database.segments == {"old-segment"}
    assert vector_store.calls == []
    assert database.events == []


@pytest.mark.asyncio
async def test_preflight_rejects_inactive_document_without_pending_restore() -> None:
    database = GenerationDatabase()
    database.document["enabled"] = False
    worker, _, vector_store, _ = make_worker(database=database)

    with pytest.raises(RuntimeError, match="inactive before generation cleanup"):
        await worker._prepare_document_generation(
            KnowledgeIngestTask("dataset-a", "document-a"),
            connection=SimpleNamespace(name="owner"),
        )

    assert vector_store.calls == []
    assert database.segments == {"old-segment"}


@pytest.mark.asyncio
async def test_legacy_image_receipt_without_source_fails_before_qdrant() -> None:
    database = GenerationDatabase(
        metadata={
            "processing_mode": "multimodal",
            "images_embedded": True,
            "embedded_image_count": 1,
        },
        image_segments=[{"segment_id": "legacy-image"}],
    )
    worker, _, vector_store, _ = make_worker(database=database)

    with pytest.raises(RuntimeError, match="no durable rebuild source"):
        await worker._prepare_document_generation(
            KnowledgeIngestTask("dataset-a", "document-a"),
            connection=SimpleNamespace(name="owner"),
        )

    assert vector_store.calls == []
    assert database.segments == {"old-segment"}


@pytest.mark.asyncio
async def test_complete_empty_confluence_source_preflight_keeps_old_image_generation() -> None:
    database = GenerationDatabase(
        source_type="confluence",
        metadata={
            "processing_mode": "multimodal",
            "extracted_images": [],
            "image_count": 0,
            "_confluence_image_source_generation": {
                "page_id": "page-a",
                "page_version": 2,
                "content_hash": "hash-a",
                "complete": True,
            },
        },
        image_segments=[{"segment_id": "old-image"}],
    )
    worker, _, vector_store, _ = make_worker(database=database)

    await worker._prepare_document_generation(
        KnowledgeIngestTask("dataset-a", "document-a"),
        connection=SimpleNamespace(name="owner"),
    )

    # T1: a rebuildable image source passes preflight without sweeping; the
    # old image generation stays until the ingest path replaces it atomically.
    assert vector_store.calls == []
    assert database.image_segments == [{"segment_id": "old-image"}]


@pytest.mark.asyncio
@pytest.mark.parametrize("large", [False, True])
@pytest.mark.parametrize("hierarchical", [False, True])
async def test_explicit_multimodal_always_uses_complete_standard_ingestion(
    large: bool,
    hierarchical: bool,
) -> None:
    database = GenerationDatabase(
        metadata={
            "processing_mode": "multimodal",
            "original_file_key": "knowledge/documents/tenant-a/document-a/original/doc.pdf",
        }
    )
    database.document["size_bytes"] = 101 if large else 1
    worker, _, _, service = make_worker(
        database=database,
        hierarchical=hierarchical,
    )
    worker._process_large_file = AsyncMock(  # type: ignore[method-assign]
        side_effect=AssertionError("multimodal must not use the text streaming path")
    )
    worker._process_with_hierarchical_indexer = AsyncMock(  # type: ignore[method-assign]
        side_effect=AssertionError("multimodal must not use the text hierarchy path")
    )

    await worker._process_task(KnowledgeIngestTask("dataset-a", "document-a"))

    assert service.ingest_calls == [("dataset-a", "document-a")]


class MultimodalDetection:
    recommended_mode = ProcessingMode.MULTIMODAL
    document_type = SimpleNamespace(value="mixed")
    confidence = 0.99

    @staticmethod
    def to_dict() -> dict[str, Any]:
        return {"recommended_mode": "multimodal", "confidence": 0.99}


class MultimodalDetector:
    async def detect(self, **_kwargs: Any) -> MultimodalDetection:
        return MultimodalDetection()


@pytest.mark.asyncio
@pytest.mark.parametrize("large", [False, True])
@pytest.mark.parametrize("hierarchical", [False, True])
async def test_auto_multimodal_cas_precedes_complete_standard_ingestion(
    large: bool,
    hierarchical: bool,
) -> None:
    database = GenerationDatabase(
        metadata={
            "processing_mode": "auto",
            "original_file_key": "knowledge/documents/tenant-a/document-a/original/doc.pdf",
        }
    )
    database.document["size_bytes"] = 101 if large else 1
    worker, _, _, service = make_worker(
        database=database,
        detector=MultimodalDetector(),  # type: ignore[arg-type]
        hierarchical=hierarchical,
    )
    worker._download_original_to_temp = AsyncMock(return_value="/tmp/document.pdf")  # type: ignore[method-assign]
    worker._cleanup_temp_file = AsyncMock()  # type: ignore[method-assign]
    worker._process_large_file = AsyncMock(  # type: ignore[method-assign]
        side_effect=AssertionError("detected multimodal must not stream text only")
    )
    worker._process_with_hierarchical_indexer = AsyncMock(  # type: ignore[method-assign]
        side_effect=AssertionError("detected multimodal must not use text hierarchy")
    )

    await worker._process_task(KnowledgeIngestTask("dataset-a", "document-a"))

    assert service.ingest_calls == [("dataset-a", "document-a")]
    assert database.events.index("mode-cas") < len(database.events)
    assert database.document["metadata"]["processing_mode"] == "multimodal"


@pytest.mark.asyncio
async def test_auto_mode_cas_failure_aborts_before_ingestion() -> None:
    database = GenerationDatabase(
        metadata={
            "processing_mode": "auto",
            "original_file_key": "knowledge/documents/tenant-a/document-a/original/doc.pdf",
        }
    )
    database.mode_publication_result = False
    worker, _, _, service = make_worker(
        database=database,
        detector=MultimodalDetector(),  # type: ignore[arg-type]
    )

    with pytest.raises(RuntimeError, match="lost document generation authority"):
        await worker._process_task(KnowledgeIngestTask("dataset-a", "document-a"))

    assert service.ingest_calls == []


class RunGenerationDatabase(GenerationDatabase):
    def __init__(self) -> None:
        super().__init__(status="waiting")

    @asynccontextmanager
    async def document_index_update_lease(
        self,
        dataset_id: str,
        document_id: str,
    ):
        assert (dataset_id, document_id) == ("dataset-a", "document-a")
        yield SimpleNamespace(name="owner")

    async def claim_queued_document_for_processing(
        self,
        dataset_id: str,
        document_id: str,
        *,
        connection: Any,
    ) -> bool:
        assert (dataset_id, document_id) == ("dataset-a", "document-a")
        assert connection is not None
        if self.document["status"] != "waiting":
            return False
        self.document["status"] = "parsing"
        return True


@pytest.mark.asyncio
async def test_noncompleted_processor_result_keeps_partial_generation_for_retry() -> None:
    database = RunGenerationDatabase()
    vector_store = GenerationVectorStore()
    worker, _, _, _ = make_worker(database=database, vector_store=vector_store)
    await worker.enqueue_claimed("dataset-a", "document-a")

    async def partial_hierarchical_result(
        _task: KnowledgeIngestTask,
        *,
        connection: Any = None,
        stage_receipt: Any = None,
    ) -> None:
        # PRD T9-1: the run loop now hands a stage receipt to _process_task.
        del connection, stage_receipt
        database.segments.add("partial-hierarchy-segment")
        database.summary_exists = True
        vector_store.points.add("partial-hierarchy-point")
        database.document["status"] = "error"
        worker._running = False

    worker._process_task = partial_hierarchical_result  # type: ignore[method-assign]
    worker._running = True
    await asyncio.wait_for(worker._run(), timeout=1)
    await asyncio.wait_for(worker.queue.join(), timeout=1)

    # T1: a failed generation must NOT be swept. The staged/partial rows and
    # the prior serving generation stay intact so the retry can resume them
    # (deterministic lineage ids), instead of rebuilding from zero.
    assert database.document["status"] == "error"
    assert database.segments == {"old-segment", "partial-hierarchy-segment"}
    assert database.summary_exists is True
    assert vector_store.points == {"old-point", "partial-hierarchy-point"}
    assert vector_store.calls == []
