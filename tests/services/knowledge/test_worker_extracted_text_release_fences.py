from __future__ import annotations

import sys
from copy import deepcopy
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge import extracted_text_budget as budget_module
from knowledge_service.services.knowledge import worker as worker_module
from knowledge_service.services.knowledge.processing_mode import ProcessingMode
from knowledge_service.services.knowledge.worker import (
    DurableEnqueueProxy,
    KnowledgeIngestTask,
    KnowledgeWorker,
)

TASK = KnowledgeIngestTask(dataset_id="dataset-a", document_id="document-a")


class _VectorStore:
    def __init__(self) -> None:
        self.delete_calls: list[dict[str, Any]] = []
        self.documents_with_points: set[str] = set()
        self.documents_with_specialized_points: set[str] = set()

    async def document_has_points(
        self, *, tenant_id: str, dataset_id: str, document_id: str,
    ) -> bool:
        assert (tenant_id, dataset_id) == ("tenant-a", "dataset-a")
        return document_id in self.documents_with_points

    async def document_has_specialized_points(
        self, *, tenant_id: str, dataset_id: str, document_id: str,
    ) -> bool:
        assert (tenant_id, dataset_id) == ("tenant-a", "dataset-a")
        return document_id in self.documents_with_specialized_points

    async def delete_document_points(self, **kwargs: Any) -> list[str]:
        self.delete_calls.append(dict(kwargs))
        return []


class _Database:
    def __init__(self, *, content: str | None = None) -> None:
        self.events: list[str] = []
        self.completed_segment_ids: set[str] = set()
        self.specialized_segment_ids: set[str] = set()
        self.dataset = {
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
            "collection_name": "collection-a",
            "index_config": {},
        }
        self.document = {
            "document_id": "document-a",
            "dataset_id": "dataset-a",
            "source_type": "upload",
            "status": "parsing",
            "enabled": True,
            "archived": False,
            "content": content,
            "metadata": {"processing_mode": "text_only"},
        }

    async def get_dataset(
        self,
        dataset_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any]:
        del connection
        assert dataset_id == "dataset-a"
        self.events.append("get-dataset")
        return deepcopy(self.dataset)

    async def get_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any]:
        del connection
        assert document_id == "document-a"
        self.events.append("get-document")
        return deepcopy(self.document)

    async def document_has_completed_segments(
        self, dataset_id: str, tenant_id: str, document_id: str,
    ) -> bool:
        assert (dataset_id, tenant_id) == ("dataset-a", "tenant-a")
        return document_id in self.completed_segment_ids

    async def document_has_specialized_segments(
        self, dataset_id: str, tenant_id: str, document_id: str,
        *, connection: Any | None = None,
    ) -> bool:
        del connection
        assert (dataset_id, tenant_id) == ("dataset-a", "tenant-a")
        return document_id in self.specialized_segment_ids

    async def get_image_segments_by_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> list[dict[str, Any]]:
        del connection
        assert document_id == "document-a"
        self.events.append("get-image-segments")
        raise AssertionError("oversized stored content reached the generation sweep")

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
        self.events.append(f"status:{status}")

    async def execute(self, *_args: Any, **_kwargs: Any) -> None:
        self.events.append("content-sql")

    async def update_document_content(self, document_id: str, _content: str) -> None:
        assert document_id == "document-a"
        self.events.append("content-update")

    async def update_document_fields(
        self,
        document_id: str,
        _fields: dict[str, Any],
        **_kwargs: Any,
    ) -> None:
        assert document_id == "document-a"
        self.events.append("fields-update")


class _Service:
    def __init__(self, database: _Database) -> None:
        self.db = database
        self.vector_store = _VectorStore()
        self.settings = SimpleNamespace(
            knowledge=SimpleNamespace(
                large_file_threshold=1,
                pdf_split_enabled=True,
                pdf_split_max_size_bytes=1,
                pdf_split_min_pages_per_part=1,
                ocr_enabled=True,
                ocr_strategy="hybrid",
                streaming_batch_size=1,
            )
        )
        self.image_storage_service = SimpleNamespace(
            download_original_file=AsyncMock(return_value=b"%PDF-fake")
        )
        self.ingest_calls: list[dict[str, Any]] = []
        self._worker: KnowledgeWorker | None = None

    async def ingest_document(self, dataset_id: str, document_id: str, **kwargs: Any) -> None:
        self.ingest_calls.append({"dataset_id": dataset_id, "document_id": document_id, **kwargs})


