"""A pending special restore reuses its historical source and execution."""

from __future__ import annotations

import hashlib
import uuid
from contextlib import asynccontextmanager
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge import special_publication, text_restore_candidate
from knowledge_service.services.knowledge.worker import (
    KnowledgeIngestTask,
    KnowledgeWorker,
    ReplayConfigSnapshot,
)


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["text", "hierarchy", "vision"])
async def test_special_restore_publishes_pinned_historical_source(
    monkeypatch: pytest.MonkeyPatch, kind: str,
) -> None:
    execution_id = uuid.uuid4().hex  # PostgreSQL pipeline receipts use UUID hex.
    generation_id = str(uuid.UUID(execution_id))
    content = (
        "[Page 1]\nold OCR" if kind == "vision" else
        "old ordinary body" if kind == "text" else "old hierarchy body"
    )
    content_hash = hashlib.sha256(content.encode()).hexdigest()
    pdf_bytes = b"verified historical pdf"
    source_hash = hashlib.sha256(pdf_bytes if kind == "vision" else content.encode()).hexdigest()
    index_config = {"chunking": {"mode": "hierarchical"}}
    pages = [{"page_number": 1, "text": "old OCR", "sha256": "a" * 64,
              "storage_key": "historical-page"}] if kind == "vision" else []
    source = {
        "source_kind": kind,
        "content_hash": content_hash,
        "source_hash": source_hash,
        "index_config": index_config,
        "original_source_key": (
            "knowledge/documents/tenant-a/doc-a/original/old.pdf" if kind == "vision" else ""
        ),
        "page_texts": pages,
        "objects": {},
    }
    candidate = {
        "change_type": "pending_restore", "content": content,
        "content_hash": content_hash,
        "metadata": {special_publication.SOURCE_MANIFEST_KEY: source},
    }
    database = SimpleNamespace(
        get_document_version=AsyncMock(return_value=candidate),
        record_special_publication_manifest=AsyncMock(),
        get_special_publication_manifest=AsyncMock(return_value={"phase": "committed"}),
        update_document_status=AsyncMock(),
    )
    storage = SimpleNamespace(download_original_file=AsyncMock(return_value=pdf_bytes))
    plan = SimpleNamespace(
        content=content, segment_ids=["segment-new"],
        points_by_collection={"base": []}, l1_count=1, l2_count=0, l3_count=1,
        total_pages=1, processed_pages=1,
        object_manifest=[{"storage_key": "new-page", "sha256": "a" * 64}],
    )
    hierarchy = SimpleNamespace(prepare_document=AsyncMock(return_value=plan))
    vision = SimpleNamespace(
        planned_object_keys=AsyncMock(return_value=["new-page"]),
        prepare_document=AsyncMock(return_value=plan),
    )
    published: list[dict[str, Any]] = []
    text_prepares: list[dict[str, Any]] = []

    class TextPlanner:
        def __init__(self, _service: Any) -> None:
            pass

        async def prepare(self, **kwargs: Any) -> Any:
            text_prepares.append(kwargs)
            return plan

    class Coordinator:
        def __init__(self, _service: Any) -> None:
            pass

        @staticmethod
        def _object_plan(_plan: Any) -> dict[str, str]:
            return {"new-page": "a" * 64}

        @staticmethod
        def _vision_page_texts(_plan: Any, _objects: Any) -> list[dict[str, Any]]:
            return pages

        async def publish(self, _plan: Any, _dataset: Any, _execution_id: str,
                          _expected: str, **kwargs: Any) -> list[str]:
            assert _execution_id == execution_id and _plan is plan
            assert _expected == "currently serving body"
            published.append(kwargs)
            return ["segment-new"]

        async def abort_preparing(self, *_args: Any, **_kwargs: Any) -> bool:
            raise AssertionError("successful restore must not abort")

    monkeypatch.setattr(special_publication, "SpecialPublicationCoordinator", Coordinator)
    monkeypatch.setattr(text_restore_candidate, "TextRestoreCandidatePlanner", TextPlanner)
    service = SimpleNamespace(
        db=database, settings=SimpleNamespace(knowledge=None),
        image_storage_service=storage,
    )
    worker = KnowledgeWorker(service, hierarchical_indexer=hierarchy, vision_processor=vision)
    snapshot = ReplayConfigSnapshot(
        index_config=index_config, processing_mode="scanned" if kind == "vision" else "text_only",
        restore_source_version=1, restore_source_kind=kind,
        restore_source_hash=source_hash,
    )

    result = await worker._process_special_restore(
        KnowledgeIngestTask(dataset_id="dataset-a", document_id="doc-a"),
        {"content": "currently serving body", "metadata": {
            "_document_pending_restore_version": {"candidate_version": 3},
            "_document_pipeline_execution_id": execution_id,
        }},
        {"dataset_id": "dataset-a", "tenant_id": "tenant-a",
         "collection_name": "base", "index_config": index_config},
        snapshot,
    )

    assert result == ["segment-new"]
    assert database.record_special_publication_manifest.await_args.kwargs == {
        "source_hash": source_hash,
        "planned_object_keys": ["new-page"] if kind == "vision" else [],
    }
    assert database.record_special_publication_manifest.await_args.args[3] == generation_id
    assert published[0]["document_shared_lease_held"] is True
    database.update_document_status.assert_awaited_once_with(
        "doc-a", status="indexing", progress=75,
    )
    if kind == "vision":
        storage.download_original_file.assert_awaited_once_with(source["original_source_key"])
        assert vision.prepare_document.await_args.kwargs["pinned_page_texts"] == {1: "old OCR"}
        assert vision.prepare_document.await_args.kwargs["generation_id"] == generation_id
        assert "text_extractor" not in vision.prepare_document.await_args.kwargs
    elif kind == "hierarchy":
        storage.download_original_file.assert_not_awaited()
        assert hierarchy.prepare_document.await_args.kwargs["text"] == content
        assert hierarchy.prepare_document.await_args.kwargs["generation_id"] == generation_id
        assert hierarchy.prepare_document.await_args.kwargs["metadata"] == candidate["metadata"]
    else:
        storage.download_original_file.assert_not_awaited()
        hierarchy.prepare_document.assert_not_awaited()
        assert text_prepares[0]["content"] == content
        assert text_prepares[0]["generation_id"] == generation_id


