"""Dataset archive hides access while preserving the reversible source state."""

from __future__ import annotations

from contextlib import asynccontextmanager
from copy import deepcopy
from datetime import datetime, timezone
from types import SimpleNamespace
from typing import Any

import pytest
from fastapi import HTTPException, Response
from knowledge_service.api.routes import knowledge as routes
from knowledge_service.auth.user_context import UserContext
from knowledge_service.core.exceptions import PermissionDeniedError, ValidationFailedError
from knowledge_service.persistence.database import IndexLeaseUnavailableError
from knowledge_service.services.knowledge.dataset_service import DatasetService

OWNER = UserContext(user_id="owner-a", tenant_id="tenant-a")
EDITOR = UserContext(user_id="editor-a", tenant_id="tenant-a")
FOREIGN_ADMIN = UserContext(
    user_id="admin-b", tenant_id="tenant-b", user_tier="admin", roles=["admin"],
)


class ArchiveDatabase:
    def __init__(self) -> None:
        self.dataset = {
            "dataset_id": "dataset-a", "name": "A", "tenant_id": "tenant-a",
            "visibility": "public", "created_by": OWNER.user_id,
            "created_at": datetime.now(timezone.utc),
            "is_deleted": False, "is_archived": False,
            "content_revision": 7, "collection_name": "collection-a",
            "embedding_provider": "local", "embedding_model": "hash-384",
            "embedding_dimension": 384, "embedding_config": {}, "index_config": {},
        }
        self.permissions = {EDITOR.user_id: "editor"}
        self.resources = {
            "segments": ["segment-a"], "points": ["point-a"],
            "acl": deepcopy(self.permissions), "agent_bindings": ["agent-a"],
        }
        self.transitions = 0
        self.running_execution = False
        self.lease_unavailable = False
        self.lease_held = False

    @asynccontextmanager
    async def dataset_index_delete_lease(self, _dataset_id: str):
        if self.lease_unavailable:
            raise IndexLeaseUnavailableError("dataset index lifecycle work is already in progress")
        self.lease_held = True
        try:
            yield SimpleNamespace()
        finally:
            self.lease_held = False

    async def get_dataset(
        self, dataset_id: str, *, include_archived: bool = False,
    ) -> dict[str, Any] | None:
        if dataset_id != self.dataset["dataset_id"]:
            return None
        if self.dataset["is_archived"] and not include_archived:
            return None
        return deepcopy(self.dataset)

    async def list_datasets(
        self, *, tenant_id: str, include_public: bool, limit: int, offset: int,
        archived_only: bool = False, **_kwargs: Any,
    ) -> list[dict[str, Any]]:
        del offset
        if self.dataset["is_archived"] != archived_only:
            return []
        if self.dataset["tenant_id"] != tenant_id and not (
            include_public and self.dataset["visibility"] == "public"
        ):
            return []
        return [deepcopy(self.dataset)][:limit]

    async def get_dataset_permission(
        self, _dataset_id: str, subject_type: str, subject_id: str,
    ) -> dict[str, str] | None:
        if subject_type == "user" and subject_id in self.permissions:
            return {"permission": self.permissions[subject_id]}
        return None

    async def get_dataset_permissions_batch(self, dataset_ids, *, user_id, **_kwargs):
        return {dataset_id: {("user", user_id): {"permission": self.permissions[user_id]}}
                for dataset_id in dataset_ids if user_id in self.permissions}

    async def get_datasets_statistics_batch(
        self, _dataset_ids: list[str],
    ) -> dict[str, dict[str, int]]:
        return {}

    async def set_dataset_archived(
        self, dataset_id: str, *, archived: bool, user_id: str,
        tenant_id: str, roles: list[str], tenant_admin: bool,
        reason: str | None, connection: Any,
    ) -> dict[str, Any] | None:
        del roles, tenant_admin, connection
        assert self.lease_held
        if dataset_id != self.dataset["dataset_id"]:
            return None
        if user_id != OWNER.user_id or tenant_id != "tenant-a":
            raise PermissionError("owner revoked")
        if archived and self.running_execution:
            raise RuntimeError("dataset has a running document execution")
        if self.dataset["is_archived"] != archived:
            self.dataset["is_archived"] = archived
            self.dataset["content_revision"] += 1
            self.dataset["archived_at"] = datetime.now(timezone.utc) if archived else None
            self.dataset["archived_by"] = user_id if archived else None
            self.dataset["archive_reason"] = reason if archived else None
            self.transitions += 1
        return deepcopy(self.dataset)


