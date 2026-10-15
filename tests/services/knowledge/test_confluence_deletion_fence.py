from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.core.auth.user_resolver import UserContext
from knowledge_service.persistence.database import make_dataset_index_deletion_fence
from knowledge_service.services.knowledge.confluence.sync_service import (
    ConfluenceSyncError,
    ConfluenceSyncService,
)


def _service(database: Any, knowledge_service: Any) -> ConfluenceSyncService:
    return ConfluenceSyncService(
        settings=SimpleNamespace(
            confluence=SimpleNamespace(client_cache_ttl_seconds=300)
        ),
        database=database,
        knowledge_service=knowledge_service,
    )


@pytest.mark.asyncio
async def test_background_delete_routes_through_fenced_knowledge_service() -> None:
    calls: list[tuple[Any, str, str]] = []

    class KnowledgeService:
        async def detach_confluence_source(self, user: Any, binding: dict, page: dict) -> bool:
            calls.append((user, binding["dataset_id"], page["document_id"]))
            return True

    service = _service(SimpleNamespace(), KnowledgeService())
    await service._delete_bound_document(
        {
            "binding_id": "binding-a", "connection_id": "connection-a",
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
            "created_by": "user-a",
        },
        "document-a",
        page={"id": "page-a", "binding_id": "binding-a", "page_id": "page-a", "document_id": "document-a"},
    )

    user, dataset_id, document_id = calls[0]
    assert user.is_authenticated is True
    assert user.user_id == "user-a"
    assert user.tenant_id == "tenant-a"
    assert dataset_id == "dataset-a"
    assert document_id == "document-a"


@pytest.mark.asyncio
async def test_background_delete_fails_closed_without_binding_identity() -> None:
    class KnowledgeService:
        async def delete_document(self, *_args: Any, **_kwargs: Any) -> bool:
            raise AssertionError("incomplete binding must not reach document deletion")

    service = _service(SimpleNamespace(), KnowledgeService())
    with pytest.raises(ConfluenceSyncError, match="created_by"):
        await service._delete_bound_document(
            {"dataset_id": "dataset-a", "tenant_id": "tenant-a"},
            "document-a",
        )


@pytest.mark.asyncio
async def test_binding_delete_keeps_binding_when_document_cleanup_fails() -> None:
    events: list[str] = []

    class Database:
        async def get_confluence_binding(self, _binding_id: str) -> dict[str, Any]:
            return {
                "dataset_id": "dataset-a",
                "tenant_id": "tenant-a",
                "created_by": "user-a",
            }

        async def list_confluence_pages(self, _binding_id: str) -> list[dict[str, str]]:
            return [{"document_id": "document-a"}]

        async def delete_confluence_binding(self, _binding_id: str) -> bool:
            events.append("binding_deleted")
            return True

    class KnowledgeService:
        async def delete_document(self, *_args: Any, **_kwargs: Any) -> bool:
            return False

    service = _service(Database(), KnowledgeService())
    assert await service.delete_binding("binding-a", delete_documents=True) is False
    assert events == []


@pytest.mark.asyncio
async def test_binding_delete_retries_after_late_document_failure() -> None:
    binding = {
        "binding_id": "binding-a",
        "connection_id": "connection-a",
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "owner_id": "user-a",
        "created_by": "user-a",
    }
    pages = [
        {
            "id": "page-record-a",
            "binding_id": "binding-a",
            "page_id": "page-a",
            "document_id": "document-a",
        },
        {
            "id": "page-record-b",
            "binding_id": "binding-a",
            "page_id": "page-b",
            "document_id": "document-b",
        },
    ]

    class Database:
        def __init__(self) -> None:
            self.documents = {"document-a": {}, "document-b": {}}
            self.binding_deleted = False

        async def get_confluence_binding(self, _binding_id: str) -> dict[str, Any] | None:
            return None if self.binding_deleted else dict(binding)

        async def list_confluence_pages(self, _binding_id: str) -> list[dict[str, Any]]:
            return [dict(page) for page in pages]

        async def get_document(self, document_id: str) -> dict[str, Any] | None:
            return self.documents.get(document_id)

        async def get_confluence_page(self, page_record_id: str) -> dict[str, Any] | None:
            return next(
                (dict(page) for page in pages if page["id"] == page_record_id),
                None,
            )

        async def delete_confluence_binding(self, _binding_id: str) -> bool:
            self.binding_deleted = True
            return True

    database = Database()

    class KnowledgeService:
        def __init__(self) -> None:
            self.attempts: list[str] = []
            self.fail_document_b_once = True

        async def detach_confluence_source(self, _user: Any, _binding: dict, page: dict) -> bool:
            document_id = page["document_id"]
            self.attempts.append(document_id)
            if document_id not in database.documents:
                raise ConfluenceSyncError("document not found")
            if document_id == "document-b" and self.fail_document_b_once:
                self.fail_document_b_once = False
                raise RuntimeError("late document deletion failed")
            database.documents.pop(document_id)
            pages[:] = [item for item in pages if item["id"] != page["id"]]
            return True

    knowledge_service = KnowledgeService()
    service = _service(database, knowledge_service)

    assert await service.delete_binding("binding-a", delete_documents=True) is False
    assert database.documents == {"document-b": {}}
    assert database.binding_deleted is False

    assert await service.delete_binding("binding-a", delete_documents=True) is True
    assert database.documents == {}
    assert database.binding_deleted is True
    assert knowledge_service.attempts == [
        "document-a",
        "document-b",
        "document-b",
    ]


