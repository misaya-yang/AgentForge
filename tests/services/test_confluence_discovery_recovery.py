"""Incomplete source discovery must never become destructive reconciliation.

Production client and sync coordinator, with in-memory repository and HTTP doubles.
These are not public-route, real-provider or database acceptance tests.
"""
from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from knowledge_service.services.knowledge.confluence.client import (
    ConfluenceAPIError,
    ConfluenceClient,
)
from knowledge_service.services.knowledge.confluence.models import ConfluenceCredentials
from knowledge_service.services.knowledge.confluence.sync_service import ConfluenceSyncService


def make_client() -> ConfluenceClient:
    return ConfluenceClient(ConfluenceCredentials(
        domain="fixture.atlassian.net", email="fixture@example.com", api_token="fixture-only",
    ))


def make_service(roots: list[str] | None = None):
    binding = {
        "binding_id": "binding", "connection_id": "connection", "dataset_id": "dataset",
        "space_id": "space", "root_page_ids": roots if roots is not None else ["root"],
        "max_depth": 3, "status": "syncing", "last_sync_at": "previous-success",
    }
    records = [{"page_id": value, "document_id": f"doc-{value}", "content_hash": "same"}
               for value in ["root", "child", "old"]]
    database = SimpleNamespace(
        get_confluence_binding=AsyncMock(side_effect=lambda _: dict(binding)),
        list_confluence_pages=AsyncMock(side_effect=lambda _, **_kwargs: list(records)),
        get_confluence_page_by_page_id=AsyncMock(side_effect=lambda _, pid: next(
            (p for p in records if p["page_id"] == pid), None)),
        update_confluence_binding=AsyncMock(side_effect=lambda _, values: binding.update(values)),
        update_confluence_sync_task=AsyncMock(),
        delete_confluence_page_by_page_id=AsyncMock(return_value=True),
    )
    service = ConfluenceSyncService(
        settings=SimpleNamespace(confluence=SimpleNamespace(client_cache_ttl_seconds=300)),
        database=database, knowledge_service=SimpleNamespace(),
    )
    client = make_client()
    client.get_page = AsyncMock(return_value=SimpleNamespace(content_hash="same"))
    service._get_client = AsyncMock(return_value=client)
    service._require_dataset_index_writable = AsyncMock()
    service._delete_bound_document = AsyncMock(return_value=True)
    service._create_document_from_page = AsyncMock(return_value={"document_id": "new"})
    return service, client, database, binding


def assert_failed_without_reconciliation(service, client, database, binding, result):
    assert binding["status"] == "error"
    assert binding["last_sync_at"] == "previous-success"
    assert result.errors
    assert database.update_confluence_sync_task.await_args.args[1]["status"] == "failed"
    service._delete_bound_document.assert_not_awaited()
    database.delete_confluence_page_by_page_id.assert_not_awaited()
    service._create_document_from_page.assert_not_awaited()
    client.get_page.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("failed_path,roots", [
    ("/pages/root/children", ["root"]),
    ("/pages/child/children", ["root"]),
    ("/pages/second/children", ["root", "second"]),
])
async def test_failed_root_or_nested_read_stops_before_reconciliation(failed_path, roots):
    service, client, database, binding = make_service(roots)

    async def request(_method, path, params=None):
        if path == failed_path:
            raise ConfluenceAPIError("fixture read failed", status_code=503)
        return {"results": [{"id": "child"}] if path == "/pages/root/children" else []}

    client._request = AsyncMock(side_effect=request)
    result = await service._sync_space_pages("binding", "task")
    assert_failed_without_reconciliation(service, client, database, binding, result)


@pytest.mark.asyncio
@pytest.mark.parametrize("response", [
    {}, {"results": None}, {"results": {}}, {"results": [None]},
    {"results": [{}]}, {"results": [{"id": ""}]}, {"results": [{"id": False}]},
    {"results": [], "_links": {"next": "/children?limit=100"}},
])
async def test_malformed_discovery_is_not_an_empty_subtree(response):
    service, client, database, binding = make_service()
    client._request = AsyncMock(return_value=response)
    result = await service._sync_space_pages("binding", "task")
    assert_failed_without_reconciliation(service, client, database, binding, result)


