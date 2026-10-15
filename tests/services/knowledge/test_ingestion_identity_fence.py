from __future__ import annotations

import contextlib
from copy import deepcopy
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.persistence.database import (
    dataset_ingestion_identity,
    make_dataset_index_deletion_fence,
)
from knowledge_service.services.knowledge.document_service import DocumentService
from knowledge_service.services.knowledge.ingestion import ExtractedImage
from knowledge_service.services.knowledge.ingestion_service import (
    IngestionService,
    _ingestion_dataset_identity,
)
from qdrant_client.http import models as qmodels


def _dataset() -> dict:
    return {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 384,
        "embedding_config": {},
        "index_config": {
            "chunking": {"chunk_size": 400},
            "retrieval": {"top_k": 10},
        },
    }


def test_ingestion_identity_ignores_query_tuning_but_tracks_index_policy() -> None:
    baseline = _dataset()
    retrieval_change = deepcopy(baseline)
    retrieval_change["index_config"]["retrieval"]["top_k"] = 50
    chunking_change = deepcopy(baseline)
    chunking_change["index_config"]["chunking"]["chunk_size"] = 800
    embedding_change = deepcopy(baseline)
    embedding_change["embedding_model"] = "same-dimension-incompatible"

    identity = _ingestion_dataset_identity(baseline)
    assert _ingestion_dataset_identity(retrieval_change) == identity
    assert _ingestion_dataset_identity(chunking_change) != identity
    assert _ingestion_dataset_identity(embedding_change) != identity


@pytest.mark.asyncio
async def test_ingestion_identity_fence_rechecks_authoritative_dataset() -> None:
    current = _dataset()

    class Database:
        async def get_dataset(self, _dataset_id: str) -> dict:
            return deepcopy(current)

    service = IngestionService.__new__(IngestionService)
    service.db = Database()
    expected = _ingestion_dataset_identity(current)

    await service._require_ingestion_identity("dataset-a", expected)
    current["index_config"]["chunking"]["chunk_size"] = 999
    with pytest.raises(ValidationFailedError, match="mixed index generation"):
        await service._require_ingestion_identity("dataset-a", expected)


@pytest.mark.asyncio
async def test_ingestion_identity_fence_rejects_deleted_dataset() -> None:
    class Database:
        async def get_dataset(self, _dataset_id: str):
            return None

    service = IngestionService.__new__(IngestionService)
    service.db = Database()

    with pytest.raises(ValidationFailedError, match="mixed index generation"):
        await service._require_ingestion_identity(
            "dataset-a",
            _ingestion_dataset_identity(_dataset()),
        )


@pytest.mark.asyncio
async def test_ingestion_identity_fence_rejects_pending_dataset_deletion() -> None:
    pending = _dataset()
    pending["index_config"]["retrieval"]["_index_deletion_fence"] = (
        make_dataset_index_deletion_fence("dataset_delete", "dataset-a")
    )

    class Database:
        async def get_dataset(self, _dataset_id: str):
            return deepcopy(pending)

    service = IngestionService.__new__(IngestionService)
    service.db = Database()

    with pytest.raises(ValidationFailedError, match="deletion is pending"):
        await service._require_ingestion_identity(
            "dataset-a",
            _ingestion_dataset_identity(pending),
        )


@pytest.mark.asyncio
async def test_active_bm25_v2_kill_switch_does_not_mutate_document_status() -> None:
    active = _dataset()
    active["index_config"]["retrieval"] = {
        "lexical": {
            "active_version": "bm25_v2",
            "bm25_v2": {"shadow_write_enabled": True},
        }
    }
    status_updates: list[tuple[tuple, dict]] = []

    class Database:
        async def get_dataset(self, _dataset_id: str):
            return deepcopy(active)

        async def update_document_status(self, *args, **kwargs) -> None:
            status_updates.append((args, kwargs))

    class Knowledge:
        async def _get_dataset_or_404(self, _dataset_id: str):
            return deepcopy(active)

    service = IngestionService.__new__(IngestionService)
    service.db = Database()
    service._ks = Knowledge()
    service.vector_store = SimpleNamespace(bm25_v2_enabled=False)

    await service.ingest_document("dataset-a", "document-a")

    assert status_updates == []


