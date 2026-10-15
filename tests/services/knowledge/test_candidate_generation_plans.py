"""Candidate preparation must not publish the previous serving generation."""

from __future__ import annotations

import hashlib
from typing import Any

import pytest
from knowledge_service.services.knowledge.hierarchical_indexer import (
    HierarchicalIndexer,
    HierarchicalSegment,
    IndexLevel,
)
from knowledge_service.services.knowledge.vision_pdf_processor import VisionPDFProcessor

GENERATION_A = "00000000-0000-4000-8000-000000000001"
GENERATION_B = "00000000-0000-4000-8000-000000000002"


class _Embedder:
    dimension = 2

    def __init__(self, *, fail_images: bool = False) -> None:
        self.fail_images = fail_images

    async def embed_documents(self, texts: list[str]) -> list[list[float]]:
        return [[float(len(text)), 0.5] for text in texts]

    async def embed_images(self, images: list[bytes]) -> list[list[float]]:
        if self.fail_images:
            return [[0.1, 0.2]]
        return [[float(len(image)), 0.5] for image in images]


class _ReadOnlyDatabase:
    async def get_dataset(self, _dataset_id: str) -> dict[str, Any]:
        return {
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
            "collection_name": "collection-a",
        }

    async def insert_segments(self, *_args: Any, **_kwargs: Any) -> None:
        raise AssertionError("candidate preparation inserted serving segments")

    async def save_document_summary(self, *_args: Any, **_kwargs: Any) -> None:
        raise AssertionError("candidate preparation published a summary")


class _ReadOnlyVectors:
    async def ensure_collection(self, *_args: Any, **_kwargs: Any) -> None:
        raise AssertionError("candidate preparation created a serving collection")

    async def upsert(self, *_args: Any, **_kwargs: Any) -> None:
        raise AssertionError("candidate preparation published points")


class _SummaryGenerator:
    async def summarize_document(self, _text: str) -> dict[str, Any]:
        return {"summary": "document summary", "keywords": ["alpha"], "topics": []}


@pytest.mark.asyncio
async def test_hierarchy_candidate_is_complete_stable_and_unpublished() -> None:
    indexer = HierarchicalIndexer(
        _ReadOnlyVectors(),
        _ReadOnlyDatabase(),
        embedder=_Embedder(),
        summary_generator=_SummaryGenerator(),
    )
    text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda"

    first = await indexer.prepare_document(
        "document-a", "dataset-a", text, generation_id=GENERATION_A
    )
    replay = await indexer.prepare_document(
        "document-a", "dataset-a", text, generation_id=GENERATION_A
    )
    next_generation = await indexer.prepare_document(
        "document-a", "dataset-a", text, generation_id=GENERATION_B
    )
    changed_input = await indexer.prepare_document(
        "document-a", "dataset-a", text + " changed", generation_id=GENERATION_A
    )

    assert first.total_vectors > 0
    assert first.segment_ids == replay.segment_ids
    assert first.segment_ids == changed_input.segment_ids
    assert first.source_hash != changed_input.source_hash
    assert first.segment_ids != next_generation.segment_ids
    assert set(first.points_by_collection) == {
        "collection-a", "collection-a_sections", "collection-a_summary"
    }
    assert {row["content_type"] for row in first.segment_rows} == {
        "text", "section", "document_summary"
    }
    assert len({
        (row["content_type"], row["position"]) for row in first.segment_rows
    }) == len(first.segment_rows)
    section_ids = {
        row["segment_id"] for row in first.segment_rows
        if row["content_type"] == "section"
    }
    assert all(
        row["parent_segment_id"] in section_ids
        for row in first.segment_rows if row["content_type"] == "text"
    )
    assert first.summary_row is not None
    assert first.summary_row["vector_id"] in first.segment_ids
    assert first.object_manifest == []
    assert first.source_hash == hashlib.sha256(text.encode("utf-8")).hexdigest()


@pytest.mark.asyncio
@pytest.mark.parametrize("bad_vector", [[0.25], [float("nan"), 0.5]])
async def test_hierarchy_candidate_refuses_invalid_provider_vector(
    bad_vector: list[float],
) -> None:
    class InvalidEmbedder(_Embedder):
        async def embed_documents(self, texts: list[str]) -> list[list[float]]:
            return [bad_vector for _ in texts]

    indexer = HierarchicalIndexer(
        _ReadOnlyVectors(), _ReadOnlyDatabase(),
        embedder=InvalidEmbedder(),
    )

    with pytest.raises(RuntimeError, match="L3 embedding is incomplete or invalid"):
        await indexer.prepare_document(
            "document-a", "dataset-a", "alpha beta gamma delta epsilon",
            generation_id=GENERATION_A,
        )


