from __future__ import annotations

from contextlib import asynccontextmanager
from copy import deepcopy
from typing import Any

import pytest
from knowledge_service.core.auth.user_resolver import UserContext
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.persistence.document_metadata import MetadataRegistryRevisionConflict
from knowledge_service.services.knowledge.document_metadata import (
    DocumentMetadataManager,
    normalize_metadata_patch,
    validate_registry_fields,
)

USER = UserContext(user_id="editor-a", tenant_id="tenant-a")
REGISTRY = {
    "version": 1,
    "revision": 4,
    "fields": [
        {"name": "author", "label": "Author", "type": "string"},
        {"name": "priority", "label": "Priority", "type": "number"},
        {"name": "published_at", "label": "Published", "type": "datetime"},
    ],
}


class _Transaction:
    async def __aenter__(self):
        return self

    async def __aexit__(self, *_args: Any):
        return False


class _Connection:
    def transaction(self) -> _Transaction:
        return _Transaction()


class _Store:
    def __init__(self, registry: dict[str, Any] | None = None) -> None:
        self.registry = deepcopy(registry or REGISTRY)

    async def get_registry_locked(self, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
        return deepcopy(self.registry)

    async def get_registry(self, _dataset_id: str) -> dict[str, Any]:
        return deepcopy(self.registry)

    async def update_registry(
        self, *, expected_revision: int, fields: list[dict[str, Any]], **_kwargs: Any
    ) -> dict[str, Any]:
        if expected_revision != self.registry["revision"]:
            raise MetadataRegistryRevisionConflict("stale")
        self.registry = {
            "version": 1,
            "revision": expected_revision + 1,
            "fields": deepcopy(fields),
        }
        return deepcopy(self.registry)


class _Database:
    _pool = object()

    def __init__(self, documents: dict[str, dict[str, Any]]) -> None:
        self.documents = documents
        self.connection = _Connection()

    @asynccontextmanager
    async def document_index_update_lease(self, _dataset_id: str, _document_id: str):
        yield self.connection

    async def get_document(self, document_id: str, *, connection: Any = None):
        _ = connection
        document = self.documents.get(document_id)
        return deepcopy(document) if document else None

    async def update_document_fields(
        self, document_id: str, fields: dict[str, Any], *, connection: Any = None
    ) -> None:
        _ = connection
        current = self.documents[document_id]
        system = {
            key: value
            for key, value in current["metadata"].items()
            if key in {"original_file_key", "_document_lifecycle_reindex"}
        }
        current["metadata"] = {**fields["metadata"], **system}


class _KnowledgeService:
    def __init__(self, database: _Database) -> None:
        self.db = database
        self.access_calls: list[str] = []

    async def require_dataset_access(self, _user: UserContext, dataset_id: str, *, required: str):
        assert dataset_id == "dataset-a"
        self.access_calls.append(required)
        return {"dataset_id": dataset_id, "tenant_id": "tenant-a"}


def _manager(documents: dict[str, dict[str, Any]]) -> tuple[DocumentMetadataManager, _Database]:
    database = _Database(documents)
    manager = DocumentMetadataManager(_KnowledgeService(database))
    manager._store = _Store()  # type: ignore[assignment]
    return manager, database


def test_registry_rejects_reserved_duplicate_and_type_changes() -> None:
    with pytest.raises(ValidationFailedError, match="reserved"):
        validate_registry_fields(
            [{"name": "tenant_id", "label": "Tenant", "type": "string"}]
        )
    with pytest.raises(ValidationFailedError, match="duplicated"):
        validate_registry_fields(
            [
                {"name": "author", "label": "A", "type": "string"},
                {"name": "author", "label": "B", "type": "string"},
            ]
        )
    with pytest.raises(ValidationFailedError, match="immutable"):
        validate_registry_fields(
            [{"name": "author", "label": "Author", "type": "number"}],
            previous_fields=REGISTRY["fields"],
        )


def test_metadata_patch_is_typed_bounded_and_timezone_normalized() -> None:
    patch, removed = normalize_metadata_patch(
        REGISTRY,
        {
            "author": "Ada",
            "priority": 3,
            "published_at": "2026-08-29T08:00:00-04:00",
        },
        [],
    )
    assert removed == []
    assert patch == {
        "author": "Ada",
        "priority": 3,
        "published_at": "2026-08-29T12:00:00Z",
    }
    with pytest.raises(ValidationFailedError, match="finite"):
        normalize_metadata_patch(REGISTRY, {"priority": float("nan")}, [])
    with pytest.raises(ValidationFailedError, match="timezone"):
        normalize_metadata_patch(REGISTRY, {"published_at": "2026-08-29T12:00:00"}, [])
    with pytest.raises(ValidationFailedError, match="unknown"):
        normalize_metadata_patch(REGISTRY, {"constructor": "x"}, [])


@pytest.mark.asyncio
async def test_single_patch_preserves_system_and_unknown_user_metadata() -> None:
    manager, database = _manager(
        {
            "doc-a": {
                "document_id": "doc-a",
                "dataset_id": "dataset-a",
                "metadata": {
                    "author": "Old",
                    "legacy_unknown": {"keep": True},
                    "original_file_key": "server/object",
                    "_document_lifecycle_reindex": {"status": "pending"},
                },
            }
        }
    )

    result = await manager.patch_document(
        USER,
        "dataset-a",
        "doc-a",
        metadata_patch={"author": "New"},
        metadata_remove=[],
        metadata_schema_revision=4,
    )

    assert result["metadata"] == {
        "author": "New",
        "legacy_unknown": {"keep": True},
        "original_file_key": "server/object",
        "_document_lifecycle_reindex": {"status": "pending"},
    }
    assert database.documents["doc-a"]["metadata"] == result["metadata"]
    assert manager._ks.access_calls == ["editor"]  # type: ignore[attr-defined]


@pytest.mark.asyncio
async def test_batch_patch_handles_more_than_two_hundred_without_truncation() -> None:
    documents = {
        f"doc-{index:03d}": {
            "document_id": f"doc-{index:03d}",
            "dataset_id": "dataset-a",
            "metadata": {},
        }
        for index in range(250)
    }
    manager, database = _manager(documents)

    result = await manager.patch_documents(
        USER,
        "dataset-a",
        documents,
        metadata_patch={"priority": 7},
        metadata_remove=[],
        metadata_schema_revision=4,
    )

    assert result["success_count"] == 250
    assert result["failed_count"] == 0
    assert all(row["metadata"]["priority"] == 7 for row in database.documents.values())
    assert manager._ks.access_calls == ["editor"]  # type: ignore[attr-defined]


@pytest.mark.asyncio
async def test_registry_read_and_write_enforce_viewer_and_owner_boundaries() -> None:
    manager, _database = _manager({})

    await manager.get_registry(USER, "dataset-a")
    updated = await manager.update_registry(
        USER,
        "dataset-a",
        expected_revision=4,
        fields=REGISTRY["fields"],
    )

    assert updated["revision"] == 5
    assert manager._ks.access_calls == ["viewer", "owner"]  # type: ignore[attr-defined]


@pytest.mark.asyncio
async def test_patch_rejects_stale_registry_before_write() -> None:
    manager, database = _manager(
        {
            "doc-a": {
                "document_id": "doc-a",
                "dataset_id": "dataset-a",
                "metadata": {},
            }
        }
    )
    with pytest.raises(MetadataRegistryRevisionConflict):
        await manager.patch_document(
            USER,
            "dataset-a",
            "doc-a",
            metadata_patch={"author": "Ada"},
            metadata_remove=[],
            metadata_schema_revision=3,
        )
    assert database.documents["doc-a"]["metadata"] == {}
