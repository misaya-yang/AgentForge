from __future__ import annotations

from contextlib import asynccontextmanager
from unittest.mock import AsyncMock

import pytest
from knowledge_service.core.exceptions import PermissionDeniedError, ValidationFailedError
from knowledge_service.persistence.database import (
    dataset_index_deletion_fence,
    make_dataset_index_deletion_fence,
)
from knowledge_service.services.knowledge.dataset_service import DatasetService
from knowledge_service.services.knowledge.document_service import (
    _require_dataset_index_readable,
)

from src.core.auth.user_resolver import UserContext


@pytest.fixture
def mock_service() -> DatasetService:
    """Create a lightweight service instance for dataset deletion tests."""
    svc = object.__new__(DatasetService)
    svc.db = AsyncMock()
    svc._ks = AsyncMock()
    svc._ks.vector_store = AsyncMock()
    svc._ks.image_storage_service = AsyncMock()
    dataset = {
        "dataset_id": "kb_test",
        "tenant_id": "tenant_a",
        "collection_name": "col_kb_test",
    }
    svc.require_dataset_access = AsyncMock(return_value=dataset)
    svc.db.get_dataset.return_value = dataset
    lease_connection = object()
    svc._test_delete_lease_connection = lease_connection
    svc.db.set_dataset_index_deletion_fence.return_value = (dataset, True)
    svc.db.list_document_ids_by_dataset.return_value = []

    @asynccontextmanager
    async def delete_lease(_dataset_id: str):
        yield lease_connection

    svc.db.dataset_index_delete_lease = delete_lease
    return svc


@pytest.mark.asyncio
async def test_delete_dataset_requires_authenticated_user(mock_service: DatasetService) -> None:
    user = UserContext(user_id="anon:test", is_authenticated=False, roles=["guest"])

    with pytest.raises(PermissionDeniedError):
        await mock_service.delete_dataset(user, "kb_test", deletion_confirmed=True)


@pytest.mark.asyncio
async def test_delete_dataset_rejects_missing_gateway_confirmation(mock_service: DatasetService) -> None:
    user = UserContext(user_id="u_test", tenant_id="t1", is_authenticated=True, roles=["user"])

    with pytest.raises(PermissionDeniedError):
        await mock_service.delete_dataset(user, "kb_test", deletion_confirmed=False)

    mock_service.db.delete_dataset.assert_not_awaited()


@pytest.mark.asyncio
async def test_delete_dataset_accepts_verified_gateway_confirmation(mock_service: DatasetService) -> None:
    user = UserContext(user_id="u_test", tenant_id="tenant_a", is_authenticated=True, roles=["user"])
    mock_service.db.delete_dataset.return_value = True

    deleted = await mock_service.delete_dataset(
        user,
        "kb_test",
        deletion_confirmed=True,
        reason="cleanup obsolete kb",
    )

    assert deleted is True
    mock_service._ks.vector_store.delete_dataset_collections.assert_awaited_once_with(
        tenant_id="tenant_a",
        dataset_id="kb_test",
        authoritative_collection_names=["col_kb_test"],
        lifecycle_lease_held=True,
    )
    mock_service.db.delete_dataset.assert_awaited_once_with(
        "kb_test",
        deleted_by="u_test",
        delete_reason="cleanup obsolete kb",
        connection=mock_service._test_delete_lease_connection,
    )


@pytest.mark.asyncio
async def test_delete_dataset_soft_delete_and_audit(mock_service: DatasetService) -> None:
    user = UserContext(
        user_id="u_test", tenant_id="tenant_a", is_authenticated=True, roles=["user"]
    )
    mock_service.db.delete_dataset.return_value = True

    deleted = await mock_service.delete_dataset(
        user,
        "kb_test",
        deletion_confirmed=True,
        reason="cleanup obsolete kb",
    )

    assert deleted is True
    mock_service._ks.vector_store.delete_dataset_collections.assert_awaited_once_with(
        tenant_id="tenant_a",
        dataset_id="kb_test",
        authoritative_collection_names=["col_kb_test"],
        lifecycle_lease_held=True,
    )
    mock_service.db.delete_dataset.assert_awaited_once_with(
        "kb_test",
        deleted_by="u_test",
        delete_reason="cleanup obsolete kb",
        connection=mock_service._test_delete_lease_connection,
    )
    mock_service.db.log_audit.assert_awaited_once()