@pytest.mark.asyncio
async def test_atomic_page_and_document_delete_retries_without_second_cleanup() -> None:
    binding = {
        "binding_id": "binding-a",
        "connection_id": "connection-a",
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "owner_id": "user-a",
        "created_by": "user-a",
    }
    page = {
        "id": "page-record-a",
        "binding_id": "binding-a",
        "page_id": "page-a",
        "document_id": "document-a",
    }

    class Database:
        def __init__(self) -> None:
            self.document_exists = True
            self.page: dict[str, Any] | None = dict(page)
            self.delete_page_attempts = 0

        async def get_confluence_binding(self, _binding_id: str) -> dict[str, Any]:
            return dict(binding)

        async def get_confluence_page(self, _page_record_id: str) -> dict[str, Any] | None:
            return dict(self.page) if self.page else None

        async def get_document(self, _document_id: str) -> dict[str, Any] | None:
            return {} if self.document_exists else None

        async def delete_confluence_page(self, _page_record_id: str) -> bool:
            self.delete_page_attempts += 1
            if self.delete_page_attempts == 1:
                raise RuntimeError("page row deletion failed")
            self.page = None
            return True

    database = Database()

    class KnowledgeService:
        def __init__(self) -> None:
            self.attempts = 0

        async def detach_confluence_source(self, *_args: Any, **_kwargs: Any) -> bool:
            self.attempts += 1
            if self.attempts == 1:
                raise RuntimeError("page row deletion failed")
            database.document_exists = False
            database.page = None
            return True

    knowledge_service = KnowledgeService()
    service = _service(database, knowledge_service)
    user = UserContext(
        user_id="user-a",
        tenant_id="tenant-a",
        user_tier="normal",
        roles=["user"],
        is_authenticated=True,
    )

    first = await service.remove_pages(["page-record-a"], user, delete_documents=True)
    assert first == {
        "removed": 0,
        "documents_deleted": 0,
        "errors": [{"id": "page-record-a", "error": "page row deletion failed"}],
    }
    assert database.document_exists is True
    assert database.page == page

    second = await service.remove_pages(["page-record-a"], user, delete_documents=True)
    assert second == {"removed": 1, "documents_deleted": 1, "errors": []}
    assert database.page is None
    assert knowledge_service.attempts == 2

    assert database.delete_page_attempts == 0


@pytest.mark.asyncio
async def test_missing_document_is_not_accepted_without_matching_db_ownership() -> None:
    binding = {
        "binding_id": "binding-a",
        "connection_id": "connection-a",
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "owner_id": "user-a",
        "created_by": "user-a",
    }
    page = {
        "id": "page-record-a",
        "binding_id": "binding-a",
        "page_id": "page-a",
        "document_id": "document-a",
    }

    class Database:
        async def get_document(self, _document_id: str) -> None:
            return None

        async def get_confluence_binding(self, _binding_id: str) -> dict[str, Any]:
            return {**binding, "owner_id": "different-owner", "created_by": "different-owner"}

    class KnowledgeService:
        async def detach_confluence_source(self, *_args: Any, **_kwargs: Any) -> bool:
            raise ConfluenceSyncError("source ownership changed")

    service = _service(Database(), KnowledgeService())
    with pytest.raises(ConfluenceSyncError, match="ownership changed"):
        await service._delete_bound_document(binding, "document-a", page=page)


@pytest.mark.asyncio
async def test_confluence_write_guard_rejects_pending_dataset_deletion() -> None:
    class Database:
        async def get_dataset(self, _dataset_id: str) -> dict[str, Any]:
            return {
                "dataset_id": "dataset-a",
                "index_config": {
                    "retrieval": {
                        "_index_deletion_fence": make_dataset_index_deletion_fence(
                            "document_delete",
                            "document-a",
                        )
                    }
                },
            }

    service = _service(Database(), SimpleNamespace())
    with pytest.raises(ConfluenceSyncError, match="deletion is pending"):
        await service._require_dataset_index_writable("dataset-a")