@pytest.mark.asyncio
async def test_hierarchy_reprocess_resolves_current_model_for_each_generation() -> None:
    class MutableDatabase(_ReadOnlyDatabase):
        model = "model-a"

        async def get_dataset(self, dataset_id: str) -> dict[str, Any]:
            return {**await super().get_dataset(dataset_id), "embedding_model": self.model}

    database = MutableDatabase()

    class ModelEmbedder(_Embedder):
        def __init__(self, model: str) -> None:
            self.model = model
            self.closed = False

        async def embed_documents(self, texts: list[str]) -> list[list[float]]:
            if self.model == "invalid-model":
                raise RuntimeError("configured embedding model rejected")
            if self.model == "model-a":
                # A config update between layer calls cannot mix L3 and L2
                # embeddings in one unpublished candidate.
                database.model = "model-b"
            value = 1.0 if self.model == "model-a" else 2.0
            return [[value, 0.5] for _ in texts]

        async def close(self) -> None:
            self.closed = True

    resolved: list[ModelEmbedder] = []

    async def resolve(dataset_id: str) -> ModelEmbedder:
        dataset = await database.get_dataset(dataset_id)
        embedder = ModelEmbedder(dataset["embedding_model"])
        resolved.append(embedder)
        return embedder

    indexer = HierarchicalIndexer(
        _ReadOnlyVectors(), database, embedding_resolver=resolve,
    )
    options = {
        "document_id": "document-a", "dataset_id": "dataset-a",
        "text": "alpha beta gamma delta epsilon",
    }
    first = await indexer.prepare_document(generation_id=GENERATION_A, **options)
    database.model = "model-b"
    second = await indexer.prepare_document(generation_id=GENERATION_B, **options)
    assert all(
        point.vector == [1.0, 0.5]
        for points in first.points_by_collection.values() for point in points
    )
    assert all(
        point.vector == [2.0, 0.5]
        for points in second.points_by_collection.values() for point in points
    )

    database.model = "invalid-model"
    with pytest.raises(RuntimeError, match="embedding is incomplete"):
        await indexer.prepare_document(generation_id=GENERATION_A, **options)
    assert [embedder.model for embedder in resolved] == [
        "model-a", "model-b", "invalid-model",
    ]
    assert all(embedder.closed for embedder in resolved)


@pytest.mark.asyncio
async def test_hierarchy_candidate_metadata_cannot_replace_serving_identity() -> None:
    indexer = HierarchicalIndexer(
        _ReadOnlyVectors(), _ReadOnlyDatabase(), embedder=_Embedder(),
        summary_generator=_SummaryGenerator(),
    )
    plan = await indexer.prepare_document(
        "document-a", "dataset-a", "alpha beta gamma delta epsilon",
        generation_id=GENERATION_A,
        metadata={
            "tenant_id": "other-tenant", "dataset_id": "other-dataset",
            "document_id": "other-document", "segment_id": "other-segment",
            "text": "injected", "content_type": "image",
            "parent_segment_id": "other-parent", "source_hash": "forged",
            "heading": "Real heading",
        },
    )
    for points in plan.points_by_collection.values():
        for point in points:
            assert point.payload["tenant_id"] == "tenant-a"
            assert point.payload["dataset_id"] == "dataset-a"
            assert point.payload["document_id"] == "document-a"
            assert point.payload["segment_id"] == point.id
            assert point.payload["text"] != "injected"
            assert point.payload["content_type"] != "image"
            assert "source_hash" not in point.payload
    assert any(
        point.payload.get("heading") == "Real heading"
        for points in plan.points_by_collection.values() for point in points
    )