def _set_tiny_text_budget(
    monkeypatch: pytest.MonkeyPatch,
    *,
    chars: int,
    bytes_: int = 1_000,
) -> None:
    # The worker imports the enforcement functions, whose limits live in the
    # owning budget module globals. Patch the authoritative limits, not a test copy.
    monkeypatch.setattr(budget_module, "MAX_EXTRACTED_TEXT_CHARS", chars)
    monkeypatch.setattr(budget_module, "MAX_EXTRACTED_TEXT_BYTES", bytes_)


@pytest.mark.asyncio
async def test_prepare_rejects_oversized_stored_content_before_generation_side_effects(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _set_tiny_text_budget(monkeypatch, chars=8)
    database = _Database(content="123456789")
    service = _Service(database)
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]

    with pytest.raises(ValidationFailedError, match="8 character limit"):
        await worker._prepare_document_generation(
            TASK,
            connection=SimpleNamespace(name="owner"),
        )

    assert database.events == ["get-dataset", "get-document"]
    assert service.vector_store.delete_calls == []
    assert service.ingest_calls == []


class _StreamingLoader:
    def __init__(self, **_kwargs: Any) -> None:
        pass

    async def iter_batches(self, _path: str, _on_progress: Any):
        for page_number in (1, 2):
            yield SimpleNamespace(
                total_pages=2,
                pages=[
                    SimpleNamespace(
                        page_number=page_number,
                        text="abcde",
                        images=[],
                    )
                ],
                batch_index=page_number,
                start_page=page_number,
                end_page=page_number,
            )


@pytest.mark.asyncio
async def test_large_file_rejects_cumulative_batch_before_second_temp_append_or_index(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    _set_tiny_text_budget(monkeypatch, chars=20)
    monkeypatch.setattr(worker_module, "StreamingDocumentLoader", _StreamingLoader)
    database = _Database()
    service = _Service(database)
    hierarchy = SimpleNamespace(index_document=AsyncMock())
    worker = KnowledgeWorker(  # type: ignore[arg-type]
        service,
        hierarchical_indexer=hierarchy,
    )
    appended: list[str] = []
    worker._append_text = lambda _path, text: appended.append(text)  # type: ignore[method-assign]
    source_path = tmp_path / "source.pdf"
    source_path.write_bytes(b"%PDF-fake")
    doc = {
        "metadata": {
            "original_file_key": "knowledge/originals/document-a.pdf",
        }
    }

    with pytest.raises(ValidationFailedError, match="20 character limit"):
        await worker._process_large_file(
            TASK,
            doc,
            ProcessingMode.TEXT_ONLY,
            source_path=str(source_path),
        )

    assert appended == ["[Page 1]\nabcde\n\n"]
    assert database.events == []
    hierarchy.index_document.assert_not_awaited()
    assert service.ingest_calls == []


@pytest.mark.asyncio
async def test_large_file_passes_text_candidate_without_early_content_write(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(worker_module, "StreamingDocumentLoader", _StreamingLoader)
    database = _Database(content="old serving text")
    service = _Service(database)
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]
    source_path = tmp_path / "source.pdf"
    source_path.write_bytes(b"%PDF-fake")

    await worker._process_large_file(
        TASK,
        {"metadata": {"original_file_key": "knowledge/originals/document-a.pdf"}},
        ProcessingMode.TEXT_ONLY,
        source_path=str(source_path),
        index_config_override={"chunking": {"mode": "automatic"}},
    )

    assert database.document["content"] == "old serving text"
    assert "content-update" not in database.events
    assert "content-sql" not in database.events
    assert service.ingest_calls[0]["source_text_override"].count("[Page") == 2
    assert service.ingest_calls[0]["candidate_metadata_patch"] == {
        "total_pages": 2,
        "streaming_processed": True,
    }


@pytest.mark.asyncio
async def test_direct_hierarchical_rejects_extracted_text_before_status_or_index(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _set_tiny_text_budget(monkeypatch, chars=8)
    database = _Database()
    service = _Service(database)
    hierarchy = SimpleNamespace(index_document=AsyncMock())
    worker = KnowledgeWorker(  # type: ignore[arg-type]
        service,
        hierarchical_indexer=hierarchy,
    )
    worker._extract_text_from_content = AsyncMock(  # type: ignore[method-assign]
        return_value="123456789"
    )
    doc = {
        "metadata": {
            "original_file_key": "knowledge/originals/document-a.txt",
            "mime_type": "text/plain",
        }
    }

    with pytest.raises(ValidationFailedError, match="8 character limit"):
        await worker._process_with_hierarchical_indexer(
            TASK,
            doc,
            ProcessingMode.TEXT_ONLY,
        )

    assert database.events == []
    hierarchy.index_document.assert_not_awaited()
    assert service.ingest_calls == []


@pytest.mark.asyncio
async def test_existing_hierarchical_reprocess_fails_before_multicollection_write() -> None:
    database = _Database(content="old serving text")
    database.completed_segment_ids.add("document-a")
    service = _Service(database)
    hierarchy = SimpleNamespace(index_document=AsyncMock())
    worker = KnowledgeWorker(service, hierarchical_indexer=hierarchy)  # type: ignore[arg-type]
    doc = {
        "metadata": {
            "_document_ingest_action": "reprocess",
            "original_file_key": "knowledge/originals/document-a.txt",
        }
    }

    with pytest.raises(ValidationFailedError, match="specialized_rebuild_unavailable"):
        await worker._process_with_hierarchical_indexer(
            TASK, doc, ProcessingMode.TEXT_ONLY,
        )
    hierarchy.index_document.assert_not_awaited()
    assert database.document["content"] == "old serving text"
    assert "content-update" not in database.events


@pytest.mark.asyncio
async def test_existing_scanned_reprocess_fails_before_image_batch_write() -> None:
    database = _Database(content="old serving text")
    database.completed_segment_ids.add("document-a")
    service = _Service(database)
    vision = SimpleNamespace(process=AsyncMock())
    worker = KnowledgeWorker(service, vision_processor=vision)  # type: ignore[arg-type]
    doc = {
        "metadata": {
            "_document_ingest_action": "reprocess",
            "original_file_key": "knowledge/originals/document-a.pdf",
        }
    }

    with pytest.raises(ValidationFailedError, match="specialized_rebuild_unavailable"):
        await worker._process_scanned(TASK, doc)
    vision.process.assert_not_awaited()
    assert database.document["content"] == "old serving text"


@pytest.mark.asyncio
async def test_first_special_recover_without_published_rows_can_resume() -> None:
    database = _Database()
    service = _Service(database)
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]

    await worker._require_special_rebuild_without_serving_version(
        TASK, {"metadata": {"_document_ingest_action": "recover"}},
        pathway="hierarchical",
    )


@pytest.mark.asyncio
async def test_special_recover_refuses_orphan_vector_point_without_segment() -> None:
    database = _Database()
    service = _Service(database)
    service.vector_store.documents_with_points.add("document-a")
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]

    with pytest.raises(ValidationFailedError, match="specialized_rebuild_unavailable"):
        await worker._require_special_rebuild_without_serving_version(
            TASK, {"metadata": {"_document_ingest_action": "recover"}},
            pathway="scanned image",
        )