@pytest.mark.asyncio
async def test_text_receipt_refuses_unversioned_standard_reprocess() -> None:
    worker = KnowledgeWorker(SimpleNamespace(
        db=SimpleNamespace(), settings=SimpleNamespace(knowledge=None),
    ))
    with pytest.raises(ValidationFailedError, match="specialized"):
        await worker.require_safe_special_replay_admission(
            KnowledgeIngestTask(dataset_id="dataset-a", document_id="doc-a"),
            {"metadata": {
                "processing_mode": "text_only",
                special_publication.SOURCE_MANIFEST_KEY: {"source_kind": "text"},
            }},
            {"tenant_id": "tenant-a", "index_config": {"chunking": {"mode": "automatic"}}},
            action="reprocess",
        )


@pytest.mark.asyncio
async def test_text_receipt_refuses_unverified_legacy_restore() -> None:
    database = SimpleNamespace(get_document=AsyncMock(return_value={
        "metadata": {special_publication.SOURCE_MANIFEST_KEY: {"source_kind": "text"}},
    }))
    worker = KnowledgeWorker(SimpleNamespace(
        db=database, settings=SimpleNamespace(knowledge=None),
    ))

    with pytest.raises(ValidationFailedError, match="specialized"):
        await worker.require_safe_restore_admission(
            KnowledgeIngestTask(dataset_id="dataset-a", document_id="doc-a"),
            {"tenant_id": "tenant-a"},
        )


@pytest.mark.asyncio
async def test_unbound_preparing_crash_requeues_original_execution(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    generation = "00000000-0000-4000-8000-000000000101"
    item = {
        "execution_id": generation, "generation_id": generation,
        "dataset_id": "dataset-a", "document_id": "doc-a",
        "source_hash": "a" * 64,
    }

    @asynccontextmanager
    async def document_lease(_dataset_id: str, _document_id: str):
        yield object()

    database = SimpleNamespace(
        list_unbound_special_preparations=AsyncMock(return_value=[item]),
        document_index_update_lease=document_lease,
        get_special_publication_manifest=AsyncMock(return_value={
            "phase": "preparing", "publication_revision": None,
            "generation_id": generation,
        }),
        get_dataset=AsyncMock(return_value={"dataset_id": "dataset-a"}),
        list_unfinished_special_publication_datasets=AsyncMock(return_value=[]),
    )
    resumed: list[dict[str, Any]] = []

    class Coordinator:
        def __init__(self, _service: Any) -> None:
            pass

        async def abort_preparing(self, *_args: Any, **kwargs: Any) -> bool:
            resumed.append(kwargs)
            return True

    monkeypatch.setattr(special_publication, "SpecialPublicationCoordinator", Coordinator)
    worker = KnowledgeWorker(SimpleNamespace(
        db=database, settings=SimpleNamespace(knowledge=None),
    ))
    worker.enqueue_claimed = AsyncMock()  # type: ignore[method-assign]
    worker.enqueue = AsyncMock(side_effect=AssertionError("no new execution"))  # type: ignore[method-assign]

    await worker._recover_unfinished_special_publications()

    assert resumed == [{
        "document_shared_lease_held": True,
        "resume_same_execution": True,
    }]
    worker.enqueue_claimed.assert_awaited_once_with("dataset-a", "doc-a")
    worker.enqueue.assert_not_awaited()
