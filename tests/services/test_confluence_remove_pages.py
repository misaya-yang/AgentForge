from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
from knowledge_service.services.knowledge.confluence.sync_service import ConfluenceSyncService

from src.core.auth.user_resolver import UserContext


@pytest.mark.asyncio
@pytest.mark.parametrize("deleted", [True, False])
async def test_remove_pages_uses_exact_source_detach_and_counts_documents(deleted: bool) -> None:
    binding = {
        "binding_id": "binding-1",
        "dataset_id": "dataset-1",
        "connection_id": "connection-1",
        "tenant_id": "tenant-1",
        "owner_id": "user-1",
        "created_by": "user-1",
    }
    database = MagicMock()
    database.get_confluence_page = AsyncMock(
        return_value={
            "id": "page-record-1",
            "binding_id": "binding-1",
            "document_id": "document-1",
            "page_id": "provider-page-1",
        }
    )
    database.get_confluence_binding = AsyncMock(return_value=binding)
    database.delete_confluence_page = AsyncMock(return_value=True)
    knowledge_service = MagicMock()
    knowledge_service.delete_document = AsyncMock()
    knowledge_service.detach_confluence_source = AsyncMock(return_value=deleted)
    service = ConfluenceSyncService(
        settings=SimpleNamespace(confluence=SimpleNamespace(client_cache_ttl_seconds=300)),
        database=database,
        knowledge_service=knowledge_service,
    )
    user = UserContext(
        user_id="user-1",
        tenant_id="tenant-1",
        tier="normal",
        is_authenticated=True,
        roles=["user"],
    )

    result = await service.remove_pages(["page-record-1"], user, delete_documents=True)

    assert result == {"removed": 1, "documents_deleted": int(deleted), "errors": []}
    knowledge_service.detach_confluence_source.assert_awaited_once_with(
        user, binding, database.get_confluence_page.return_value
    )
    knowledge_service.delete_document.assert_not_awaited()
    database.delete_confluence_page.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("missing", ["connection_id", "created_by"])
async def test_remove_pages_incomplete_source_identity_preserves_reference(missing: str) -> None:
    binding = {
        "binding_id": "binding-1",
        "dataset_id": "dataset-1",
        "tenant_id": "tenant-1",
        "connection_id": "connection-1",
        "owner_id": "user-1",
        "created_by": "user-1",
    }
    binding.pop(missing)
    database = MagicMock()
    database.get_confluence_page = AsyncMock(return_value={
        "id": "page-record-1", "page_id": "provider-page-1",
        "binding_id": "binding-1", "document_id": "document-1",
    })
    database.get_confluence_binding = AsyncMock(return_value=binding)
    database.delete_confluence_page = AsyncMock()
    knowledge_service = MagicMock()
    knowledge_service.detach_confluence_source = AsyncMock()
    knowledge_service.delete_document = AsyncMock()
    service = ConfluenceSyncService(
        settings=SimpleNamespace(confluence=SimpleNamespace(client_cache_ttl_seconds=300)),
        database=database,
        knowledge_service=knowledge_service,
    )
    user = UserContext(
        user_id="user-1", tenant_id="tenant-1", tier="normal",
        is_authenticated=True, roles=["user"],
    )

    result = await service.remove_pages(["page-record-1"], user, delete_documents=True)

    assert result["removed"] == result["documents_deleted"] == 0
    assert result["errors"] == [{
        "id": "page-record-1",
        "error": "binding deletion requires complete source identity and created_by",
    }]
    knowledge_service.detach_confluence_source.assert_not_awaited()
    knowledge_service.delete_document.assert_not_awaited()
    database.delete_confluence_page.assert_not_awaited()
