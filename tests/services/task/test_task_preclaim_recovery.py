"""Actual manager/worker methods; no live database/provider or crash durability."""
import asyncio
import ssl
from unittest.mock import AsyncMock

import pytest
from ai_gateway_core.exceptions import TaskNotFoundError
from asyncpg.exceptions import (
    CannotConnectNowError,
    ConnectionDoesNotExistError,
    ConnectionFailureError,
    ConnectionRejectionError,
    InsufficientPrivilegeError,
    ProtocolViolationError,
    TooManyConnectionsError,
)

from src.models.request import UnifiedRequest
from src.models.response import UnifiedResponse
from src.models.task import TaskStatus
from src.services.task.task_manager import MemoryTaskStorage, TaskManager, TaskStateConflictError
from src.services.task.task_queue import MemoryTaskQueue
from src.services.task.task_worker import TaskWorker


async def setup():
    storage, queue = MemoryTaskStorage(), MemoryTaskQueue()
    manager = TaskManager(storage, queue)
    request = UnifiedRequest("r", "s", [], user_id="u", tenant_id="t")
    task = await manager.create_task(request, ["user"], "fixture")
    dispatcher = AsyncMock()
    dispatcher.invoke.return_value = UnifiedResponse("r", "success", [])
    worker = TaskWorker(queue, storage, dispatcher, manager)
    return storage, queue, manager, task, dispatcher, worker


@pytest.mark.parametrize("error", [ConnectionError, TimeoutError, ConnectionDoesNotExistError, ConnectionFailureError, CannotConnectNowError, TooManyConnectionsError])
async def test_read_outage_retains_envelope_paces_and_invokes_once(error):
    storage, queue, manager, task, dispatcher, worker = await setup()
    original = storage.get
    storage.get = AsyncMock(side_effect=[error("private backend detail"), await original(task.task_id)])
    processing = asyncio.create_task(worker._process(*await queue.dequeue()))
    try:
        await asyncio.sleep(.05)
        assert storage.get.await_count == 1
        assert queue._queue.empty()
        dispatcher.invoke.assert_not_awaited()
        await asyncio.wait_for(processing, 1.5)
        assert storage.get.await_count == 2
        dispatcher.invoke.assert_awaited_once()
        assert (await original(task.task_id)).status == TaskStatus.COMPLETED
    finally:
        processing.cancel()
        await asyncio.gather(processing, return_exceptions=True)


@pytest.mark.parametrize("boundary", ["read", "backoff"])
async def test_stop_returns_definitely_unclaimed_envelope_and_restart_completes(boundary):
    storage, queue, manager, task, dispatcher, worker = await setup()
    original = storage.get
    entered = asyncio.Event()
    async def unavailable(task_id):
        entered.set()
        if boundary == "read":
            await asyncio.Event().wait()
        raise ConnectionError("unavailable")
    storage.get = unavailable
    await worker.start()
    await asyncio.wait_for(entered.wait(), .5)
    await worker.stop()
    assert queue._queue.qsize() == 1
    assert (await original(task.task_id)).status == TaskStatus.PENDING
    dispatcher.invoke.assert_not_awaited()
    storage.get = original
    completed = asyncio.Event()
    async def invoke(*args, **kwargs):
        completed.set()
        return UnifiedResponse("r", "success", [])
    dispatcher.invoke.side_effect = invoke
    await worker.start()
    try:
        await asyncio.wait_for(completed.wait(), .5)
        for _ in range(20):
            if (await original(task.task_id)).status == TaskStatus.COMPLETED:
                break
            await asyncio.sleep(.01)
        assert (await original(task.task_id)).status == TaskStatus.COMPLETED
        dispatcher.invoke.assert_awaited_once()
    finally:
        await worker.stop()


@pytest.mark.parametrize("commit", [False, True])
async def test_uncertain_claim_never_retries_or_requeues(commit):
    storage, queue, manager, task, dispatcher, worker = await setup()
    original = storage.compare_status
    async def uncertain(expected, status, **kwargs):
        if commit:
            await original(expected, status, **kwargs)
        raise OSError("claim ACK lost")
    storage.compare_status = AsyncMock(side_effect=uncertain)
    with pytest.raises(OSError):
        await worker._process(*await queue.dequeue())
    assert storage.compare_status.await_count == 1
    assert queue._queue.empty()
    dispatcher.invoke.assert_not_awaited()
    assert (await storage.get(task.task_id)).status == (TaskStatus.PROCESSING if commit else TaskStatus.PENDING)


@pytest.mark.parametrize("commit", [False, True])
async def test_cancellation_inside_claim_write_never_requeues(commit):
    storage, queue, manager, task, dispatcher, worker = await setup()
    original = storage.compare_status
    entered = asyncio.Event()
    async def held(expected, status, **kwargs):
        if commit:
            await original(expected, status, **kwargs)
        entered.set()
        await asyncio.Event().wait()
    storage.compare_status = held
    await worker.start()
    await asyncio.wait_for(entered.wait(), .5)
    await worker.stop()
    assert queue._queue.empty()
    dispatcher.invoke.assert_not_awaited()


@pytest.mark.parametrize("error", [OSError("unknown I/O"), PermissionError("denied"), FileNotFoundError("configuration"),
    ssl.SSLCertVerificationError("certificate"), ProtocolViolationError("protocol"),
    ConnectionRejectionError("rejected"), ValueError("malformed"), InsufficientPrivilegeError("denied"), TaskNotFoundError("missing"), RuntimeError("configuration unavailable")])
async def test_nontransient_read_errors_do_not_retry(error):
    storage, queue, manager, task, dispatcher, worker = await setup()
    storage.get = AsyncMock(side_effect=error)
    with pytest.raises(type(error)):
        await asyncio.wait_for(worker._process(*await queue.dequeue()), .5)
    assert storage.get.await_count == 1
    assert queue._queue.empty()
    dispatcher.invoke.assert_not_awaited()


@pytest.mark.parametrize("winner", ["cancel", "duplicate", "identity"])
async def test_authority_change_while_waiting_never_replays(winner):
    storage, queue, manager, task, dispatcher, worker = await setup()
    original = storage.get
    reads = 0
    async def flaky(task_id):
        nonlocal reads
        reads += 1
        if reads == 1:
            raise ConnectionError("unavailable")
        return await original(task_id)
    storage.get = flaky
    processing = asyncio.create_task(worker._process(*await queue.dequeue()))
    await asyncio.sleep(.05)
    if winner == "cancel":
        await manager.cancel(task.task_id)
    elif winner == "duplicate":
        await worker._process(task.task_id, UnifiedRequest("r", "s", [], user_id="u", tenant_id="t"), [], None)
    else:
        replaced = await original(task.task_id)
        replaced.tenant_id = "other"
        await storage.save(replaced)
    if winner == "identity":
        with pytest.raises(TaskStateConflictError):
            await asyncio.wait_for(processing, 1.5)
    else:
        await asyncio.wait_for(processing, 1.5)
    assert dispatcher.invoke.await_count == (1 if winner == "duplicate" else 0)
