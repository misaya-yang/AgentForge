"""Production repository SQL with controlled transactions/advisory-lock transport.

These tests exercise the real write/lease methods. The transport models the
required shared/exclusive lock conflict and transaction release; PostgreSQL SQL,
row locking, foreign keys and query plans still require separate live evidence.
"""
from __future__ import annotations

import asyncio
import copy
from contextlib import asynccontextmanager
from typing import Any

import pytest
from knowledge_service.persistence.database import (
    DatabaseStorage,
    IndexLeaseUnavailableError,
    make_dataset_index_deletion_fence,
)


class State:
    def __init__(self) -> None:
        self.bindings = {"binding": {"dataset_id": "dataset"}}
        self.datasets = {"dataset": {"tenant_id": "tenant", "index_config": {}}}
        self.documents = {"document": {"dataset_id": "dataset", "metadata": {"source_owner": "original"}}}
        self.pages = {"legacy-record-id": {
            "id": "legacy-record-id", "binding_id": "binding", "page_id": "page",
            "document_id": None,
        }}
        self.events: list[tuple[Any, ...]] = []
        self.shared: dict[str, set[Connection]] = {}
        self.exclusive: dict[str, Connection] = {}
        self.session_shared: dict[str, set[Connection]] = {}
        self.after_binding_read = None
        self.before_write = None
        self.fail_write = False


class Connection:
    def __init__(self, state: State, name: str) -> None:
        self.state = state
        self.name = name
        self.depth = 0
        self.pending: dict[str, dict[str, Any]] = {}
        self.transaction_locks: set[str] = set()

    @asynccontextmanager
    async def transaction(self):
        self.depth += 1
        self.state.events.append((self.name, "begin", self.depth))
        saved = copy.deepcopy(self.pending)
        try:
            yield
        except BaseException:
            self.pending = saved
            self.state.events.append((self.name, "rollback", self.depth))
            raise
        else:
            self.state.events.append((self.name, "commit", self.depth))
            if self.depth == 1:
                self.state.pages.update(self.pending)
                self.pending.clear()
        finally:
            self.depth -= 1
            if self.depth == 0:
                for key in self.transaction_locks:
                    self.state.shared.get(key, set()).discard(self)
                self.transaction_locks.clear()
                self.state.events.append((self.name, "transaction-released"))

    async def fetchval(self, query, *params):
        key = params[0]
        self.state.events.append((self.name, "lock", query, key, self.depth))
        if "pg_try_advisory_xact_lock_shared" in query:
            assert self.depth > 0
            if key in self.state.exclusive and self.state.exclusive[key] is not self:
                return False
            self.state.shared.setdefault(key, set()).add(self)
            self.transaction_locks.add(key)
            return True
        if "pg_try_advisory_lock_shared(" in query:
            if key in self.state.exclusive and self.state.exclusive[key] is not self:
                return False
            self.state.session_shared.setdefault(key, set()).add(self)
            return True
        if "pg_advisory_unlock_shared(" in query:
            self.state.session_shared.get(key, set()).remove(self)
            return True
        if "pg_try_advisory_lock(" in query:
            if (self.state.shared.get(key, set()) | self.state.session_shared.get(key, set())) - {self}:
                return False
            if key in self.state.exclusive and self.state.exclusive[key] is not self:
                return False
            self.state.exclusive[key] = self
            return True
        if "pg_advisory_unlock(" in query:
            return self.state.exclusive.pop(key, None) is self
        raise AssertionError(query)

    def page(self, key):
        return self.pending.get(key, self.state.pages.get(key))

    async def fetchrow(self, query, *params):
        normalized = " ".join(query.split())
        self.state.events.append((self.name, "read", normalized, params, self.depth))
        if normalized.startswith("SELECT * FROM confluence_connections"):
            return {"connection_id": params[0]}
        if normalized.startswith("SELECT * FROM confluence_sync_tasks"):
            return {"task_id": params[0]}
        if "FROM confluence_space_bindings" in normalized:
            binding = copy.deepcopy(self.state.bindings.get(params[0]))
            if "FOR SHARE" not in query and self.state.after_binding_read:
                callback, self.state.after_binding_read = self.state.after_binding_read, None
                await callback()
            return binding
        if "FROM datasets" in normalized:
            assert "is_deleted = FALSE AND is_archived = FALSE" in normalized
            row = self.state.datasets.get(params[0])
            if row and (row.get("is_deleted") or row.get("is_archived")):
                return None
            return copy.deepcopy(row)
        if "FROM documents" in normalized:
            assert "FOR SHARE" in query
            return copy.deepcopy(self.state.documents.get(params[0]))
        if normalized.startswith("SELECT binding_id, page_id FROM confluence_pages"):
            return copy.deepcopy(self.page(params[0]))
        if normalized.startswith("UPDATE documents"):
            assert "status = 'syncing'" in normalized
            assert "RETURNING document_id" in normalized
            return {"document_id": params[0]}
        if normalized.startswith("UPDATE confluence_pages"):
            assert "AND binding_id = $" in query and "AND page_id = $" in query
            assert "RETURNING id" in query
            record_id, binding_id, page_id = params[-3:]
            row = self.page(record_id)
            if not row or (row["binding_id"], row["page_id"]) != (binding_id, page_id):
                return None
            await self.before_write()
            changed = dict(row, status=params[0], document_id=params[1])
            self.pending[record_id] = changed
            self.state.events.append((self.name, "write", record_id, self.depth))
            return {"id": record_id}
        raise AssertionError(normalized)

    async def before_write(self):
        if self.state.before_write:
            await self.state.before_write()
        if self.state.fail_write:
            raise RuntimeError("controlled write failure")

    async def execute(self, query, *params):
        normalized = " ".join(query.split())
        if normalized.startswith("INSERT INTO confluence_pages"):
            assert "ON CONFLICT (binding_id, page_id) DO UPDATE SET" in normalized
            assert "ON CONFLICT (id)" not in normalized
            assert "binding_id = EXCLUDED" not in query
            assert "page_id = EXCLUDED" not in query
            record_id, binding_id, document_id, page_id = params[:4]
            rows = dict(self.state.pages, **self.pending)
            existing = next((row for row in rows.values()
                             if (row["binding_id"], row["page_id"]) == (binding_id, page_id)), None)
            if existing:
                record_id = existing["id"]
            elif record_id in rows:
                raise ValueError("controlled primary-key collision")
            await self.before_write()
            row = dict(existing or {}, id=record_id, binding_id=binding_id,
                       document_id=document_id, page_id=page_id)
            self.pending[record_id] = row
            self.state.events.append((self.name, "write", record_id, self.depth))
            return "INSERT 0 1"
        if normalized.startswith("UPDATE confluence_pages"):
            assert "document_id =" not in query
            self.state.events.append((self.name, "status", params, self.depth))
            return "UPDATE 1"
        raise AssertionError(normalized)


