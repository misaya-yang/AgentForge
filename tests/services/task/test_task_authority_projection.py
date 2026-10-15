"""Lifecycle-only read equivalence; real adapters with in-process SQL recording."""
import asyncio
from dataclasses import FrozenInstanceError
from unittest.mock import AsyncMock

import pytest

from src.models.task import Task, TaskStatus
from src.services.task.task_manager import MemoryTaskStorage, TaskManager
from src.services.task.task_queue import MemoryTaskQueue
from src.services.task.task_worker import TaskWorker, _TaskObservationError
from tests.services.task.test_database_task_transitions import fixture


async def storages():
    memory = MemoryTaskStorage()
    await memory.save(Task('id', 'r', 's', TaskStatus.PROCESSING, user_id='u', tenant_id='t'))
    pool, database, adapter = fixture()
    pool.row['status'] = 'processing'
    original = pool.fetchrow
    async def fetchrow(sql, *params):
        row = await original(sql, *params)
        if sql.startswith('SELECT task_id, request_id, service_id, user_id, tenant_id, status FROM'):
            return {key: row[key] for key in ('task_id', 'request_id', 'service_id', 'user_id', 'tenant_id', 'status')} if row else None
        return row
    pool.fetchrow = fetchrow
    return memory, pool, database, adapter


@pytest.mark.parametrize('status', list(TaskStatus))
async def test_projection_matches_full_status_and_detaches(status):
    memory, pool, _, adapter = await storages()
    memory._tasks['id'].status = status
    pool.row['status'] = status.value
    for storage in (memory, adapter):
        full, authority = await storage.get('id'), await storage.get_authority('id')
        assert authority.identity == full.identity and authority.status == full.status
        with pytest.raises(FrozenInstanceError):
            authority.status = TaskStatus.PENDING
    assert pool.calls[-1][0] == 'SELECT task_id, request_id, service_id, user_id, tenant_id, status FROM tasks WHERE task_id = $1'
    assert pool.calls[-1][1] == ('id',)
    assert len(pool.calls) == 2
    adapter.redis.get.assert_not_awaited()


@pytest.mark.parametrize('field', ['task_id', 'request_id', 'service_id', 'user_id', 'tenant_id'])
@pytest.mark.parametrize('value', ['replacement', '', None])
async def test_every_identity_component_matches_full_rejection(field, value):
    memory, pool, _, adapter = await storages()
    for storage in (memory, adapter):
        claimed = await storage.get('id')
        if storage is memory:
            setattr(memory._tasks['id'], field, value)
        else:
            pool.row[field] = value
        worker = TaskWorker(MemoryTaskQueue(), storage, AsyncMock(), TaskManager(storage, MemoryTaskQueue()))
        if field == 'task_id' and storage is adapter:
            with pytest.raises(_TaskObservationError):
                await worker._still_processing(claimed)
        else:
            current = await storage.get('id')
            assert (current.identity == claimed.identity and current.status == TaskStatus.PROCESSING) is False
            assert await worker._still_processing(claimed) is False


@pytest.mark.parametrize('failure', ['missing', 'error', 'invalid_status', 'disabled'])
async def test_missing_or_uncertain_authority_never_invokes_or_transitions(failure):
    memory, pool, database, adapter = await storages()
    for storage in (memory, adapter):
        claimed = await storage.get('id')
        manager = TaskManager(storage, MemoryTaskQueue())
        manager.transition_task = AsyncMock()
        dispatcher = AsyncMock()
        worker = TaskWorker(manager.queue, storage, dispatcher, manager)
        if failure == 'missing':
            if storage is memory:
                memory._tasks.clear()
            else:
                pool.row['task_id'] = 'missing'
        elif failure == 'error':
            storage.get_authority = AsyncMock(side_effect=OSError('private backend error'))
        elif failure == 'invalid_status':
            if storage is memory:
                memory._tasks['id'].status = 'invalid'
            else:
                pool.row['status'] = 'invalid'
        elif storage is adapter:
            database.enabled = False
        else:
            continue
        with pytest.raises(_TaskObservationError, match='authority unavailable'):
            await worker._invoke_while_owned(claimed, None, [], None)
        dispatcher.invoke.assert_not_awaited()
        manager.transition_task.assert_not_awaited()


async def test_memory_observation_holds_lock_without_copying_payload():
    memory, _, _, _ = await storages()
    class NoCopy:
        def __deepcopy__(self, memo):
            raise AssertionError('payload copied')
    memory._tasks['id'].request_data = NoCopy()
    async with memory._lock:
        read = asyncio.create_task(memory.get_authority('id'))
        await asyncio.sleep(0)
        assert not read.done()
        memory._tasks['id'].status = TaskStatus.CANCELLED
    assert (await read).status == TaskStatus.CANCELLED
    with pytest.raises(AssertionError, match='payload copied'):
        await memory.get('id')


async def test_observation_does_not_decode_payload_but_full_reads_still_fail_closed():
    _, pool, _, adapter = await storages()
    pool.row['metadata'] = '{broken-json'
    with pytest.raises(ValueError, match='invalid task storage JSON'):
        await adapter.get('id')
    assert (await adapter.get_authority('id')).status == TaskStatus.PROCESSING


async def test_legacy_storage_subclass_uses_safe_default_get_projection():
    from src.services.task.task_manager import TaskStorage
    class LegacyStorage(TaskStorage):
        async def get(self, task_id):
            return Task(task_id, 'r', 's', TaskStatus.PROCESSING, user_id='u', tenant_id='t')
    storage = LegacyStorage()
    authority = await storage.get_authority('id')
    assert authority.identity == ('id', 'r', 's', 'u', 't')
    assert authority.status == TaskStatus.PROCESSING
    storage.get = AsyncMock(side_effect=OSError('unavailable'))
    with pytest.raises(OSError):
        await storage.get_authority('id')