@pytest.mark.asyncio
async def test_hierarchy_candidate_excludes_document_source_and_private_metadata() -> None:
    indexer = HierarchicalIndexer(
        _ReadOnlyVectors(), _ReadOnlyDatabase(), embedder=_Embedder(),
        summary_generator=_SummaryGenerator(),
    )
    plan = await indexer.prepare_document(
        "document-a", "dataset-a", "alpha beta gamma delta epsilon",
        generation_id=GENERATION_A,
        metadata={
            "category": "handbook",
            "_special_source_manifest": {"objects": {"secret-key": "old-hash"}},
            "_document_pipeline_execution_id": "other-execution",
            "_private_note": "not for retrieval",
            "original_file_key": "source/private-original.pdf",
            "processing_mode": "scanned",
            "source_reference": {"internal": "credential"},
        },
    )
    forbidden = {
        "_special_source_manifest", "_document_pipeline_execution_id",
        "_private_note", "original_file_key", "processing_mode",
        "source_reference",
    }
    for row in plan.segment_rows:
        assert row["metadata"].get("category") == "handbook"
        assert forbidden.isdisjoint(row["metadata"])
    for points in plan.points_by_collection.values():
        for point in points:
            assert forbidden.isdisjoint(point.payload)


@pytest.mark.asyncio
async def test_legacy_hierarchy_l3_metadata_cannot_replace_serving_identity() -> None:
    class RecordingVectors:
        def __init__(self) -> None:
            self.points: list[Any] = []

        async def ensure_collection(self, **kwargs: Any) -> str:
            return kwargs["collection_name"]

        async def upsert(self, *, collection_name: str, points: list[Any]) -> None:
            del collection_name
            self.points.extend(points)

    class RecordingDatabase(_ReadOnlyDatabase):
        async def insert_segments(self, rows: list[dict[str, Any]]) -> None:
            self.rows = rows

    vectors = RecordingVectors()
    database = RecordingDatabase()
    indexer = HierarchicalIndexer(vectors, database, embedder=_Embedder())
    segment = HierarchicalSegment(
        segment_id="segment-a", document_id="document-a", dataset_id="dataset-a",
        level=IndexLevel.PARAGRAPH, text="grounded text", position=1,
        parent_id="parent-a",
        metadata={
            "tenant_id": "other-tenant", "document_id": "other-document",
            "segment_id": "other-segment", "text": "injected",
            "content_type": "image", "parent_segment_id": "other-parent",
        },
    )
    await indexer._index_segments([segment], "dataset-a", 2, "collection-a")
    payload = vectors.points[0].payload
    assert payload["tenant_id"] == "tenant-a"
    assert payload["document_id"] == "document-a"
    assert payload["segment_id"] == "segment-a"
    assert payload["text"] == "grounded text"
    assert payload["content_type"] == "text"
    assert payload["parent_segment_id"] == "parent-a"


class _ObjectStorage:
    def __init__(self, *, fail_page: int | None = None) -> None:
        self.fail_page = fail_page
        self.objects: dict[str, bytes] = {}
        self.deleted: list[str] = []
        self.uploaded: list[str] = []

    @staticmethod
    def _generate_key(
        tenant_id: str, document_id: str, attachment_id: str, filename: str
    ) -> str:
        return f"{tenant_id}/{document_id}/{attachment_id}_{filename}"

    async def upload_image(self, **kwargs: Any) -> str:
        if self.fail_page is not None and kwargs["attachment_id"].startswith(
            f"page_{self.fail_page}_"
        ):
            raise RuntimeError("injected image upload failure")
        key = self._generate_key(
            kwargs["tenant_id"], kwargs["document_id"],
            kwargs["attachment_id"], kwargs["filename"],
        )
        self.objects[key] = kwargs["content"]
        self.uploaded.append(key)
        return f"storage://{key}"

    async def image_exists(self, **kwargs: Any) -> bool:
        key = self._generate_key(
            kwargs["tenant_id"], kwargs["document_id"],
            kwargs["attachment_id"], kwargs["filename"],
        )
        return key in self.objects

    async def download_image(self, **kwargs: Any) -> bytes:
        key = self._generate_key(
            kwargs["tenant_id"], kwargs["document_id"],
            kwargs["attachment_id"], kwargs["filename"],
        )
        return self.objects[key]

    def get_image_url(self, **kwargs: Any) -> str:
        key = self._generate_key(
            kwargs["tenant_id"], kwargs["document_id"],
            kwargs["attachment_id"], kwargs["filename"],
        )
        return f"storage://{key}"

    async def delete_image(self, **kwargs: Any) -> bool:
        key = self._generate_key(
            kwargs["tenant_id"], kwargs["document_id"],
            kwargs["attachment_id"], kwargs["filename"],
        )
        self.deleted.append(key)
        self.objects.pop(key, None)
        return True


