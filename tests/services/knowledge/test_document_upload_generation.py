from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from copy import deepcopy
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.core.auth.user_resolver import UserContext
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.persistence.database import DOCUMENT_UPLOAD_GENERATION_KEY
from knowledge_service.services.knowledge.document_service import DocumentService

DATASET = {
    "dataset_id": "dataset-a",
    "tenant_id": "tenant-a",
    "collection_name": "collection-a",
    "embedding_provider": "local",
    "embedding_model": "hash-384",
    "embedding_dimension": 384,
    "embedding_config": {},
    "index_config": {},
}


class UploadDatabase:
    def __init__(self) -> None:
        self.document: dict[str, Any] | None = None
        self.events: list[str] = []
        self.owner_lock = asyncio.Lock()
        self.finalize_result = True
        self.hide_before_lease_read = False

    async def insert_document(
        self,
        document: dict[str, Any],
        *,
        expected_ingestion_identity: str | None = None,
    ) -> None:
        assert expected_ingestion_identity
        assert self.document is None
        self.document = deepcopy(document)
        self.events.append("insert")

    @asynccontextmanager
    async def document_index_update_lease(self, dataset_id: str, document_id: str):
        assert dataset_id == "dataset-a"
        assert document_id
        async with self.owner_lock:
            self.events.append("lease-enter")
            try:
                yield self
            finally:
                self.events.append("lease-exit")

    async def get_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        del document_id
        if connection is not None:
            assert connection is self
            assert self.owner_lock.locked()
        if self.hide_before_lease_read and connection is not None:
            return None
        return deepcopy(self.document)

    async def finalize_document_upload(
        self,
        document: dict[str, Any],
        *,
        upload_generation: str,
        expected_ingestion_identity: str | None = None,
        connection: Any | None = None,
    ) -> bool:
        assert expected_ingestion_identity
        assert connection is self
        assert self.owner_lock.locked()
        self.events.append("finalize")
        if not self.finalize_result or self.document is None:
            return False
        current_metadata = dict(self.document.get("metadata") or {})
        if current_metadata.get(DOCUMENT_UPLOAD_GENERATION_KEY) != upload_generation:
            return False
        persisted = deepcopy(document)
        persisted_metadata = dict(persisted.get("metadata") or {})
        persisted_metadata.pop(DOCUMENT_UPLOAD_GENERATION_KEY, None)
        persisted["metadata"] = persisted_metadata
        self.document = persisted
        return True


class UploadStorage:
    def __init__(self, database: UploadDatabase) -> None:
        self.database = database
        self.events: list[str] = []
        self.written = asyncio.Event()
        self.block_after_write = False

    async def upload_original_file(self, **kwargs: Any) -> str:
        assert kwargs["tenant_id"] == "tenant-a"
        assert self.database.owner_lock.locked()
        self.events.append("upload")
        self.written.set()
        if self.block_after_write:
            await asyncio.Event().wait()
        return (
            f"knowledge/documents/tenant-a/{kwargs['document_id']}/original/"
            f"{kwargs['filename']}"
        )

    async def delete_document_assets(self, **kwargs: Any) -> int:
        assert kwargs["tenant_id"] == "tenant-a"
        self.events.append(f"cleanup:{kwargs['document_id']}")
        return 1


class UploadKnowledgeService:
    def __init__(self, storage: UploadStorage) -> None:
        self.image_storage_service = storage
        self.vector_store = SimpleNamespace(
            upsert=lambda *_args, **_kwargs: (_ for _ in ()).throw(
                AssertionError("upload request path must not write vectors")
            )
        )

    async def require_dataset_access(
        self,
        _user: UserContext,
        dataset_id: str,
        *,
        required: str,
    ) -> dict[str, Any]:
        assert dataset_id == "dataset-a"
        assert required == "editor"
        return deepcopy(DATASET)


def make_service() -> tuple[DocumentService, UploadDatabase, UploadStorage]:
    database = UploadDatabase()
    storage = UploadStorage(database)
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = UploadKnowledgeService(storage)  # type: ignore[assignment]
    return service, database, storage


@pytest.mark.asyncio
@pytest.mark.parametrize("processing_mode", ["text_only", "scanned", "multimodal", "auto"])
async def test_upload_request_path_only_persists_original_and_finalizes_generation(
    processing_mode: str,
) -> None:
    service, database, storage = make_service()

    result = await service.create_document_from_upload(
        UserContext(user_id="editor-a", tenant_id="tenant-a"),
        "dataset-a",
        "document.pdf",
        b"raw-pdf",
        mime_type="application/pdf",
        metadata={"user_key": "value"},
        processing_mode=processing_mode,
    )

    assert result["status"] == "waiting"
    assert result["content"] == ""
    assert result["metadata"]["processing_mode"] == processing_mode
    assert result["metadata"]["user_key"] == "value"
    assert result["metadata"]["original_file_key"].startswith("knowledge/documents/")
    assert DOCUMENT_UPLOAD_GENERATION_KEY not in result["metadata"]
    assert database.events == ["insert", "lease-enter", "finalize", "lease-exit"]
    assert storage.events == ["upload"]


@pytest.mark.asyncio
async def test_upload_cas_failure_cleans_storage_without_resurrecting_row() -> None:
    service, database, storage = make_service()
    database.finalize_result = False

    with pytest.raises(ValidationFailedError, match="deleted or superseded"):
        await service.create_document_from_upload(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "document.pdf",
            b"raw-pdf",
        )

    assert database.document is not None
    assert database.document["status"] == "uploading"
    assert DOCUMENT_UPLOAD_GENERATION_KEY in database.document["metadata"]
    assert storage.events[0] == "upload"
    assert storage.events[1].startswith("cleanup:")


@pytest.mark.asyncio
async def test_deleted_generation_before_storage_write_is_never_recreated() -> None:
    service, database, storage = make_service()
    database.hide_before_lease_read = True

    with pytest.raises(ValidationFailedError, match="lost ownership"):
        await service.create_document_from_upload(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "document.pdf",
            b"raw-pdf",
        )

    assert "upload" not in storage.events
    assert "finalize" not in database.events
    assert storage.events and storage.events[0].startswith("cleanup:")


@pytest.mark.asyncio
async def test_upload_cancellation_after_storage_write_runs_shielded_cleanup() -> None:
    service, database, storage = make_service()
    storage.block_after_write = True
    task = asyncio.create_task(
        service.create_document_from_upload(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "document.pdf",
            b"raw-pdf",
        )
    )

    await asyncio.wait_for(storage.written.wait(), timeout=1)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task

    assert database.document is not None
    assert database.document["status"] == "uploading"
    assert "finalize" not in database.events
    assert any(event.startswith("cleanup:") for event in storage.events)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "reserved_key",
    [
        "original_file_key",
        "extracted_images",
        "processing_mode",
        "_confluence_attachment_manifest",
    ],
)
async def test_upload_rejects_public_source_receipt_metadata(reserved_key: str) -> None:
    service, database, storage = make_service()

    with pytest.raises(ValidationFailedError, match="reserved"):
        await service.create_document_from_upload(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "document.pdf",
            b"raw-pdf",
            metadata={reserved_key: "attacker-controlled"},
        )

    assert database.document is None
    assert storage.events == []