@pytest.mark.asyncio
async def test_ingestion_upsert_passes_identity_to_central_vector_fence() -> None:
    expected = _ingestion_dataset_identity(_dataset())

    class Database:
        @contextlib.asynccontextmanager
        async def dataset_index_write_lease(
            self, *_args, **_kwargs
        ):
            raise AssertionError("the central VectorStore owns the lifecycle lease")
            yield  # pragma: no cover

    class Store:
        kwargs: dict | None = None

        async def upsert(self, **kwargs) -> None:
            self.kwargs = kwargs

    store = Store()
    service = IngestionService.__new__(IngestionService)
    service.db = Database()
    service.vector_store = store
    await service._upsert_with_ingestion_identity(
        collection="collection-a",
        points=[
            qmodels.PointStruct(
                id="segment-a",
                vector=[0.1, 0.2],
                payload={
                    "dataset_id": "dataset-a",
                    "document_id": "document-a",
                },
            )
        ],
        dataset_id="dataset-a",
        expected_ingestion_identity=expected,
    )

    assert store.kwargs is not None
    assert store.kwargs["expected_ingestion_identity"] == expected


@pytest.mark.asyncio
async def test_direct_upload_image_write_uses_captured_identity_lease() -> None:
    dataset = _dataset()
    expected = _ingestion_dataset_identity(dataset)
    events: list[object] = []

    class Database:
        async def get_dataset(self, _dataset_id: str):
            return deepcopy(dataset)

        @contextlib.asynccontextmanager
        async def dataset_index_write_lease(
            self,
            dataset_id: str,
            document_ids: list[str],
            *,
            expected_ingestion_identity: str,
        ):
            events.append(
                (
                    "lease",
                    dataset_id,
                    document_ids,
                    expected_ingestion_identity,
                )
            )
            yield

        async def update_document_status(self, *_args, **_kwargs) -> None:
            return None

        async def save_image_segment(self, _segment: dict) -> None:
            return None

    class Store:
        async def upsert(self, **kwargs) -> None:
            events.append(("qdrant", kwargs.get("expected_ingestion_identity")))

    class Embedder:
        async def embed_images(self, _images: list[bytes]):
            return [[0.1, 0.2]]

    service = IngestionService.__new__(IngestionService)
    service.db = Database()
    service.vector_store = Store()
    service._ks = SimpleNamespace()

    count, metadata = await service._embed_images_in_memory(
        embedder=Embedder(),
        dataset_id="dataset-a",
        document_id="document-a",
        images=[
            ExtractedImage(
                image_id="image-a",
                content=b"image",
                mime_type="image/png",
                width=100,
                height=100,
                source_location="page-1",
            )
        ],
        collection="collection-a",
        tenant_id="tenant-a",
    )

    assert count == 1
    assert metadata[0]["image_id"] == "image-a"
    assert events == [
        ("qdrant", expected),
    ]


@pytest.mark.asyncio
async def test_image_receipt_compensates_qdrant_when_segment_db_write_fails() -> None:
    dataset = _dataset()
    expected = _ingestion_dataset_identity(dataset)
    events: list[tuple] = []

    class Database:
        async def get_dataset(self, _dataset_id: str):
            return deepcopy(dataset)

        @contextlib.asynccontextmanager
        async def dataset_index_write_lease(self, *_args, **_kwargs):
            raise AssertionError("the central VectorStore owns the lifecycle lease")
            yield  # pragma: no cover

        async def update_document_status(self, *_args, **_kwargs) -> None:
            return None

        async def save_image_segment(self, segment: dict) -> None:
            events.append(("db-save", segment["segment_id"]))
            raise RuntimeError("postgres write failed")

        async def delete_segment(self, segment_id: str) -> bool:
            events.append(("db-delete", segment_id))
            return False

    class Store:
        async def upsert(self, **kwargs) -> None:
            points = kwargs["points"]
            events.append(
                (
                    "qdrant-upsert",
                    [str(point.id) for point in points],
                    kwargs.get("expected_ingestion_identity"),
                )
            )

        async def delete_points(
            self,
            collection: str,
            point_ids: list[str],
            *,
            tenant_id: str,
            dataset_id: str,
            affects_bm25_scope: bool = True,
        ) -> None:
            assert affects_bm25_scope is False
            events.append(
                (
                    "qdrant-delete",
                    collection,
                    list(point_ids),
                    tenant_id,
                    dataset_id,
                )
            )

    class Embedder:
        async def embed_images(self, _images: list[bytes]):
            return [[0.1, 0.2]]

    service = IngestionService.__new__(IngestionService)
    service.db = Database()
    service.vector_store = Store()
    service._ks = SimpleNamespace()

    with pytest.raises(RuntimeError, match="new image generation was rejected"):
        await service._embed_images_in_memory(
            embedder=Embedder(),
            dataset_id="dataset-a",
            document_id="document-a",
            images=[
                ExtractedImage(
                    image_id="image-a",
                    content=b"image",
                    mime_type="image/png",
                    width=100,
                    height=100,
                    source_location="page-1",
                )
            ],
            collection="collection-a",
            tenant_id="tenant-a",
        )

    upsert = next(event for event in events if event[0] == "qdrant-upsert")
    qdrant_delete = next(event for event in events if event[0] == "qdrant-delete")
    db_delete = next(event for event in events if event[0] == "db-delete")
    assert upsert[2] == expected
    assert qdrant_delete == (
        "qdrant-delete",
        "collection-a",
        upsert[1],
        "tenant-a",
        "dataset-a",
    )
    assert db_delete == ("db-delete", upsert[1][0])
    assert [event[0] for event in events] == [
        "qdrant-upsert",
        "db-save",
        "qdrant-delete",
        "db-delete",
    ]