def _vision_processor(*, fail_images: bool = False) -> VisionPDFProcessor:
    processor = VisionPDFProcessor(
        _Embedder(fail_images=fail_images),
        _ReadOnlyVectors(),
        _ReadOnlyDatabase(),
        batch_size=2,
    )
    processor._pdf_page_count = lambda _pdf: 2  # type: ignore[method-assign]
    processor._render_page_batch = (  # type: ignore[method-assign]
        lambda _pdf, start, stop: [
            (index, f"image-{index}".encode(), (100, 200))
            for index in range(start, stop)
        ]
    )
    return processor


@pytest.mark.asyncio
async def test_vision_candidate_uses_generation_scoped_objects_without_index_writes() -> None:
    storage = _ObjectStorage()
    processor = _vision_processor()
    first = await processor.prepare_document(
        b"pdf", "document-a", "dataset-a", "collection-a",
        generation_id=GENERATION_A, tenant_id="tenant-a", storage_service=storage,
        text_extractor=lambda image: _ocr_text(image),
    )
    replay = await processor.prepare_document(
        b"pdf", "document-a", "dataset-a", "collection-a",
        generation_id=GENERATION_A, tenant_id="tenant-a", storage_service=storage,
    )
    second = await processor.prepare_document(
        b"pdf", "document-a", "dataset-a", "collection-a",
        generation_id=GENERATION_B, tenant_id="tenant-a", storage_service=storage,
    )
    changed_input = await processor.prepare_document(
        b"pdf-changed", "document-a", "dataset-a", "collection-a",
        generation_id=GENERATION_A, tenant_id="tenant-a", storage_service=storage,
    )

    assert first.segment_ids == replay.segment_ids
    assert first.source_hash == hashlib.sha256(b"pdf").hexdigest()
    assert first.source_hash != changed_input.source_hash
    assert first.object_manifest == replay.object_manifest
    assert len(storage.uploaded) == 4  # Same-generation replay reuses its objects.
    assert all(
        receipt["sha256"] == hashlib.sha256(storage.objects[receipt["storage_key"]]).hexdigest()
        for receipt in first.object_manifest
    )
    assert first.segment_ids != second.segment_ids
    assert first.content == "[Page 1]\nimage-0\n\n[Page 2]\nimage-1"
    assert "image-0" in first.points_by_collection["collection-a"][0].payload["text"]
    assert "image-0" in first.segment_rows[0]["text"]
    assert first.processed_pages == first.total_pages == 2
    assert len(first.points_by_collection["collection-a"]) == 2
    assert all(row["status"] == "indexing" for row in first.segment_rows)
    assert len(storage.objects) == 4
    assert {
        receipt["storage_key"] for receipt in first.object_manifest
    }.isdisjoint({receipt["storage_key"] for receipt in second.object_manifest})


async def _ocr_text(image: bytes) -> str:
    return image.decode()


@pytest.mark.asyncio
async def test_vision_planned_object_keys_match_upload_manifest_without_writes() -> None:
    storage = _ObjectStorage()
    processor = _vision_processor()
    planned = await processor.planned_object_keys(
        b"pdf", "document-a", generation_id=GENERATION_A,
        tenant_id="tenant-a", storage_service=storage, page_offset=3,
    )
    assert planned == [
        storage._generate_key(
            "tenant-a", "document-a",
            f"page_{number}_g{GENERATION_A.replace('-', '')}",
            f"page_{number}.png",
        )
        for number in (4, 5)
    ]
    assert storage.objects == {}
    assert storage.uploaded == []
    assert storage.deleted == []

    plan = await processor.prepare_document(
        b"pdf", "document-a", "dataset-a", "collection-a",
        generation_id=GENERATION_A, tenant_id="tenant-a",
        storage_service=storage, page_offset=3,
    )
    assert [receipt["storage_key"] for receipt in plan.object_manifest] == planned