@pytest.mark.asyncio
async def test_late_pagination_read_failure_preserves_all_existing_pages():
    service, client, database, binding = make_service()
    client._request = AsyncMock(side_effect=[
        {"results": [{"id": "child"}], "_links": {"next": "/children?cursor=next"}},
        ConfluenceAPIError("later page failed", status_code=503),
    ])
    result = await service._sync_space_pages("binding", "task")
    assert_failed_without_reconciliation(service, client, database, binding, result)


@pytest.mark.asyncio
async def test_repaired_read_completes_and_keeps_complete_sync_deletion_semantics():
    service, client, database, binding = make_service()
    client._request = AsyncMock(side_effect=ConfluenceAPIError("temporary read failure"))
    failed = await service._sync_space_pages("binding", "failed-task")
    assert_failed_without_reconciliation(service, client, database, binding, failed)

    async def repaired(_method, path, params=None):
        return {"results": [{"id": "child"}] if path == "/pages/root/children" else []}

    client._request = AsyncMock(side_effect=repaired)
    result = await service._sync_space_pages("binding", "recovery-task")
    assert not result.errors
    assert binding["status"] == "completed"
    assert binding["last_sync_at"] != "previous-success"
    assert result.deleted_documents == ["doc-old"]
    service._delete_bound_document.assert_awaited_once()
    database.delete_confluence_page_by_page_id.assert_not_awaited()
    assert database.update_confluence_sync_task.await_args.args[1]["status"] == "completed"


@pytest.mark.asyncio
async def test_confirmed_empty_subtree_still_deletes_removed_descendants():
    service, client, database, binding = make_service()
    client._request = AsyncMock(return_value={"results": []})
    result = await service._sync_space_pages("binding", "task")
    assert binding["status"] == "completed"
    assert set(result.deleted_documents) == {"doc-child", "doc-old"}
    assert service._delete_bound_document.await_count == 2
    database.delete_confluence_page_by_page_id.assert_not_awaited()
    client.get_page.assert_awaited_once_with("root")


@pytest.mark.asyncio
async def test_short_and_empty_pages_with_next_are_not_end_of_discovery():
    client = make_client()
    client._request = AsyncMock(side_effect=[
        {"results": [{"id": "first"}], "_links": {"next": "/children?cursor=one"}},
        {"results": [], "_links": {"next": "/children?cursor=two"}},
        {"results": [{"id": "last"}]},
    ])
    assert await client.get_page_children("root") == [{"id": "first"}, {"id": "last"}]
    assert [c.kwargs["params"].get("cursor") for c in client._request.await_args_list] == [None, "one", "two"]


@pytest.mark.asyncio
@pytest.mark.parametrize("cursors", [["one", "one"], ["one", "two", "one"]])
async def test_cursor_cycle_fails_instead_of_hanging_or_claiming_complete(cursors):
    client = make_client()
    client._request = AsyncMock(side_effect=[
        *[{"results": [{"id": "child"}], "_links": {"next": f"/children?cursor={cursor}"}}
          for cursor in cursors],
        AssertionError("cursor loop must have been rejected"),
    ])
    with pytest.raises(ConfluenceAPIError, match="pagination"):
        await client.get_page_children("root", limit=1)
    assert client._request.await_count == len(cursors)


@pytest.mark.asyncio
async def test_whitelist_sync_does_not_discover_or_delete_unseen_pages():
    service, client, database, binding = make_service([])
    client._request = AsyncMock(side_effect=AssertionError("whitelist must not discover"))
    result = await service._sync_space_pages("binding", "task")
    assert not result.errors
    assert binding["status"] == "completed"
    client._request.assert_not_awaited()
    service._delete_bound_document.assert_not_awaited()
    database.delete_confluence_page_by_page_id.assert_not_awaited()