@pytest.mark.asyncio
async def test_dataset_delete_sweeps_every_document_asset_before_qdrant_and_db(
    mock_service: DatasetService,
) -> None:
    user = UserContext(
        user_id="u_test",
        tenant_id="tenant_a",
        is_authenticated=True,
        roles=["user"],
    )
    mock_service.db.list_document_ids_by_dataset.return_value = ["doc-a", "doc-b"]
    mock_service.db.delete_dataset.return_value = True
    events: list[str] = []

    async def delete_assets(*, tenant_id: str, document_id: str) -> int:
        assert tenant_id == "tenant_a"
        events.append(f"assets:{document_id}")
        return 1

    async def delete_vectors(**_kwargs) -> list[str]:
        events.append("qdrant")
        return ["col_kb_test"]

    async def delete_database(*_args, **_kwargs) -> bool:
        events.append("database")
        return True

    mock_service._ks.image_storage_service.delete_document_assets.side_effect = (
        delete_assets
    )
    mock_service._ks.vector_store.delete_dataset_collections.side_effect = (
        delete_vectors
    )
    mock_service.db.delete_dataset.side_effect = delete_database

    assert await mock_service.delete_dataset(
        user,
        "kb_test",
        deletion_confirmed=True,
    )

    assert events == ["assets:doc-a", "assets:doc-b", "qdrant", "database"]
    mock_service.db.list_document_ids_by_dataset.assert_awaited_once_with(
        "kb_test",
        connection=mock_service._test_delete_lease_connection,
    )


@pytest.mark.asyncio
async def test_dataset_asset_failure_keeps_qdrant_and_db_then_same_target_retry_converges(
    mock_service: DatasetService,
) -> None:
    user = UserContext(
        user_id="u_test",
        tenant_id="tenant_a",
        is_authenticated=True,
        roles=["user"],
    )
    mock_service.db.list_document_ids_by_dataset.return_value = ["doc-a", "doc-b"]
    mock_service.db.delete_dataset.return_value = True
    attempts: list[str] = []
    fail_doc_b = True

    async def delete_assets(*, tenant_id: str, document_id: str) -> int:
        nonlocal fail_doc_b
        assert tenant_id == "tenant_a"
        attempts.append(document_id)
        if document_id == "doc-b" and fail_doc_b:
            raise RuntimeError("object storage unavailable")
        return 1

    mock_service._ks.image_storage_service.delete_document_assets.side_effect = (
        delete_assets
    )

    with pytest.raises(RuntimeError, match="object storage unavailable"):
        await mock_service.delete_dataset(
            user,
            "kb_test",
            deletion_confirmed=True,
        )

    mock_service._ks.vector_store.delete_dataset_collections.assert_not_awaited()
    mock_service.db.delete_dataset.assert_not_awaited()

    fail_doc_b = False
    assert await mock_service.delete_dataset(
        user,
        "kb_test",
        deletion_confirmed=True,
    )

    assert attempts == ["doc-a", "doc-b", "doc-a", "doc-b"]
    mock_service._ks.vector_store.delete_dataset_collections.assert_awaited_once()
    mock_service.db.delete_dataset.assert_awaited_once()


@pytest.mark.asyncio
async def test_delete_dataset_keeps_database_when_vector_sweep_fails(
    mock_service: DatasetService,
) -> None:
    user = UserContext(
        user_id="u_test",
        tenant_id="tenant_a",
        is_authenticated=True,
        roles=["user"],
    )
    mock_service._ks.vector_store.delete_dataset_collections.side_effect = RuntimeError(
        "qdrant unavailable"
    )

    with pytest.raises(RuntimeError, match="qdrant unavailable"):
        await mock_service.delete_dataset(
            user,
            "kb_test",
            deletion_confirmed=True,
        )

    mock_service.db.delete_dataset.assert_not_awaited()
    mock_service.db.set_dataset_index_deletion_fence.assert_awaited_once()