@pytest.mark.asyncio
async def test_vision_candidate_uses_exact_pinned_page_texts_without_ocr() -> None:
    storage = _ObjectStorage()
    processor = _vision_processor()

    async def forbidden_ocr(_image: bytes) -> str:
        raise AssertionError("historical page text was re-OCRed")

    plan = await processor.prepare_document(
        b"pdf", "document-a", "dataset-a", "collection-a",
        generation_id=GENERATION_A, tenant_id="tenant-a",
        storage_service=storage, text_extractor=forbidden_ocr,
        pinned_page_texts={1: "historical alpha", 2: "historical beta"},
    )
    assert plan.content == "[Page 1]\nhistorical alpha\n\n[Page 2]\nhistorical beta"
    assert [row["text"] for row in plan.segment_rows] == [
        "[Page 1]\nhistorical alpha", "[Page 2]\nhistorical beta",
    ]
    assert [point.payload["text"] for point in plan.points_by_collection["collection-a"]] == [
        row["text"] for row in plan.segment_rows
    ]


@pytest.mark.asyncio
async def test_vision_candidate_rejects_incomplete_pinned_pages_before_upload() -> None:
    storage = _ObjectStorage()
    with pytest.raises(ValueError, match="cover exactly"):
        await _vision_processor().prepare_document(
            b"pdf", "document-a", "dataset-a", "collection-a",
            generation_id=GENERATION_A, tenant_id="tenant-a",
            storage_service=storage, pinned_page_texts={1: "only one"},
        )
    assert storage.objects == {}


@pytest.mark.asyncio
async def test_vision_candidate_upload_failure_preserves_prior_objects() -> None:
    storage = _ObjectStorage()
    processor = _vision_processor()
    serving = await processor.prepare_document(
        b"pdf", "document-a", "dataset-a", "collection-a",
        generation_id=GENERATION_A, tenant_id="tenant-a", storage_service=storage,
    )
    storage.fail_page = 2

    with pytest.raises(RuntimeError, match="injected image upload failure"):
        await processor.prepare_document(
            b"pdf", "document-a", "dataset-a", "collection-a",
            generation_id=GENERATION_B, tenant_id="tenant-a", storage_service=storage,
        )

    assert {
        receipt["storage_key"] for receipt in serving.object_manifest
    } <= set(storage.objects)
    assert storage.deleted == []


@pytest.mark.asyncio
async def test_vision_candidate_replay_rejects_changed_object_without_overwrite() -> None:
    storage = _ObjectStorage()
    processor = _vision_processor()
    first = await processor.prepare_document(
        b"pdf", "document-a", "dataset-a", "collection-a",
        generation_id=GENERATION_A, tenant_id="tenant-a", storage_service=storage,
    )
    key = first.object_manifest[0]["storage_key"]
    storage.objects[key] = b"changed-bytes"
    before = dict(storage.objects)
    with pytest.raises(RuntimeError, match="generation object differs"):
        await processor.prepare_document(
            b"pdf", "document-a", "dataset-a", "collection-a",
            generation_id=GENERATION_A, tenant_id="tenant-a", storage_service=storage,
        )
    assert storage.objects == before


@pytest.mark.asyncio
async def test_vision_candidate_failed_replay_preserves_durable_objects() -> None:
    storage = _ObjectStorage()
    first = await _vision_processor().prepare_document(
        b"pdf", "document-a", "dataset-a", "collection-a",
        generation_id=GENERATION_A, tenant_id="tenant-a", storage_service=storage,
    )
    before = dict(storage.objects)
    with pytest.raises(RuntimeError, match="embedding is incomplete"):
        await _vision_processor(fail_images=True).prepare_document(
            b"pdf", "document-a", "dataset-a", "collection-a",
            generation_id=GENERATION_A, tenant_id="tenant-a", storage_service=storage,
        )
    assert storage.objects == before
    assert {receipt["storage_key"] for receipt in first.object_manifest} == set(before)
    assert storage.deleted == []


@pytest.mark.asyncio
async def test_vision_candidate_incomplete_embedding_never_returns_a_plan() -> None:
    storage = _ObjectStorage()
    with pytest.raises(RuntimeError, match="embedding is incomplete"):
        await _vision_processor(fail_images=True).prepare_document(
            b"pdf", "document-a", "dataset-a", "collection-a",
            generation_id=GENERATION_A, tenant_id="tenant-a", storage_service=storage,
        )
    assert len(storage.objects) == 2  # Orphans are reclaimed by manifest-aware GC.