@pytest.mark.asyncio
async def test_image_rebuild_rejects_incomplete_durable_receipt_before_qdrant() -> None:
    dataset = _dataset()

    class Database:
        async def get_dataset(self, _dataset_id: str):
            return deepcopy(dataset)

    class Store:
        async def upsert(self, **_kwargs) -> None:
            raise AssertionError("Qdrant must not be touched for an incomplete receipt")

    class Embedder:
        async def embed_images(self, _images: list[bytes]):
            raise AssertionError("embedding must not start without durable image bytes")

    service = IngestionService.__new__(IngestionService)
    service.db = Database()
    service.vector_store = Store()
    service._ks = SimpleNamespace(
        image_storage_service=SimpleNamespace(_backend=SimpleNamespace())
    )

    with pytest.raises(RuntimeError, match="durable image receipt"):
        await service._process_document_images_with_embedder(
            embedder=Embedder(),
            dataset_id="dataset-a",
            document_id="document-a",
            image_metadata_list=[
                {
                    "image_id": "image-without-storage",
                    "mime_type": "image/png",
                }
            ],
            collection="collection-a",
            tenant_id="tenant-a",
        )


@pytest.mark.asyncio
async def test_image_rebuild_preserves_confluence_source_receipt_fields() -> None:
    dataset = _dataset()
    saved_segments: list[dict] = []
    upserted_points: list[qmodels.PointStruct] = []

    class Database:
        async def get_dataset(self, _dataset_id: str):
            return deepcopy(dataset)

        @contextlib.asynccontextmanager
        async def dataset_index_write_lease(self, *_args, **_kwargs):
            raise AssertionError("the central VectorStore owns the lifecycle lease")
            yield  # pragma: no cover

        async def update_document_status(self, *_args, **_kwargs) -> None:
            return None

        async def save_image_segment(self, segment: dict) -> None:
            saved_segments.append(deepcopy(segment))

    class Store:
        async def upsert(self, **kwargs) -> None:
            upserted_points.extend(kwargs["points"])

    class Storage:
        async def download_document_image(
            self,
            *,
            tenant_id: str,
            document_id: str,
            storage_key: str,
        ) -> bytes:
            assert (tenant_id, document_id) == ("tenant-a", "document-a")
            assert storage_key == "immutable/source/image-a.png"
            return b"image-bytes"

    class Embedder:
        async def embed_images(self, images: list[bytes]):
            assert images == [b"image-bytes"]
            return [[0.1, 0.2]]

    service = IngestionService.__new__(IngestionService)
    service.db = Database()
    service.vector_store = Store()
    service._ks = SimpleNamespace(
        image_storage_service=Storage()
    )

    count = await service._process_document_images_with_embedder(
        embedder=Embedder(),
        dataset_id="dataset-a",
        document_id="document-a",
        image_metadata_list=[
            {
                "image_id": "image-a",
                "storage_url": "https://storage.invalid/image-a.png",
                "storage_key": "immutable/source/image-a.png",
                "filename": "diagram.png",
                "mime_type": "image/png",
                "size_bytes": 123,
                "context_text": "diagram context",
                "vlm_description": "architecture diagram",
                "source_location": "attachment",
                "confluence_attachment_id": "attachment-a",
                "attachment_updated_at": "2026-08-02T12:00:00Z",
            }
        ],
        collection="collection-a",
        tenant_id="tenant-a",
    )

    assert count == 1
    assert len(saved_segments) == 1
    assert len(upserted_points) == 1
    segment = saved_segments[0]
    assert segment["image_attachment_id"] == "attachment-a"
    assert segment["image_filename"] == "diagram.png"
    assert segment["text"] == "architecture diagram"
    assert segment["metadata"]["storage_key"] == "immutable/source/image-a.png"
    assert segment["metadata"]["confluence_attachment_id"] == "attachment-a"
    assert (
        segment["metadata"]["attachment_updated_at"]
        == "2026-08-02T12:00:00Z"
    )
    payload = upserted_points[0].payload or {}
    assert payload["confluence_attachment_id"] == "attachment-a"
    assert payload["attachment_updated_at"] == "2026-08-02T12:00:00Z"


