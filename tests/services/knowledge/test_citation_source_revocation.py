"""Controlled production-route reads across committed authority transitions."""
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException
from knowledge_service.api.routes.knowledge import get_active_document_source
from knowledge_service.core.auth.user_resolver import UserContext
from knowledge_service.core.exceptions import PermissionDeniedError


@pytest.mark.asyncio
@pytest.mark.parametrize("transition", [
    "unchanged", "disabled", "archived", "lifecycle_marker", "acl_revoked",
    "revision_changed", "tenant_changed", "wrong_document_dataset", "active_read_failed",
])
async def test_citation_source_retires_changes_during_body_fetch(transition):
    dataset = {"dataset_id": "dataset-a", "tenant_id": "tenant-a",
               "content_revision": 7, "index_config": {}}
    document = {"document_id": "document-a", "dataset_id": "dataset-a",
                "content": "source bytes", "status": "completed",
                "enabled": True, "archived": False, "metadata": {}}
    user = UserContext(user_id="user-a", tenant_id="tenant-a")
    revoked = False
    events = []

    async def access(*_args, **_kwargs):
        events.append("acl")
        if revoked:
            raise PermissionDeniedError("viewer grant revoked")
        return dict(dataset)

    async def get_document(document_id):
        nonlocal revoked
        assert document_id == "document-a"
        events.append("body")
        # These are deterministic await-boundary transitions, not a simulated
        # SQL transaction: the real repository reads use independent acquires.
        if transition == "disabled":
            document["enabled"] = False
        elif transition == "archived":
            document["archived"] = True
        elif transition == "lifecycle_marker":
            document["metadata"] = {"_document_lifecycle_reindex": {"status": "deactivating"}}
        elif transition == "acl_revoked":
            revoked = True
        elif transition == "revision_changed":
            dataset["content_revision"] = 8
        elif transition == "tenant_changed":
            dataset["tenant_id"] = "tenant-b"
        elif transition == "wrong_document_dataset":
            document["dataset_id"] = "dataset-b"
        return dict(document)

    async def active(dataset_id, tenant_id, ids):
        events.append("active")
        assert (dataset_id, tenant_id, ids) == ("dataset-a", "tenant-a", ["document-a"])
        if transition == "active_read_failed":
            raise RuntimeError("authority unavailable")
        return ({"document-a"} if document["enabled"] and not document["archived"]
                and "_document_lifecycle_reindex" not in document["metadata"] else set())

    svc = SimpleNamespace(
        # Existing management read succeeds before the later citation read.
        get_document=AsyncMock(return_value=dict(document)),
        require_dataset_access=access,
        db=SimpleNamespace(get_document=get_document, filter_active_document_ids=active),
    )
    if transition == "unchanged":
        result = await get_active_document_source("dataset-a", "document-a", svc=svc, user=user)
        assert result["content"] == "source bytes"
        assert events == ["acl", "body", "active", "acl"]
    elif transition == "active_read_failed":
        with pytest.raises(RuntimeError, match="authority unavailable"):
            await get_active_document_source("dataset-a", "document-a", svc=svc, user=user)
    else:
        with pytest.raises(HTTPException) as denied:
            await get_active_document_source("dataset-a", "document-a", svc=svc, user=user)
        assert denied.value.status_code == (403 if transition == "acl_revoked" else 404)