class Pool:
    def __init__(self, state: State):
        self.state = state
        self.acquires = 0

    @asynccontextmanager
    async def acquire(self):
        self.acquires += 1
        conn = Connection(self.state, f"pool-{self.acquires}")
        self.state.events.append((conn.name, "acquire"))
        yield conn
        self.state.events.append((conn.name, "release"))


def fixture():
    state = State()
    database = DatabaseStorage.__new__(DatabaseStorage)
    database._pool = Pool(state)
    return database, state


async def write(database, method, *, connection=None, document_id="document"):
    options = {} if connection is None else {"connection": connection}
    if method == "save":
        return await database.save_confluence_page(
            {"id": "new-record-id", "binding_id": "binding", "page_id": "page",
             "document_id": document_id, "space_key": "SPACE", "title": "Page"},
            **options,
        )
    if method == "upsert":
        return await database.upsert_confluence_page(
            "binding", "page", document_id=document_id, **options
        )
    return await database.update_confluence_page_status(
        "legacy-record-id", "synced", document_id=document_id, **options
    )


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["save", "upsert", "status"])
async def test_every_reference_writer_uses_same_transaction_dataset_lock(method):
    database, state = fixture()
    documents = copy.deepcopy(state.documents)
    await write(database, method)
    assert database._pool.acquires == 1
    assert state.pages["legacy-record-id"]["document_id"] == "document"
    assert set(state.pages) == {"legacy-record-id"}
    assert state.documents == documents  # Source ownership is never rewritten.
    locks = [event for event in state.events if event[1] == "lock"]
    assert len(locks) == 1
    assert locks[0][3] == database._dataset_index_lock_name("dataset")
    assert "pg_try_advisory_xact_lock_shared" in locks[0][2]
    kinds = [event[1] for event in state.events]
    assert kinds.index("begin") < kinds.index("lock") < kinds.index("write")
    assert kinds.index("write") < kinds.index("commit") < kinds.index("transaction-released")
    assert not state.shared[locks[0][3]]
    reads = [event[2] for event in state.events if event[1] == "read"]
    assert any("confluence_space_bindings" in query and "FOR SHARE" in query for query in reads)
    assert any("documents" in query and "FOR SHARE" in query for query in reads)


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["save", "upsert", "status"])
async def test_passed_connection_keeps_lock_until_outer_transaction_ends(method):
    database, state = fixture()
    conn = Connection(state, "owner")
    key = database._dataset_index_lock_name("dataset")
    async with conn.transaction():
        await write(database, method, connection=conn)
        assert database._pool.acquires == 0
        assert state.shared[key] == {conn}
        assert state.pages["legacy-record-id"]["document_id"] is None
        with pytest.raises(IndexLeaseUnavailableError):
            async with database.dataset_index_delete_lease("dataset"):
                pytest.fail("an outer transaction still owns the shared lock")
    assert state.pages["legacy-record-id"]["document_id"] == "document"
    assert not state.shared[key]
    async with database.dataset_index_delete_lease("dataset"):
        pass


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["save", "upsert", "status"])
async def test_active_delete_excludes_each_reference_writer(method):
    database, state = fixture()
    async with database.dataset_index_delete_lease("dataset"):
        with pytest.raises(IndexLeaseUnavailableError):
            await write(database, method)
    assert not any(event[1] == "write" for event in state.events)
    assert state.pages["legacy-record-id"]["document_id"] is None


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["save", "upsert", "status"])
@pytest.mark.parametrize("fault", ["missing-binding", "missing-document", "other-dataset",
                                  "missing-dataset", "deleted-dataset", "archived-dataset", "fenced"])