@pytest.mark.asyncio
async def test_reextracted_image_upload_failure_compensates_and_retry_publishes_only_complete_receipt() -> None:
    class Storage:
        def __init__(self) -> None:
            self.fail_second = True
            self.objects: dict[str, bytes] = {}
            self.deleted: list[str] = []

        @staticmethod
        def _generate_key(
            tenant_id: str,
            document_id: str,
            attachment_id: str,
            filename: str,
        ) -> str:
            return (
                f"knowledge/confluence/{tenant_id}/{document_id}/images/"
                f"{attachment_id}_{filename}"
            )

        async def upload_image(self, **kwargs: Any) -> str:
            key = self._generate_key(
                kwargs["tenant_id"],
                kwargs["document_id"],
                kwargs["attachment_id"],
                kwargs["filename"],
            )
            if self.fail_second and kwargs["attachment_id"] == "reindex_image-b":
                raise RuntimeError("transient object storage failure")
            self.objects[key] = kwargs["content"]
            return f"https://storage.invalid/{key}"

        async def delete_image(self, **kwargs: Any) -> bool:
            key = self._generate_key(
                kwargs["tenant_id"],
                kwargs["document_id"],
                kwargs["attachment_id"],
                kwargs["filename"],
            )
            self.deleted.append(key)
            self.objects.pop(key, None)
            return True

    class Database:
        def __init__(self) -> None:
            self.receipts: list[list[dict[str, Any]]] = []

        async def publish_document_image_receipt(self, *args: Any, **kwargs: Any) -> bool:
            assert args == ("document-a", "dataset-a")
            assert kwargs["expected_original_file_key"] == "original/key.pdf"
            assert kwargs["expected_processing_mode"] == "multimodal"
            receipt = deepcopy(kwargs["extracted_images"])
            assert all(image.get("storage_url") for image in receipt)
            assert all(image.get("storage_key") for image in receipt)
            self.receipts.append(receipt)
            return True

    images = [
        ExtractedImage(
            image_id="image-a",
            content=b"image-a",
            mime_type="image/png",
            width=10,
            height=10,
            source_location="page-1",
            filename="a.png",
        ),
        ExtractedImage(
            image_id="image-b",
            content=b"image-b",
            mime_type="image/png",
            width=20,
            height=20,
            source_location="page-2",
            filename="b.png",
        ),
    ]
    storage = Storage()
    database = Database()
    service = IngestionService.__new__(IngestionService)
    service.db = database
    service._ks = SimpleNamespace(image_storage_service=storage)
    original_metadata = {
        "processing_mode": "multimodal",
        "original_file_key": "original/key.pdf",
    }

    with pytest.raises(RuntimeError, match="could not be stored durably"):
        await service._persist_reextracted_image_receipt(
            dataset_id="dataset-a",
            tenant_id="tenant-a",
            document_id="document-a",
            processing_mode="multimodal",
            doc_metadata=original_metadata,
            images=images,
        )

    assert storage.objects == {}
    assert database.receipts == []
    assert "extracted_images" not in original_metadata

    storage.fail_second = False
    published = await service._persist_reextracted_image_receipt(
        dataset_id="dataset-a",
        tenant_id="tenant-a",
        document_id="document-a",
        processing_mode="multimodal",
        doc_metadata=original_metadata,
        images=images,
    )

    assert len(database.receipts) == 1
    assert len(database.receipts[0]) == 2
    assert published["image_count"] == 2
    assert published["extracted_images"] == database.receipts[0]
    assert set(storage.objects) == {
        image["storage_key"] for image in database.receipts[0]
    }