@pytest.mark.asyncio
async def test_missing_scanned_processor_cannot_fallback_over_old_image_points() -> None:
    database = _Database()
    service = _Service(database)
    service.vector_store.documents_with_points.add("document-a")
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]

    with pytest.raises(ValidationFailedError, match="specialized_rebuild_unavailable"):
        await worker.require_safe_special_replay_admission(
            TASK, {"metadata": {"processing_mode": "scanned"}},
            database.dataset, action="recover",
        )


@pytest.mark.asyncio
async def test_missing_hierarchy_and_original_cannot_fallback_over_old_points() -> None:
    database = _Database()
    database.dataset["index_config"] = {"chunking": {"mode": "hierarchical"}}
    service = _Service(database)
    service.vector_store.documents_with_points.add("document-a")
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]

    with pytest.raises(ValidationFailedError, match="specialized_rebuild_unavailable"):
        await worker.require_safe_special_replay_admission(
            TASK, {"metadata": {"processing_mode": "text_only"}},
            database.dataset, action="reprocess",
        )


@pytest.mark.asyncio
async def test_multimodal_receipt_blocks_replay_over_old_text_only_rows() -> None:
    database = _Database(content="old serving text")
    database.completed_segment_ids.add("document-a")
    service = _Service(database)
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]
    document = {
        "metadata": {
            "processing_mode": "multimodal",
            "extracted_images": [{"storage_url": "images/example.png"}],
            "image_count": 1,
        }
    }

    with pytest.raises(ValidationFailedError, match="specialized_rebuild_unavailable"):
        await worker.require_safe_special_replay_admission(
            TASK, document, database.dataset, action="recover",
        )


@pytest.mark.asyncio
async def test_multimodal_receipt_without_existing_rows_allows_first_recover() -> None:
    database = _Database()
    service = _Service(database)
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]

    await worker.require_safe_special_replay_admission(
        TASK,
        {"metadata": {"processing_mode": "multimodal", "image_count": 1}},
        database.dataset,
        action="recover",
    )


