"""Cooperative local cancellation, authority and cleanup; no external I/O."""
import asyncio
from copy import deepcopy
from unittest.mock import AsyncMock

import pytest

from src.models.request import UnifiedRequest
from src.models.response import UnifiedResponse
from src.models.task import TaskStatus
from src.services.task.task_manager import MemoryTaskStorage, TaskManager
from src.services.task.task_queue import MemoryTaskQueue
from src.services.task.task_worker import TaskWorker


async def fixture():
    storage, queue = MemoryTaskStorage(), MemoryTaskQueue()
    manager = TaskManager(storage, queue)
    request = UnifiedRequest(request_id="r", service_id="s", inputs=[], user_id="u", tenant_id="t")
    task = await manager.create_task(request, [], None)
    dispatcher = AsyncMock()
    worker = TaskWorker(queue, storage, dispatcher, manager)
    entered, stopped, release = asyncio.Event(), asyncio.Event(), asyncio.Event()
    async def invoke(*args, **kwargs):
        entered.set()
        try:
            await release.wait()
            return UnifiedResponse("r", "success", [])
        finally:
            stopped.set()
    dispatcher.invoke.side_effect = invoke
    return storage, manager, request, task, worker, dispatcher, entered, stopped, release


@pytest.mark.parametrize("loss", ["cancel", "identity", "completed", "read_error"])
async def test_observed_authority_loss_stops_owned_work_without_callback(loss):
    storage, manager, request, task, worker, dispatcher, entered, stopped, release = await fixture()
    manager._send_callback = AsyncMock()
    processing = asyncio.create_task(worker._process(task.task_id, request, [], None))
    await entered.wait()
    if loss == "cancel":
        await manager.cancel(task.task_id)
    elif loss == "read_error":
        storage.get_authority = AsyncMock(side_effect=OSError("private authority error"))
    else:
        row = await storage.get(task.task_id)
        if loss == "identity":
            row.tenant_id = "replacement"
        else:
            row.status, row.result = TaskStatus.COMPLETED, {"winner": True}
        await storage.save(row)
    if loss == "read_error":
        with pytest.raises(RuntimeError, match="authority unavailable"):
            await asyncio.wait_for(processing, 1.6)
        row = storage._tasks[task.task_id]
        assert row.status == TaskStatus.PROCESSING and row.error is None
    else:
        await asyncio.wait_for(processing, 1.6)
        row = await storage.get(task.task_id)
        assert row.status == {"cancel": TaskStatus.CANCELLED, "identity": TaskStatus.PROCESSING,
                              "completed": TaskStatus.COMPLETED}[loss]
    assert stopped.is_set() and not release.is_set()
    dispatcher.invoke.assert_awaited_once()
    manager._send_callback.assert_not_awaited()


async def test_authority_observer_uses_claim_storage_not_legacy_constructor_storage():
    storage, manager, request, task, worker, dispatcher, entered, stopped, release = await fixture()
    worker.storage = AsyncMock()
    processing = asyncio.create_task(worker._process(task.task_id, request, [], None))
    await entered.wait()
    await manager.cancel(task.task_id)
    await asyncio.wait_for(processing, 1.6)
    assert stopped.is_set()
    worker.storage.get_authority.assert_not_awaited()


async def test_finished_invocation_cancels_stalled_authority_read_before_commit():
    storage, manager, request, task, worker, dispatcher, entered, stopped, release = await fixture()
    original = storage.get_authority
    reading, read_stopped = asyncio.Event(), asyncio.Event()
    calls = 0
    async def get(task_id):
        nonlocal calls
        calls += 1
        if calls >= 2:  # initial read, then first periodic read
            reading.set()
            try:
                await asyncio.Event().wait()
            finally:
                read_stopped.set()
        return await original(task_id)
    storage.get_authority = get
    processing = asyncio.create_task(worker._process(task.task_id, request, [], None))
    await asyncio.wait_for(reading.wait(), 1.6)
    release.set()
    await asyncio.wait_for(processing, .5)
    assert read_stopped.is_set()
    assert (await storage.get(task.task_id)).status == TaskStatus.COMPLETED