@pytest.mark.asyncio
async def test_image_receipt_original_key_cas_rejection_compensates_uploaded_objects() -> None:
    objects: set[str] = set()

    class Storage:
        @staticmethod
        def _generate_key(
            tenant_id: str,
            document_id: str,
            attachment_id: str,
            filename: str,
        ) -> str:
            return f"knowledge/confluence/{tenant_id}/{document_id}/images/{attachment_id}_{filename}"

        async def upload_image(self, **kwargs: Any) -> str:
            key = self._generate_key(
                kwargs["tenant_id"],
                kwargs["document_id"],
                kwargs["attachment_id"],
                kwargs["filename"],
            )
            objects.add(key)
            return f"https://storage.invalid/{key}"

        async def delete_image(self, **kwargs: Any) -> bool:
            key = self._generate_key(
                kwargs["tenant_id"],
                kwargs["document_id"],
                kwargs["attachment_id"],
                kwargs["filename"],
            )
            objects.discard(key)
            return True

    class Database:
        async def publish_document_image_receipt(self, *args: Any, **kwargs: Any) -> bool:
            assert args == ("document-a", "dataset-a")
            assert kwargs["expected_original_file_key"] == "stale/original.pdf"
            return False

    service = IngestionService.__new__(IngestionService)
    service.db = Database()
    service._ks = SimpleNamespace(image_storage_service=Storage())

    with pytest.raises(RuntimeError, match="lost document generation authority"):
        await service._persist_reextracted_image_receipt(
            dataset_id="dataset-a",
            tenant_id="tenant-a",
            document_id="document-a",
            processing_mode="multimodal",
            doc_metadata={
                "processing_mode": "multimodal",
                "original_file_key": "stale/original.pdf",
            },
            images=[
                ExtractedImage(
                    image_id="image-a",
                    content=b"image-a",
                    mime_type="image/png",
                    width=10,
                    height=10,
                    source_location="page-1",
                    filename="a.png",
                )
            ],
        )

    assert objects == set()


@pytest.mark.asyncio
async def test_image_receipt_compensation_rejects_false_storage_delete() -> None:
    class Storage:
        @staticmethod
        def _generate_key(
            tenant_id: str,
            document_id: str,
            attachment_id: str,
            filename: str,
        ) -> str:
            return f"knowledge/confluence/{tenant_id}/{document_id}/images/{attachment_id}_{filename}"

        async def upload_image(self, **kwargs: Any) -> str:
            key = self._generate_key(
                kwargs["tenant_id"],
                kwargs["document_id"],
                kwargs["attachment_id"],
                kwargs["filename"],
            )
            return f"https://storage.invalid/{key}"

        async def delete_image(self, **_kwargs: Any) -> bool:
            return False

    class Database:
        async def publish_document_image_receipt(self, *_args: Any, **_kwargs: Any) -> bool:
            return False

    service = IngestionService.__new__(IngestionService)
    service.db = Database()
    service._ks = SimpleNamespace(image_storage_service=Storage())

    with pytest.raises(RuntimeError, match="compensation was incomplete"):
        await service._persist_reextracted_image_receipt(
            dataset_id="dataset-a",
            tenant_id="tenant-a",
            document_id="document-a",
            processing_mode="multimodal",
            doc_metadata={
                "processing_mode": "multimodal",
                "original_file_key": "original/key.pdf",
            },
            images=[
                ExtractedImage(
                    image_id="image-a",
                    content=b"image-a",
                    mime_type="image/png",
                    width=10,
                    height=10,
                    source_location="page-1",
                    filename="a.png",
                )
            ],
        )