def make_service() -> tuple[DatasetService, ArchiveDatabase]:
    database = ArchiveDatabase()
    return DatasetService(SimpleNamespace(), database), database


@pytest.mark.asyncio
async def test_owner_archive_restore_hides_default_access_and_preserves_resources() -> None:
    service, database = make_service()
    resources_before = deepcopy(database.resources)

    archived = await service.set_dataset_archived(
        OWNER, "dataset-a", archived=True, reason="No longer active",
    )
    assert archived["is_archived"] is True
    assert archived["content_revision"] == 8
    assert (await service.list_datasets_page(OWNER))["items"] == []
    assert await service.authorize_datasets(OWNER, ["dataset-a"]) == []
    with pytest.raises(ValidationFailedError, match="not found"):
        await service.require_dataset_access(OWNER, "dataset-a")

    archived_page = await service.list_datasets_page(OWNER, archived_only=True)
    assert [item["dataset_id"] for item in archived_page["items"]] == ["dataset-a"]
    assert archived_page["items"][0]["my_permission"] == "owner"
    assert (await service.list_datasets_page(EDITOR, archived_only=True))["items"] == []

    replay = await service.set_dataset_archived(OWNER, "dataset-a", archived=True)
    assert replay["content_revision"] == 8 and database.transitions == 1
    restored = await service.set_dataset_archived(OWNER, "dataset-a", archived=False)
    assert restored["content_revision"] == 9
    assert restored["is_archived"] is False
    assert await service.authorize_datasets(EDITOR, ["dataset-a"]) == ["dataset-a"]
    assert database.resources == resources_before


@pytest.mark.asyncio
async def test_archive_requires_owner_even_for_public_dataset() -> None:
    service, database = make_service()
    for actor in (EDITOR, FOREIGN_ADMIN):
        with pytest.raises(PermissionDeniedError, match="owner"):
            await service.set_dataset_archived(actor, "dataset-a", archived=True)
    assert database.transitions == 0


@pytest.mark.asyncio
async def test_archive_routes_expose_owner_catalog_and_forbid_editor_write() -> None:
    service, _database = make_service()
    svc = SimpleNamespace(
        dataset_service=service, list_datasets_page=service.list_datasets_page,
    )
    response = Response()
    await routes.set_dataset_archived(
        "dataset-a", routes.DatasetArchiveRequest(archived=True), svc=svc, user=OWNER,
    )
    rows = await routes.list_datasets(
        response, limit=200, cursor=None, archived=True, svc=svc, user=OWNER,
    )
    assert [item["dataset_id"] for item in rows] == ["dataset-a"]
    with pytest.raises(HTTPException) as denied:
        await routes.set_dataset_archived(
            "dataset-a", routes.DatasetArchiveRequest(archived=False),
            svc=svc, user=EDITOR,
        )
    assert denied.value.status_code == 403
    with pytest.raises(HTTPException) as missing:
        await routes.set_dataset_archived(
            "missing", routes.DatasetArchiveRequest(archived=True),
            svc=svc, user=OWNER,
        )
    assert missing.value.status_code == 404


@pytest.mark.asyncio
async def test_archive_rejects_running_execution_and_document_lease_contention() -> None:
    service, database = make_service()
    svc = SimpleNamespace(dataset_service=service)
    database.running_execution = True
    with pytest.raises(HTTPException) as pending:
        await routes.set_dataset_archived(
            "dataset-a", routes.DatasetArchiveRequest(archived=True),
            svc=svc, user=OWNER,
        )
    assert pending.value.status_code == 409
    assert database.dataset["content_revision"] == 7
    assert database.dataset["is_archived"] is False

    database.running_execution = False
    database.lease_unavailable = True
    with pytest.raises(HTTPException) as contended:
        await routes.set_dataset_archived(
            "dataset-a", routes.DatasetArchiveRequest(archived=True),
            svc=svc, user=OWNER,
        )
    assert contended.value.status_code == 409
    assert contended.value.headers == {"Retry-After": "1"}
    assert database.transitions == 0
