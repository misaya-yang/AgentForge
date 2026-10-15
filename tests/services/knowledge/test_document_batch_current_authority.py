"""Persisted batch provenance must not become current dispatch authority."""
from __future__ import annotations

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException
from knowledge_service.auth.user_context import UserContext
from knowledge_service.persistence.document_batches import ClaimedDocumentBatchItem
from knowledge_service.services.knowledge import worker as worker_module
from knowledge_service.services.knowledge.dataset_service import DatasetService
from knowledge_service.services.knowledge.worker import KnowledgeWorker


def setup_case(monkeypatch, operation, *, grant=True, roles=None):
    actor = UserContext(user_id="creator", tenant_id="tenant", roles=roles or ["user"])
    resolver = AsyncMock(return_value=actor)
    # raising=False lets the same test prove the old worker never calls it.
    monkeypatch.setattr(worker_module, "resolve_runtime_actor", resolver, raising=False)
    database = SimpleNamespace(
        get_dataset=AsyncMock(return_value={
            "dataset_id": "dataset", "tenant_id": "tenant", "created_by": "someone-else",
            "visibility": "private", "is_deleted": False,
        }),
        get_dataset_permission=AsyncMock(return_value={"permission": "editor"} if grant else None),
    )
    policy = DatasetService(SimpleNamespace(), database)
    service = SimpleNamespace(
        require_dataset_access=AsyncMock(side_effect=policy.require_dataset_access),
        delete_document=AsyncMock(return_value=True),
    )
    worker = object.__new__(KnowledgeWorker)
    worker.service = service
    worker.enqueue = AsyncMock(return_value=True)
    store = SimpleNamespace(complete_item=AsyncMock(), release_item=AsyncMock())
    item = ClaimedDocumentBatchItem(
        operation_id="operation", tenant_id="tenant", dataset_id="dataset",
        operation=operation, document_id="document", created_by="creator",
        actor_roles=("admin",),
    )
    return worker, store, item, resolver, actor, service, database


def assert_rejected(worker, store, service):
    worker.enqueue.assert_not_awaited()
    service.delete_document.assert_not_awaited()
    store.release_item.assert_not_awaited()
    assert store.complete_item.await_count == 1
    assert store.complete_item.call_args.kwargs["status"] == "failed"
    assert store.complete_item.call_args.kwargs["error_code"] == "operation_failed"


@pytest.mark.parametrize("operation", ["delete", "reembed"])
@pytest.mark.parametrize("status", [403, 503])
async def test_disabled_or_unavailable_actor_blocks_dispatch(monkeypatch, operation, status):
    worker, store, item, resolver, _, service, _ = setup_case(monkeypatch, operation)
    resolver.side_effect = HTTPException(status, "capability identity unavailable")
    await worker._dispatch_document_batch_item(store, item)
    assert_rejected(worker, store, service)
    service.require_dataset_access.assert_not_awaited()


@pytest.mark.parametrize("operation", ["delete", "reembed"])
async def test_removed_admin_does_not_reuse_persisted_roles(monkeypatch, operation):
    worker, store, item, resolver, actor, service, _ = setup_case(monkeypatch, operation, grant=False)
    await worker._dispatch_document_batch_item(store, item)
    assert_rejected(worker, store, service)
    resolver.assert_awaited_once_with(tenant_id="tenant", user_id="creator")
    service.require_dataset_access.assert_awaited_once_with(actor, "dataset", required="editor")


@pytest.mark.parametrize("operation", ["delete", "reembed"])
@pytest.mark.parametrize("change", ["user", "tenant", "anonymous"])
async def test_wrong_actor_identity_blocks_dispatch(monkeypatch, operation, change):
    worker, store, item, resolver, _, service, _ = setup_case(monkeypatch, operation)
    resolver.return_value = UserContext(
        user_id="other" if change == "user" else "creator",
        tenant_id="other" if change == "tenant" else "tenant",
        is_authenticated=change != "anonymous",
    )
    await worker._dispatch_document_batch_item(store, item)
    assert_rejected(worker, store, service)
    service.require_dataset_access.assert_not_awaited()


@pytest.mark.parametrize("operation", ["delete", "reembed"])
async def test_current_editor_dispatch_uses_current_actor(monkeypatch, operation):
    worker, store, item, resolver, actor, service, _ = setup_case(monkeypatch, operation)
    await worker._dispatch_document_batch_item(store, item)
    resolver.assert_awaited_once_with(tenant_id="tenant", user_id="creator")
    service.require_dataset_access.assert_awaited_once_with(actor, "dataset", required="editor")
    if operation == "delete":
        service.delete_document.assert_awaited_once_with(actor, "dataset", "document")
        assert service.delete_document.call_args.args[0].roles == ["user"]
        worker.enqueue.assert_not_awaited()
    else:
        worker.enqueue.assert_awaited_once_with("dataset", "document", action="reembed")
        service.delete_document.assert_not_awaited()
    store.complete_item.assert_awaited_once_with(
        operation_id="operation", document_id="document", status="queued", error_code=None,
    )


@pytest.mark.parametrize("operation", ["delete", "reembed"])
async def test_current_acl_failure_blocks_dispatch(monkeypatch, operation):
    worker, store, item, _, _, service, database = setup_case(monkeypatch, operation)
    database.get_dataset_permission.side_effect = RuntimeError("authority unavailable")
    await worker._dispatch_document_batch_item(store, item)
    assert_rejected(worker, store, service)


@pytest.mark.parametrize("operation", ["delete", "reembed"])
async def test_dataset_tenant_change_blocks_dispatch(monkeypatch, operation):
    worker, store, item, _, _, service, _ = setup_case(monkeypatch, operation)
    service.require_dataset_access.side_effect = None
    service.require_dataset_access.return_value = {"tenant_id": "other"}
    await worker._dispatch_document_batch_item(store, item)
    assert_rejected(worker, store, service)


@pytest.mark.parametrize("operation", ["delete", "reembed"])
async def test_cancellation_is_not_failed_or_retried(monkeypatch, operation):
    worker, store, item, resolver, _, service, _ = setup_case(monkeypatch, operation)
    resolver.side_effect = asyncio.CancelledError()
    with pytest.raises(asyncio.CancelledError):
        await worker._dispatch_document_batch_item(store, item)
    worker.enqueue.assert_not_awaited()
    service.delete_document.assert_not_awaited()
    store.complete_item.assert_not_awaited()
    store.release_item.assert_not_awaited()


async def test_unsupported_operation_keeps_existing_receipt(monkeypatch):
    worker, store, item, resolver, _, service, _ = setup_case(monkeypatch, "unsupported")
    await worker._dispatch_document_batch_item(store, item)
    resolver.assert_not_awaited()
    service.require_dataset_access.assert_not_awaited()
    worker.enqueue.assert_not_awaited()
    service.delete_document.assert_not_awaited()
    store.complete_item.assert_awaited_once_with(
        operation_id="operation", document_id="document", status="failed",
        error_code="unsupported_operation",
    )
