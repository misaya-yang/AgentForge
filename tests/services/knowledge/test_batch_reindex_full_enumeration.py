"""Batch reindex must see every document, not just the first page.

``DocumentService.list_documents`` caps a single page (limit=200) because it
backs the list UI. ``list_all_document_ids`` exists so bulk operations such as
``POST /knowledge/{id}/documents/batch-reindex`` with ``all_documents=true``
cannot silently skip documents past that cap.
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.core.auth.user_resolver import UserContext
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.document_service import DocumentService

USER = UserContext(user_id="user-a", tenant_id="tenant-a")


def _dataset_row(*, content_revision: int = 7) -> dict[str, Any]:
    return {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "index_config": {},
        "content_revision": content_revision,
    }


class EnumerationDatabase:
    def __init__(self, document_ids: list[str]) -> None:
        self.document_ids = document_ids
        self.list_calls = 0

    async def list_document_ids_by_dataset(self, dataset_id, *, connection=None, active_only=False, tenant_id=None):
        assert dataset_id == "dataset-a"
        assert connection is None
        assert active_only is False
        assert tenant_id == "tenant-a"
        self.list_calls += 1
        return list(self.document_ids)


class EnumerationKnowledge:
    def __init__(self, *, bump_revision_on_reread: bool = False) -> None:
        self.bump_revision_on_reread = bump_revision_on_reread
        self.access_calls: list[str] = []

    async def require_dataset_access(self, _user, dataset_id, *, required):
        assert dataset_id == "dataset-a"
        self.access_calls.append(required)
        revision = 8 if (self.bump_revision_on_reread and len(self.access_calls) > 1) else 7
        return _dataset_row(content_revision=revision)

    async def _effective_dataset_permission(self, _dataset, _user):
        return "editor"


def _make_service(document_ids: list[str], *, bump: bool = False):
    database = EnumerationDatabase(document_ids)
    knowledge = EnumerationKnowledge(bump_revision_on_reread=bump)
    service = DocumentService(SimpleNamespace(), database)  # type: ignore[arg-type]
    service._ks = knowledge  # type: ignore[assignment]
    return service, database, knowledge


async def test_list_all_document_ids_returns_every_document_beyond_the_page_cap() -> None:
    ids = [f"document-{index:04d}" for index in range(321)]
    service, database, knowledge = _make_service(ids)

    result = await service.list_all_document_ids(USER, "dataset-a")

    assert result == ids
    assert len(result) > 200  # regression guard: the old path capped at 200
    assert database.list_calls == 1
    # One viewer check up front, one re-check after the read (generation fence).
    assert knowledge.access_calls == ["viewer", "viewer"]


async def test_list_all_document_ids_rejects_generation_change_mid_read() -> None:
    service, database, _knowledge = _make_service(["document-a"], bump=True)

    with pytest.raises(ValidationFailedError, match="retry the request"):
        await service.list_all_document_ids(USER, "dataset-a")

    assert database.list_calls == 1