@pytest.mark.asyncio
async def test_dataset_late_collection_failure_happens_after_durable_marker(
    mock_service: DatasetService,
) -> None:
    user = UserContext(
        user_id="u_test",
        tenant_id="tenant_a",
        is_authenticated=True,
        roles=["user"],
    )
    events: list[str] = []
    dataset = await mock_service.require_dataset_access(user, "kb_test", required="owner")

    async def set_fence(*_args, **_kwargs):
        events.append("marker-committed")
        dataset["index_config"] = {
            "retrieval": {
                "_index_deletion_fence": make_dataset_index_deletion_fence(
                    "dataset_delete",
                    "kb_test",
                )
            }
        }
        return dataset, True

    async def partial_sweep(**_kwargs):
        events.extend(["first-collection-deleted", "second-collection-failed"])
        raise RuntimeError("second collection failed")

    mock_service.db.set_dataset_index_deletion_fence.side_effect = set_fence
    mock_service._ks.vector_store.delete_dataset_collections.side_effect = partial_sweep

    with pytest.raises(RuntimeError, match="second collection failed"):
        await mock_service.delete_dataset(user, "kb_test", deletion_confirmed=True)

    assert events == [
        "marker-committed",
        "first-collection-deleted",
        "second-collection-failed",
    ]
    assert dataset_index_deletion_fence(dataset) == make_dataset_index_deletion_fence(
        "dataset_delete",
        "kb_test",
    )
    with pytest.raises(ValidationFailedError, match="indexed content is unavailable"):
        _require_dataset_index_readable(dataset)
    mock_service.db.delete_dataset.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", [False, RuntimeError("db commit failed")])
async def test_dataset_final_db_failure_never_reports_success_or_clears_fence(
    mock_service: DatasetService,
    failure: object,
) -> None:
    user = UserContext(
        user_id="u_test",
        tenant_id="tenant_a",
        is_authenticated=True,
        roles=["user"],
    )
    dataset = await mock_service.require_dataset_access(user, "kb_test", required="owner")

    async def set_fence(*_args, **_kwargs):
        dataset["index_config"] = {
            "retrieval": {
                "_index_deletion_fence": make_dataset_index_deletion_fence(
                    "dataset_delete",
                    "kb_test",
                )
            }
        }
        return dataset, True

    mock_service.db.set_dataset_index_deletion_fence.side_effect = set_fence
    if isinstance(failure, Exception):
        mock_service.db.delete_dataset.side_effect = failure
        expected = "db commit failed"
    else:
        mock_service.db.delete_dataset.return_value = failure
        expected = "database deletion failed"

    with pytest.raises(Exception, match=expected):
        await mock_service.delete_dataset(user, "kb_test", deletion_confirmed=True)

    mock_service.db.set_dataset_index_deletion_fence.assert_awaited_once()
    mock_service.db.log_audit.assert_not_awaited()
    assert dataset_index_deletion_fence(dataset) == make_dataset_index_deletion_fence(
        "dataset_delete",
        "kb_test",
    )
    with pytest.raises(ValidationFailedError, match="indexed content is unavailable"):
        _require_dataset_index_readable(dataset)


@pytest.mark.asyncio
async def test_dataset_same_target_retry_reuses_fence_and_finishes(
    mock_service: DatasetService,
) -> None:
    user = UserContext(
        user_id="u_test",
        tenant_id="tenant_a",
        is_authenticated=True,
        roles=["user"],
    )
    dataset = dict(await mock_service.require_dataset_access(user, "kb_test", required="owner"))
    dataset["index_config"] = {
        "retrieval": {
            "_index_deletion_fence": make_dataset_index_deletion_fence(
                "dataset_delete",
                "kb_test",
            )
        }
    }
    mock_service.require_dataset_access.return_value = dataset
    mock_service.db.set_dataset_index_deletion_fence.return_value = (dataset, False)
    mock_service.db.delete_dataset.return_value = True

    assert await mock_service.delete_dataset(
        user,
        "kb_test",
        deletion_confirmed=True,
    ) is True
    mock_service.db.set_dataset_index_deletion_fence.assert_awaited_once_with(
        "kb_test",
        operation="dataset_delete",
        target_id="kb_test",
        connection=mock_service._test_delete_lease_connection,
    )
    mock_service._ks.vector_store.delete_dataset_collections.assert_awaited_once()
    mock_service.db.delete_dataset.assert_awaited_once()


@pytest.mark.asyncio
async def test_dataset_delete_rejects_different_pending_target(
    mock_service: DatasetService,
) -> None:
    user = UserContext(
        user_id="u_test",
        tenant_id="tenant_a",
        is_authenticated=True,
        roles=["user"],
    )
    dataset = dict(await mock_service.require_dataset_access(user, "kb_test", required="owner"))
    dataset["index_config"] = {
        "retrieval": {
            "_index_deletion_fence": make_dataset_index_deletion_fence(
                "document_delete",
                "document-a",
            )
        }
    }
    mock_service.require_dataset_access.return_value = dataset

    with pytest.raises(ValidationFailedError, match="another dataset index deletion"):
        await mock_service.delete_dataset(user, "kb_test", deletion_confirmed=True)

    mock_service.db.set_dataset_index_deletion_fence.assert_not_awaited()