@pytest.mark.asyncio
async def test_version_restore_refuses_image_receipt_without_image_rows() -> None:
    database = _Database(content="old serving text")
    database.document["metadata"] = {
        "processing_mode": "multimodal",
        "extracted_images": [{"storage_url": "images/example.png"}],
    }
    service = _Service(database)
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]

    with pytest.raises(ValidationFailedError, match="specialized_rebuild_unavailable"):
        await worker.require_safe_restore_admission(TASK, database.dataset)


@pytest.mark.asyncio
async def test_api_producer_uses_same_replay_and_restore_admission() -> None:
    database = _Database(content="old serving text")
    database.completed_segment_ids.add("document-a")
    database.document["metadata"] = {
        "processing_mode": "multimodal", "image_count": 1,
    }
    service = _Service(database)
    producer = DurableEnqueueProxy(service)  # type: ignore[arg-type]

    with pytest.raises(ValidationFailedError, match="specialized_rebuild_unavailable"):
        await producer.require_safe_special_replay_admission(
            TASK, database.document, database.dataset, action="reprocess",
        )
    with pytest.raises(ValidationFailedError, match="specialized_rebuild_unavailable"):
        await producer.require_safe_restore_admission(TASK, database.dataset)


@pytest.mark.asyncio
@pytest.mark.parametrize("source", ["segments", "points"])
async def test_restore_refuses_old_specialized_generation(source: str) -> None:
    database = _Database()
    service = _Service(database)
    if source == "segments":
        database.specialized_segment_ids.add("document-a")
    else:
        service.vector_store.documents_with_specialized_points.add("document-a")
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]

    with pytest.raises(ValidationFailedError, match="specialized_rebuild_unavailable"):
        await worker.require_safe_restore_admission(TASK, database.dataset)


class _Pixmap:
    width = 1
    height = 1
    n = 3

    @staticmethod
    def tobytes(format_: str) -> bytes:
        assert format_ == "png"
        return b"png"


class _PdfPage:
    @staticmethod
    def get_pixmap(*, matrix: Any) -> _Pixmap:
        assert matrix is not None
        return _Pixmap()


class _PdfDocument:
    def __init__(self) -> None:
        self.pages = [_PdfPage(), _PdfPage()]
        self.closed = False

    def __len__(self) -> int:
        return len(self.pages)

    def __getitem__(self, index: int) -> _PdfPage:
        return self.pages[index]

    def close(self) -> None:
        self.closed = True


@pytest.mark.asyncio
async def test_vlm_ocr_rejects_cumulative_page_text_before_content_or_ingestion(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _set_tiny_text_budget(monkeypatch, chars=20)
    pdf_document = _PdfDocument()
    monkeypatch.setitem(
        sys.modules,
        "fitz",
        SimpleNamespace(
            Matrix=lambda *_args: object(),
            open=lambda **_kwargs: pdf_document,
        ),
    )
    database = _Database()
    service = _Service(database)
    vlm_ocr = SimpleNamespace(
        ocr_pdf_pages=AsyncMock(return_value=["abcde", "fghij"]),
    )
    worker = KnowledgeWorker(  # type: ignore[arg-type]
        service,
        vlm_ocr_service=vlm_ocr,
    )
    doc = {
        "metadata": {
            "original_file_key": "knowledge/originals/document-a.pdf",
        }
    }

    with pytest.raises(ValidationFailedError, match="20 character limit"):
        await worker._process_scanned_with_vlm_ocr(TASK, doc)

    assert pdf_document.closed is True
    assert database.events == ["status:parsing"]
    assert service.ingest_calls == []


@pytest.mark.asyncio
async def test_vlm_ocr_passes_text_candidate_without_early_content_write(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    pdf_document = _PdfDocument()
    monkeypatch.setitem(
        sys.modules,
        "fitz",
        SimpleNamespace(
            Matrix=lambda *_args: object(),
            open=lambda **_kwargs: pdf_document,
        ),
    )
    database = _Database(content="old serving text")
    service = _Service(database)
    worker = KnowledgeWorker(  # type: ignore[arg-type]
        service,
        vlm_ocr_service=SimpleNamespace(
            ocr_pdf_pages=AsyncMock(return_value=["new page one", "new page two"]),
        ),
    )

    await worker._process_scanned_with_vlm_ocr(
        TASK,
        {"metadata": {"original_file_key": "knowledge/originals/document-a.pdf"}},
    )

    assert database.document["content"] == "old serving text"
    assert "content-update" not in database.events
    assert "fields-update" not in database.events
    assert "new page one" in service.ingest_calls[0]["source_text_override"]
    assert service.ingest_calls[0]["candidate_metadata_patch"]["vlm_ocr_pages"] == 2
