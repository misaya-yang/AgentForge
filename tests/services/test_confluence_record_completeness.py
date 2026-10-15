"""Reconciliation must consume all existing records, not a UI-sized first page.

The real repository SQL assembly runs against a controlled pool, not PostgreSQL.
"""
from __future__ import annotations

from contextlib import asynccontextmanager
from datetime import datetime
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from knowledge_service.persistence.database import DatabaseStorage
from knowledge_service.services.knowledge.confluence.client import ConfluenceAPIError
from knowledge_service.services.knowledge.confluence.sync_service import ConfluenceSyncService


class ReadPool:
    def __init__(self, rows):
        self.rows = rows
        self.calls = []
        self.failure = None

    @asynccontextmanager
    async def acquire(self):
        yield self

    async def fetch(self, query, *params):
        self.calls.append((query, params))
        if self.failure:
            raise self.failure
        assert "WHERE cp.binding_id = $1" in query
        assert "LIMIT $" in query and "OFFSET $" in query
        limit, offset = params[-2:]
        # PostgreSQL LIMIT NULL semantics represented by the test transport.
        selected = [row for row in self.rows if row["binding_id"] == params[0]]
        return selected[offset:] if limit is None else selected[offset:offset + limit]


def fixture_service(count=201, roots=None):
    rows = [{"binding_id": "binding", "page_id": f"p{i:03}",
             "document_id": f"d{i:03}", "content_hash": "same"} for i in range(count)]
    rows.append({"binding_id": "other-binding", "page_id": "foreign", "document_id": "foreign"})
    database = DatabaseStorage.__new__(DatabaseStorage)
    database._pool = ReadPool(rows)
    binding = {"binding_id": "binding", "connection_id": "connection", "dataset_id": "dataset",
               "space_id": "space", "space_key": "SPACE", "root_page_ids": roots or [],
               "max_depth": 1, "last_sync_at": datetime(2026, 1, 1), "status": "syncing"}
    database.get_confluence_binding = AsyncMock(side_effect=lambda _: dict(binding))
    database.get_confluence_page_by_page_id = AsyncMock(side_effect=lambda bid, pid: next(
        (row for row in rows if row["binding_id"] == bid and row["page_id"] == pid), None))
    database.update_confluence_binding = AsyncMock(side_effect=lambda _, values: binding.update(values))
    database.update_confluence_sync_task = AsyncMock()
    database.delete_confluence_page_by_page_id = AsyncMock(return_value=True)
    service = ConfluenceSyncService(
        settings=SimpleNamespace(confluence=SimpleNamespace(client_cache_ttl_seconds=300)),
        database=database, knowledge_service=SimpleNamespace(),
    )
    client = SimpleNamespace(
        get_page=AsyncMock(return_value=SimpleNamespace(content_hash="same")),
        get_page_children=AsyncMock(return_value=[{"id": f"p{i:03}"} for i in range(1, count - 1)]),
        search_pages_modified_since=AsyncMock(return_value=[]),
    )
    service._get_client = AsyncMock(return_value=client)
    service._require_dataset_index_writable = AsyncMock()
    service._delete_bound_document = AsyncMock(return_value=True)
    service._create_document_from_page = AsyncMock(return_value={"document_id": "new"})
    return service, client, database, binding


@pytest.mark.asyncio
@pytest.mark.parametrize("count", [0, 99, 100, 101, 201])
async def test_full_whitelist_reads_every_bound_record(count):
    service, client, database, binding = fixture_service(count)
    result = await service._sync_space_pages("binding", "task")
    assert not result.errors
    assert result.total_pages == count
    assert client.get_page.await_count == count
    assert {call.args[0] for call in client.get_page.await_args_list} == {f"p{i:03}" for i in range(count)}
    assert len(database._pool.calls) == 1
    assert database._pool.calls[0][1][0] == "binding"
    assert binding["status"] == "completed"
    database.delete_confluence_page_by_page_id.assert_not_awaited()


@pytest.mark.asyncio
async def test_full_root_sync_reconciles_missing_record_beyond_first_page():
    service, client, database, binding = fixture_service(201, ["p000"])
    result = await service._sync_space_pages("binding", "task")
    assert not result.errors
    assert result.deleted_documents == ["d200"]
    service._delete_bound_document.assert_awaited_once()
    database.delete_confluence_page_by_page_id.assert_not_awaited()
    assert client.get_page.await_count == 200
    assert binding["status"] == "completed"


@pytest.mark.asyncio
async def test_incremental_checks_tail_records_and_only_removes_verified_404():
    service, client, database, binding = fixture_service()

    async def get_page(page_id):
        code = {"p198": 503, "p199": 403, "p200": 404}.get(page_id)
        if code:
            raise ConfluenceAPIError("fixture unavailable", status_code=code)
        return SimpleNamespace(content_hash="same")

    client.get_page.side_effect = get_page
    result = await service._incremental_sync_pages("binding", "task")
    assert result.errors
    assert client.get_page.await_count == 201
    assert result.deleted_documents == ["d200"]
    service._delete_bound_document.assert_awaited_once()
    database.delete_confluence_page_by_page_id.assert_not_awaited()
    assert database._pool.calls[0][1] == ("binding", None, 0)
    assert binding["status"] == "error"
    assert binding["last_sync_at"] == datetime(2026, 1, 1)


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["_sync_space_pages", "_incremental_sync_pages"])
async def test_existing_record_read_failure_does_not_run_removal(method):
    service, client, database, binding = fixture_service()
    previous_success = binding["last_sync_at"]
    database._pool.failure = RuntimeError("fixture read failed")
    result = await getattr(service, method)("binding", "task")
    assert result.errors
    assert binding["status"] == "error"
    assert binding["last_sync_at"] == previous_success
    service._delete_bound_document.assert_not_awaited()
    database.delete_confluence_page_by_page_id.assert_not_awaited()
    client.get_page.assert_not_awaited()


@pytest.mark.asyncio
async def test_repository_default_page_and_explicit_offset_remain_compatible():
    _, _, database, _ = fixture_service()
    assert len(await database.list_confluence_pages("binding")) == 100
    page = await database.list_confluence_pages("binding", limit=2, offset=199)
    assert [row["page_id"] for row in page] == ["p199", "p200"]
    assert len(await database.list_confluence_pages("binding", limit=None)) == 201
    assert database._pool.calls[0][1] == ("binding", 100, 0)
    assert database._pool.calls[1][1] == ("binding", 2, 199)
    assert database._pool.calls[2][1] == ("binding", None, 0)
