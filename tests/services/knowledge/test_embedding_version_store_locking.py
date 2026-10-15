"""Focused unit contracts for embedding rollback/reclamation locking."""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Any

import pytest
from knowledge_service.persistence.embedding_version_store import (
    EmbeddingVersionStore,
    MigrationStateError,
)


class _AsyncContext:
    def __init__(self, value: Any) -> None:
        self.value = value
        self.exit_exception: type[BaseException] | None = None

    async def __aenter__(self) -> Any:
        return self.value

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        _exc: BaseException | None,
        _traceback: Any,
    ) -> None:
        self.exit_exception = exc_type


class _Connection:
    def __init__(self, fetchrows: list[Any]) -> None:
        self.fetchrows = list(fetchrows)
        self.operations: list[tuple[str, str, tuple[Any, ...]]] = []
        self.transaction_context = _AsyncContext(None)

    def transaction(self) -> _AsyncContext:
        return self.transaction_context

    async def fetchrow(self, query: str, *args: Any) -> Any:
        self.operations.append(("fetchrow", query, args))
        return self.fetchrows.pop(0)

    async def fetchval(self, query: str, *args: Any) -> bool:
        self.operations.append(("fetchval", query, args))
        return True

    async def execute(self, query: str, *args: Any) -> str:
        self.operations.append(("execute", query, args))
        return "UPDATE 1"


class _Pool:
    def __init__(self, conn: _Connection) -> None:
        self.conn = conn

    def acquire(self) -> _AsyncContext:
        return _AsyncContext(self.conn)


def _binding_row(
    *, binding_id: str, state: str, collection_name: str
) -> dict[str, Any]:
    now = datetime.now(UTC)
    return {
        "binding_id": binding_id,
        "dataset_id": "ds-a",
        "tenant_id": "tenant-a",
        "collection_name": collection_name,
        "embedding_provider": "local",
        "embedding_model": "model-a",
        "embedding_model_version": "v1",
        "embedding_dimension": 3,
        "capabilities": "[]",
        "state": state,
        "created_at": now,
        "activated_at": now,
        "retired_at": None,
        "retained_until": now,
    }


@pytest.mark.asyncio
async def test_retire_binding_takes_dataset_lock_before_state_change() -> None:
    retired = _binding_row(
        binding_id="00000000-0000-0000-0000-000000000001",
        state="retired",
        collection_name="kb_ds_a_old",
    )
    conn = _Connection([{"dataset_id": "ds-a"}, retired])
    store = EmbeddingVersionStore(_Pool(conn))

    result = await store.retire_binding(str(retired["binding_id"]))

    assert result is not None and result["state"] == "retired"
    assert [operation for operation, _query, _args in conn.operations] == [
        "fetchrow",
        "fetchval",
        "fetchrow",
    ]
    lock = conn.operations[1]
    assert "pg_advisory_xact_lock" in lock[1]
    assert lock[2] == ("knowledge-dataset-index:ds-a",)


@pytest.mark.asyncio
async def test_rollback_aborts_when_retained_binding_promotion_updates_no_row() -> None:
    source_id = "00000000-0000-0000-0000-000000000001"
    target_id = "00000000-0000-0000-0000-000000000002"
    migration_id = "00000000-0000-0000-0000-000000000003"
    migration = {
        "migration_id": migration_id,
        "dataset_id": "ds-a",
        "source_binding_id": source_id,
        "target_binding_id": target_id,
        "state": "completed",
    }
    source = _binding_row(
        binding_id=source_id,
        state="retained",
        collection_name="kb_ds_a_old",
    )
    target = _binding_row(
        binding_id=target_id,
        state="serving",
        collection_name="kb_ds_a_new",
    )
    # migration, source, target, datasets pointer flip, failed source promotion
    conn = _Connection([migration, source, target, {"dataset_id": "ds-a"}, None])
    store = EmbeddingVersionStore(_Pool(conn))

    with pytest.raises(MigrationStateError, match="could not be promoted"):
        await store.rollback_migration(migration_id)

    assert conn.transaction_context.exit_exception is MigrationStateError
    promote_query = conn.operations[-1][1]
    assert "state = 'serving'" in promote_query
    assert "RETURNING binding_id" in promote_query
