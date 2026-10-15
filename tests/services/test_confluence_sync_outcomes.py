"""Production coordinator terminal facts using controlled provider/repository I/O."""
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from knowledge_service.services.knowledge.confluence.client import ConfluenceAPIError
from knowledge_service.services.knowledge.confluence.models import SyncResult

from tests.services.test_confluence_record_completeness import fixture_service


def assert_failed(database, binding, result, previous):
    assert result.errors
    assert result.completed_at is not None
    assert result.task_id == 'task'
    assert binding["status"] == "error"
    assert binding["last_sync_at"] == previous
    final = database.update_confluence_sync_task.await_args.args[1]
    assert final["status"] == "failed"
    assert final["result"]["errors"] == result.errors
    assert final["result"]["synced_pages"] == result.synced_pages


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["_sync_space_pages", "_incremental_sync_pages"])
async def test_partial_update_failure_preserves_success_watermark_and_progress(method):
    service, client, database, binding = fixture_service(2)
    previous = binding["last_sync_at"]
    client.search_pages_modified_since.return_value = [SimpleNamespace(page_id='p000'), SimpleNamespace(page_id='p001')]
    client.get_page.side_effect = lambda page_id: SimpleNamespace(page_id=page_id, content_hash='changed')
    async def prepare(**values):
        if values['document_id'] == 'd001':
            raise RuntimeError('fixture update failure')
        return 0
    service._prepare_existing_page_update = AsyncMock(side_effect=prepare)
    result = await getattr(service, method)('binding', 'task')
    assert_failed(database, binding, result, previous)
    assert result.synced_pages == 1 and result.failed_pages == 1
    assert result.updated_documents == ['d000']
    assert any(error.get('error') == 'fixture update failure' for error in result.errors)


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["_sync_space_pages", "_incremental_sync_pages"])
async def test_removal_failure_is_not_completed(method):
    service, client, database, binding = fixture_service(2, ['p000'])
    previous = binding['last_sync_at']
    if method == '_incremental_sync_pages':
        client.get_page.side_effect = ConfluenceAPIError('gone', status_code=404)
    service._delete_bound_document.side_effect = RuntimeError('fixture removal failure')
    result = await getattr(service, method)('binding', 'task')
    assert_failed(database, binding, result, previous)
    database.delete_confluence_page_by_page_id.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("code", [403, 429, 503, None])
async def test_unverified_incremental_source_does_not_advance_watermark(code):
    service, client, database, binding = fixture_service(1)
    previous = binding['last_sync_at']
    client.get_page.side_effect = (ConfluenceAPIError('unavailable', status_code=code)
                                 if code is not None else TimeoutError('fixture timeout'))
    result = await service._incremental_sync_pages('binding', 'task')
    assert_failed(database, binding, result, previous)
    service._delete_bound_document.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("method,count", [("_sync_space_pages", 0), ("_sync_space_pages", 2), ("_incremental_sync_pages", 2)])
async def test_success_clears_previous_issue(method, count):
    service, client, database, binding = fixture_service(count)
    binding['last_error'] = 'earlier problem'
    result = await getattr(service, method)('binding', 'task')
    assert not result.errors
    assert binding['status'] == 'completed'
    assert binding['last_error'] is None


@pytest.mark.asyncio
async def test_no_watermark_fallback_never_precompletes_and_returns_actual_outcome():
    service, client, database, binding = fixture_service(0)
    binding['last_sync_at'] = None
    outcome = SyncResult(errors=[{'error': 'full sync failed'}])
    service._sync_space_pages = AsyncMock(return_value=outcome)
    result = await service._incremental_sync_pages('binding', 'task')
    assert result is outcome
    service._sync_space_pages.assert_awaited_once_with('binding', 'task')
    database.update_confluence_sync_task.assert_not_awaited()


@pytest.mark.asyncio
async def test_explicit_retry_can_recover_without_repeating_success_as_new_document():
    service, client, database, binding = fixture_service(1)
    client.get_page.side_effect = RuntimeError('temporary failure')
    failed = await service._sync_space_pages('binding', 'failed-task')
    assert failed.errors and binding['status'] == 'error'
    client.get_page.side_effect = None
    client.get_page.return_value = SimpleNamespace(content_hash='same')
    recovered = await service._sync_space_pages('binding', 'retry-task')
    assert not recovered.errors and recovered.skipped_pages == 1
    assert binding['status'] == 'completed' and binding['last_error'] is None
    service._create_document_from_page.assert_not_awaited()


@pytest.mark.asyncio
async def test_terminal_persistence_failure_remains_nonatomic_characterization():
    service, client, database, binding = fixture_service(1)
    previous = binding['last_sync_at']
    async def persist(_task_id, values):
        if values.get('status') == 'completed':
            raise RuntimeError('fixture terminal write failed')
    database.update_confluence_sync_task.side_effect = persist
    result = await service._sync_space_pages('binding', 'task')
    assert result.errors and binding['status'] == 'error'
    # Explicit bounded limitation: success watermark commits before task terminal.
    assert binding['last_sync_at'] != previous
    assert database.update_confluence_sync_task.await_args.args[1]['status'] == 'failed'


@pytest.mark.asyncio
async def test_actual_no_watermark_fallback_retains_original_task_identity():
    service, client, database, binding = fixture_service(0)
    binding['last_sync_at'] = None
    result = await service._incremental_sync_pages('binding', 'original-task')
    assert result.task_id == 'original-task'
    assert database.update_confluence_sync_task.await_args.args[0] == 'original-task'