async def test_reference_validation_fails_closed(method, fault):
    database, state = fixture()
    if fault == "missing-binding":
        state.bindings.clear()
    elif fault == "missing-document":
        state.documents.clear()
    elif fault == "other-dataset":
        state.documents["document"]["dataset_id"] = "other"
    elif fault == "missing-dataset":
        state.datasets.clear()
    elif fault == "deleted-dataset":
        state.datasets["dataset"]["is_deleted"] = True
    elif fault == "archived-dataset":
        state.datasets["dataset"]["is_archived"] = True
    else:
        state.datasets["dataset"]["index_config"] = {
            "retrieval": {"_index_deletion_fence": make_dataset_index_deletion_fence(
                "document_delete", "document")}}
    with pytest.raises(RuntimeError):
        await write(database, method)
    assert not any(event[1] == "write" for event in state.events)


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["save", "upsert", "status"])
@pytest.mark.parametrize("change", ["delete-binding", "move-binding", "delete-document"])
async def test_prelock_read_cannot_authorize_after_identity_change(method, change):
    database, state = fixture()

    async def change_identity():
        async with database.dataset_index_delete_lease("dataset"):
            if change == "delete-binding":
                state.bindings.clear()
            elif change == "move-binding":
                state.bindings["binding"]["dataset_id"] = "other"
            else:
                state.documents.clear()

    state.after_binding_read = change_identity
    with pytest.raises(RuntimeError):
        await write(database, method)
    assert not any(event[1] == "write" for event in state.events)


@pytest.mark.asyncio
async def test_inflight_reference_write_excludes_delete_until_commit():
    database, state = fixture()
    reached, release = asyncio.Event(), asyncio.Event()

    async def pause():
        reached.set()
        await release.wait()

    state.before_write = pause
    task = asyncio.create_task(write(database, "upsert"))
    await asyncio.wait_for(reached.wait(), timeout=2)
    with pytest.raises(IndexLeaseUnavailableError):
        async with database.dataset_index_delete_lease("dataset"):
            pytest.fail("delete overlapped an uncommitted reference")
    release.set()
    await task
    async with database.dataset_index_delete_lease("dataset"):
        assert state.pages["legacy-record-id"]["document_id"] == "document"


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["save", "upsert", "status"])
async def test_write_failure_rolls_back_and_releases_reference_lock(method):
    database, state = fixture()
    state.fail_write = True
    with pytest.raises(RuntimeError, match="controlled write failure"):
        await write(database, method)
    assert state.pages["legacy-record-id"]["document_id"] is None
    async with database.dataset_index_delete_lease("dataset"):
        pass