async def test_read_cost_bounded_and_no_observer_survives_into_callback():
    storage, manager, request, task, worker, dispatcher, entered, stopped, release = await fixture()
    task.callback_url = "https://callback.invalid"
    await storage.save(task)
    original = storage.get_authority
    storage.get_authority = AsyncMock(side_effect=original)
    callback_entered, callback_release = asyncio.Event(), asyncio.Event()
    async def callback(receipt):
        callback_entered.set()
        await callback_release.wait()
    manager._send_callback = AsyncMock(side_effect=callback)
    processing = asyncio.create_task(worker._process(task.task_id, request, [], None))
    await entered.wait()
    await asyncio.sleep(2.1)
    assert 2 <= storage.get_authority.await_count <= 3  # initial + at most two polls
    release.set()
    await callback_entered.wait()
    reads = storage.get_authority.await_count
    await asyncio.sleep(1.1)
    assert storage.get_authority.await_count == reads
    callback_release.set()
    await processing
    manager._send_callback.assert_awaited_once()


async def test_shutdown_retains_cleanup_across_concurrent_stop_and_caller_cancel():
    storage, manager, request, task, worker, dispatcher, entered, stopped, release = await fixture()
    cleanup_entered, cleanup_release = asyncio.Event(), asyncio.Event()
    async def invoke(*args, **kwargs):
        entered.set()
        try:
            await asyncio.Event().wait()
        finally:
            cleanup_entered.set()
            await cleanup_release.wait()
            stopped.set()
    dispatcher.invoke.side_effect = invoke
    await worker.start()
    await entered.wait()
    stopping = asyncio.create_task(worker.stop())
    await cleanup_entered.wait()
    stopping.cancel()
    other = asyncio.create_task(worker.stop())
    await asyncio.sleep(0)
    assert not stopping.done() and not other.done() and worker._workers
    await worker.start()  # cannot replace the still-cleaning lane
    assert len(worker._workers) == 1
    cleanup_release.set()
    with pytest.raises(asyncio.CancelledError):
        await stopping
    await other
    assert stopped.is_set() and worker._workers == []
    assert worker._stop_task.done()
    await worker.stop()


async def test_independent_adapter_cancellation_keeps_lane_usable_without_replay():
    storage, manager, request, task, worker, dispatcher, entered, stopped, release = await fixture()
    second = deepcopy(request)
    second.request_id = "r2"
    second.priority = request.priority - 1  # deterministic priority-queue order
    second_task = await manager.create_task(second, [], None)
    calls = 0
    async def invoke(*args, **kwargs):
        nonlocal calls
        calls += 1
        if calls == 1:
            raise asyncio.CancelledError
        worker._running = False
        return UnifiedResponse("r2", "success", [])
    dispatcher.invoke.side_effect = invoke
    worker._running = True
    await worker._run()
    assert calls == 2
    assert (await storage.get(task.task_id)).status == TaskStatus.PROCESSING
    assert (await storage.get(second_task.task_id)).status == TaskStatus.COMPLETED


async def test_cancel_after_claim_before_invoke_never_starts_adapter():
    storage, manager, request, task, worker, dispatcher, entered, stopped, release = await fixture()
    claim = manager.claim_task
    async def claim_then_cancel(*args):
        claimed = await claim(*args)
        await manager.cancel(task.task_id)
        return claimed
    manager.claim_task = claim_then_cancel
    await worker._process(task.task_id, request, [], None)
    dispatcher.invoke.assert_not_awaited()
    assert (await storage.get(task.task_id)).status == TaskStatus.CANCELLED


async def test_old_stop_caller_cannot_clear_restarted_worker_generation(monkeypatch):
    import src.services.task.task_worker as module
    real_finish = module._finish_cleanup
    second_waiting, release_second = asyncio.Event(), asyncio.Event()
    count = 0
    async def finish(task):
        nonlocal count
        count += 1
        mine = count
        await real_finish(task)
        if mine == 2:
            second_waiting.set()
            await release_second.wait()
    monkeypatch.setattr(module, "_finish_cleanup", finish)
    _, _, _, _, worker, _, _, _, _ = await fixture()
    async def lane():
        await asyncio.Event().wait()
    monkeypatch.setattr(worker, "_run", lane)
    await worker.start()
    first, second = asyncio.create_task(worker.stop()), asyncio.create_task(worker.stop())
    await second_waiting.wait()
    await first
    await worker.start()
    fresh = list(worker._workers)
    release_second.set()
    await second
    assert worker._workers == fresh and worker._running
    await worker.stop()
    assert all(task.done() for task in fresh)