@pytest.mark.asyncio
@pytest.mark.parametrize("source_type", ["upload", "confluence"])
@pytest.mark.parametrize(
    ("dataset_multimodal", "document_text", "expected_status", "expected_images"),
    [
        (True, "", "completed", 1),
        (False, "", "error", 0),
        (False, "searchable text " * 30, "completed", 0),
    ],
)
async def test_image_receipt_requires_unified_space_but_preserves_text_fallback(
    source_type: str,
    dataset_multimodal: bool,
    document_text: str,
    expected_status: str,
    expected_images: int,
) -> None:
    dataset = _dataset()
    dataset["embedding_dimension"] = 2
    receipt = {
        "image_id": "image-a",
        "storage_url": "https://storage.invalid/image-a.png",
        "storage_key": (
            "knowledge/confluence/tenant-a/document-a/images/image-a.png"
        ),
        "filename": "image-a.png",
        "mime_type": "image/png",
        "size_bytes": 7,
        "context_text": "only image",
        "source_location": "page-1",
    }
    metadata: dict[str, Any] = {
        "processing_mode": "multimodal",
        "extracted_images": [receipt],
        "image_count": 1,
    }
    if source_type == "upload":
        metadata.update(
            {
                "original_file_key": (
                    "knowledge/documents/tenant-a/document-a/original/image.png"
                ),
                "original_filename": "image.png",
                "original_mime_type": "image/png",
            }
        )
    document = {
        "document_id": "document-a",
        "dataset_id": "dataset-a",
        "source_type": source_type,
        "title": "image.png",
        "mime_type": "image/png",
        "content": document_text,
        "metadata": metadata,
    }
    status_updates: list[str] = []
    image_segments: list[dict[str, Any]] = []
    text_segments: list[dict[str, Any]] = []

    class Database:
        @contextlib.asynccontextmanager
        async def dataset_index_write_lease(self, *_args: Any, **_kwargs: Any):
            yield

        @contextlib.asynccontextmanager
        async def dataset_index_publication_lease(
            self, *_args: Any, **_kwargs: Any
        ):
            yield SimpleNamespace(
                connection=self,
                revision=-100_000,
                recovered=False,
            )

        async def abort_index_publication(self, *_args: Any, **_kwargs: Any) -> None:
            return None

        async def get_dataset(self, _dataset_id: str) -> dict[str, Any]:
            return deepcopy(dataset)

        async def get_document(self, _document_id: str) -> dict[str, Any]:
            return deepcopy(document)

        async def update_document_status(
            self,
            _document_id: str,
            *,
            status: str,
            **_kwargs: Any,
        ) -> None:
            status_updates.append(status)

        async def update_document_fields(
            self,
            _document_id: str,
            fields: dict[str, Any],
            *,
            allow_lifecycle_marker_update: bool = False,
        ) -> None:
            assert allow_lifecycle_marker_update is True
            document["metadata"] = deepcopy(fields["metadata"])

        async def get_segment_hashes_by_document(
            self,
            _document_id: str,
            *,
            content_type: str,
        ) -> dict[int, dict[str, Any]]:
            assert content_type == "text"
            return {}

        async def get_image_segments_by_document(
            self,
            _document_id: str,
        ) -> list[dict[str, Any]]:
            return deepcopy(image_segments)

        async def save_image_segment(self, segment: dict[str, Any]) -> None:
            image_segments.append(deepcopy(segment))

        async def insert_segments(self, segments: list[dict[str, Any]]) -> None:
            text_segments.extend(deepcopy(segments))

        async def activate_staged_segments(
            self,
            _document_id: str,
            segment_ids: list[str],
            **_kwargs: Any,
        ) -> int:
            # T1 staging flip: this fake models every staged row as promoted.
            return len([sid for sid in segment_ids if sid])

        async def commit_text_segment_publication(
            self,
            *,
            segment_rows: list[dict[str, Any]],
            staged_segment_ids: list[str],
            **_kwargs: Any,
        ) -> tuple[int, int]:
            await self.insert_segments(segment_rows)
            promoted = await self.activate_staged_segments(
                "document-a",
                staged_segment_ids,
            )
            return promoted, 0

        async def refresh_document_segment_count(self, _document_id: str) -> int:
            return len(image_segments)

        async def clear_dataset_needs_reindex(self, _dataset_id: str) -> None:
            return None

    class Store:
        def __init__(self) -> None:
            self.points: list[qmodels.PointStruct] = []

        async def ensure_collection(self, **_kwargs: Any) -> str:
            return "collection-a"

        async def upsert(self, **kwargs: Any) -> None:
            self.points.extend(kwargs["points"])

        async def snapshot_points(
            self, _collection: str, point_ids: list[str], **_kwargs: Any
        ) -> dict[str, qmodels.PointStruct]:
            wanted = set(point_ids)
            return {
                str(point.id): deepcopy(point)
                for point in self.points
                if str(point.id) in wanted
            }

        async def delete_points(
            self, _collection: str, point_ids: list[str], **_kwargs: Any
        ) -> None:
            removed = set(point_ids)
            self.points = [
                point for point in self.points if str(point.id) not in removed
            ]

    class Storage:
        async def download_original_file(self, _key: str) -> bytes:
            return b"original-image"

        async def download_document_image(
            self,
            *,
            tenant_id: str,
            document_id: str,
            storage_key: str,
        ) -> bytes:
            assert (tenant_id, document_id) == ("tenant-a", "document-a")
            assert storage_key == receipt["storage_key"]
            return b"image-a"

    class Embedder:
        _dimension = 2
        dimension = 2
        supports_multimodal = True

        async def embed_images(self, images: list[bytes]) -> list[list[float]]:
            assert images == [b"image-a"]
            return [[0.1, 0.2]]

        async def embed_documents(self, texts: list[str]) -> list[list[float]]:
            return [[0.1, 0.2] for _ in texts]

        async def close(self) -> None:
            return None

    embedder = Embedder()
    storage = Storage()
    store = Store()
    database = Database()
    knowledge = SimpleNamespace(
        image_storage_service=storage,
        multimodal_embedding=embedder,
        document_image_extractor=SimpleNamespace(
            extract=AsyncMock(
                return_value=SimpleNamespace(text="", embeddable_images=[])
            )
        ),
        _get_dataset_or_404=AsyncMock(return_value=deepcopy(dataset)),
        _is_multimodal_dataset=lambda _dataset: dataset_multimodal,
        _get_unified_multimodal_embedder=lambda _dataset, _config: embedder,
        _get_text_embedder=lambda _dataset, _config: embedder,
        associate_images_to_chunks=AsyncMock(
            return_value={"associations_created": 0, "segments_with_images": 0}
        ),
        _ocr_pdf_bytes=lambda _content: "",
    )
    service = IngestionService.__new__(IngestionService)
    service.settings = SimpleNamespace(
        knowledge=SimpleNamespace(
            pdf_min_text_chars_for_ocr=200,
            scanned_min_images_for_image_only=5,
            ocr_enabled=True,
            text_embedding_batch_size=8,
            text_embedding_max_concurrent=1,
            multimodal_embedding_max_concurrent=1,
        )
    )
    service.db = database
    service.vector_store = store
    service._ks = knowledge

    await service.ingest_document("dataset-a", "document-a")

    assert status_updates[-1] == expected_status
    assert len(image_segments) == expected_images
    image_points = [
        point
        for point in store.points
        if (point.payload or {}).get("content_type") == "image"
    ]
    assert len(image_points) == expected_images
    if document_text and not dataset_multimodal:
        assert text_segments
        assert document["metadata"]["image_indexing"] == {
            "status": "skipped",
            "reason": "text_only_dataset",
            "receipt_count": 1,
            "indexed_count": 0,
        }
    elif not dataset_multimodal:
        assert text_segments == []
        assert store.points == []


@pytest.mark.asyncio
async def test_document_create_passes_observed_identity_to_atomic_insert() -> None:
    dataset = _dataset()
    captured: dict[str, object] = {}

    class Database:
        async def insert_document(
            self,
            document: dict,
            *,
            expected_ingestion_identity: str,
        ) -> None:
            captured["document"] = document
            captured["identity"] = expected_ingestion_identity

        async def get_document(self, _document_id: str):
            return None

    class Knowledge:
        async def require_dataset_access(self, *_args, **_kwargs):
            return deepcopy(dataset)

        @staticmethod
        def _sanitize_text_for_db(value: str) -> str:
            return value

    service = DocumentService.__new__(DocumentService)
    service.db = Database()
    service._ks = Knowledge()

    created = await service.create_document_from_text(
        SimpleNamespace(),
        "dataset-a",
        "Title",
        "Body",
    )

    assert created["dataset_id"] == "dataset-a"
    assert captured["identity"] == dataset_ingestion_identity(dataset)