@pytest.mark.asyncio
async def test_cancelled_reference_write_releases_lock_without_committing():
    database, state = fixture()
    reached = asyncio.Event()

    async def pause():
        reached.set()
        await asyncio.Event().wait()

    state.before_write = pause
    task = asyncio.create_task(write(database, "upsert"))
    await asyncio.wait_for(reached.wait(), timeout=2)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert state.pages["legacy-record-id"]["document_id"] is None
    async with database.dataset_index_delete_lease("dataset"):
        pass


@pytest.mark.asyncio
async def test_status_only_update_does_not_claim_reference_or_delete_authority():
    database, state = fixture()
    state.bindings.clear()
    state.documents.clear()
    async with database.dataset_index_delete_lease("dataset"):
        await database.update_confluence_page_status("legacy-record-id", "error", error="failed")
    assert not any("shared" in event[2] for event in state.events if event[1] == "lock")
    assert any(event[1] == "status" for event in state.events)
    assert not any(event[1] == "begin" for event in state.events)


@pytest.mark.asyncio
@pytest.mark.parametrize("change", ["removed", "rebound"])
async def test_status_reference_rechecks_exact_page_identity(change):
    database, state = fixture()

    async def mutate_page():
        if change == "removed":
            state.pages.clear()
        else:
            state.pages["legacy-record-id"]["binding_id"] = "other"

    state.after_binding_read = mutate_page
    with pytest.raises(RuntimeError, match="page changed"):
        await write(database, "status")
    assert not any(event[1] == "write" for event in state.events)


@pytest.mark.asyncio
async def test_missing_status_reference_page_does_not_create_one():
    database, state = fixture()
    state.pages.clear()
    with pytest.raises(RuntimeError, match="page is missing"):
        await write(database, "status")
    assert not state.pages


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["save", "upsert"])
async def test_new_reference_inserts_but_id_collision_cannot_rebind_other_source(method):
    database, state = fixture()
    state.pages.clear()
    await write(database, method)
    assert len(state.pages) == 1
    row = next(iter(state.pages.values()))
    assert (row["binding_id"], row["page_id"], row["document_id"]) == ("binding", "page", "document")
    row["binding_id"] = "other"
    with pytest.raises(ValueError, match="primary-key collision"):
        await write(database, method)
    assert next(iter(state.pages.values()))["binding_id"] == "other"


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["get_confluence_binding", "get_confluence_connection", "get_confluence_sync_task"])
@pytest.mark.parametrize("has_pool", [True, False])
async def test_guard_reads_reuse_exact_supplied_connection(method, has_pool):
    database, state = fixture()
    pool = database._pool
    if not has_pool:
        database._pool = None
    conn = Connection(state, "owner")
    key = {"get_confluence_binding": "binding", "get_confluence_connection": "connection",
           "get_confluence_sync_task": "task"}[method]
    assert await getattr(database, method)(key, connection=conn)
    assert pool.acquires == 0
    assert all(event[0] == "owner" for event in state.events)


@pytest.mark.asyncio
async def test_existing_source_publish_reuses_document_owner_lease_connection():
    database, state = fixture()
    async with database.document_index_update_lease("dataset", "document") as conn:
        async with conn.transaction():
            assert await database.prepare_confluence_document_update(
                "document", "dataset", generation="generation", title="Title", content="body",
                confluence_version=2, source_metadata={},
                page_record={"binding_id": "binding", "page_id": "page"}, connection=conn,
            )
        assert state.pages["legacy-record-id"]["document_id"] == "document"
        assert database._pool.acquires == 1
        assert state.session_shared[database._dataset_index_lock_name("dataset")] == {conn}
    assert not state.session_shared[database._dataset_index_lock_name("dataset")]
    assert not state.exclusive


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["save", "upsert"])
async def test_unlinked_page_writes_validate_binding_without_requiring_document(method):
    database, state = fixture()
    state.documents.clear()
    await write(database, method, document_id=None)
    assert state.pages["legacy-record-id"]["document_id"] is None
    assert any(event[1] == "lock" and "shared" in event[2] for event in state.events)
    assert not any(event[1] == "read" and "FROM documents" in event[2] for event in state.events)


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["save", "upsert", "status"])
async def test_empty_document_reference_is_rejected_without_a_write(method):
    database, state = fixture()
    with pytest.raises(ValueError, match="document_id"):
        await write(database, method, document_id=" ")
    assert not any(event[1] == "write" for event in state.events)
