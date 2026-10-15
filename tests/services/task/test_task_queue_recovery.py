"""Actual queue/manager/worker recovery; command/provider boundaries are controlled."""
import asyncio
from unittest.mock import AsyncMock

import pytest

from src.models.request import UnifiedRequest
from src.models.response import UnifiedResponse
from src.models.task import TaskStatus
from src.services.task.task_manager import MemoryTaskStorage, TaskManager
from src.services.task.task_queue import MemoryTaskQueue, RedisTaskQueue
from src.services.task.task_worker import TaskWorker


class RecoveringRedis:
    def __init__(self, fault="transient"):
        self.rows = {}
        self.fault = fault
        self.calls = 0
        self.first_fault = asyncio.Event()
        self.popped = []

    async def zadd(self, key, rows):
        assert key == "gateway:tasks:priority_queue"
        self.rows.update(rows)

    async def zpopmin(self, key, count):
        assert key == "gateway:tasks:priority_queue" and count == 1
        self.calls += 1
        if self.calls == 1 or self.fault == "persistent":
            self.first_fault.set()
            if self.fault == "malformed":
                return [("{bad-json", 0)]
            raise ConnectionError("private backend detail")
        if not self.rows:
            return []
        member = min(self.rows, key=self.rows.get)
        self.popped.append(member)
        return [(member, self.rows.pop(member))]


async def setup(fault="transient"):
    redis = RecoveringRedis(fault)
    queue = RedisTaskQueue(redis)
    storage = MemoryTaskStorage()
    manager = TaskManager(storage, queue)
    request = UnifiedRequest("r", "s", [], user_id="u", tenant_id="t")
    task = await manager.create_task(request, ["user"], None)
    dispatcher = AsyncMock()
    dispatcher.invoke.return_value = UnifiedResponse("r", "success", [])
    worker = TaskWorker(queue, storage, dispatcher, manager)
    return redis, queue, storage, manager, task, dispatcher, worker


async def settled(storage, task_id, status):
    for _ in range(150):
        if (await storage.get(task_id)).status == status:
            return True
        await asyncio.sleep(.01)
    return False


@pytest.mark.parametrize("fault", ["transient", "malformed"])
async def test_recovered_redis_dispatches_remaining_task_once(fault):
    redis, queue, storage, _, task, dispatcher, worker = await setup(fault)
    await worker.start()
    try:
        await asyncio.wait_for(redis.first_fault.wait(), .5)
        completed = await settled(storage, task.task_id, TaskStatus.COMPLETED)
        # Failure specifically means a live lane is stranded on empty fallback,
        # not an arbitrary timeout, crash, lost admission or provider failure.
        if not completed:
            assert redis.calls == 1 and len(redis.rows) == 1
            assert queue.fallback._queue.empty()
            assert len(worker._workers) == 1 and not worker._workers[0].done()
            assert (await storage.get(task.task_id)).status == TaskStatus.PENDING
            dispatcher.invoke.assert_not_awaited()
        assert completed, "live worker stranded on empty fallback after one Redis poll"
        dispatcher.invoke.assert_awaited_once()
        assert len(redis.popped) == 1 and not redis.rows and queue.fallback._queue.empty()
    finally:
        await asyncio.wait_for(worker.stop(), .5)
        assert worker._workers == []


async def test_cancelled_pending_task_does_not_dispatch_after_queue_recovery():
    redis, _, storage, manager, task, dispatcher, worker = await setup()
    await worker.start()
    try:
        await asyncio.wait_for(redis.first_fault.wait(), .5)
        await manager.cancel(task.task_id)
        for _ in range(150):
            if redis.popped:
                break
            await asyncio.sleep(.01)
        assert len(redis.popped) == 1
        assert (await storage.get(task.task_id)).status == TaskStatus.CANCELLED
        dispatcher.invoke.assert_not_awaited()
    finally:
        await asyncio.wait_for(worker.stop(), .5)


async def test_stop_during_outage_joins_without_dispatch_or_requeue():
    redis, queue, storage, _, task, dispatcher, worker = await setup("persistent")
    await worker.start()
    await asyncio.wait_for(redis.first_fault.wait(), .5)
    await asyncio.wait_for(worker.stop(), .5)
    assert worker._workers == []
    assert redis.calls == 1 and len(redis.rows) == 1
    assert (await storage.get(task.task_id)).status == TaskStatus.PENDING
    assert queue.fallback._queue.empty()
    dispatcher.invoke.assert_not_awaited()


async def test_outage_drains_existing_memory_fallback():
    redis = RecoveringRedis("persistent")
    queue = RedisTaskQueue(redis)
    request = UnifiedRequest("r", "s", [], user_id="u", tenant_id="t")
    await queue.fallback.enqueue("fallback-task", request, ["user"], None)
    result = await asyncio.wait_for(queue.dequeue(), .5)
    assert result == ("fallback-task", request, ["user"], None)
    assert redis.calls == 1 and not redis.rows


@pytest.mark.parametrize("redis", [None, object()])
async def test_unconfigured_redis_keeps_direct_fallback(redis):
    queue = RedisTaskQueue(redis)
    request = UnifiedRequest("r", "s", [])
    await queue.fallback.enqueue("task", request, [], None)
    assert (await asyncio.wait_for(queue.dequeue(), .5))[0] == "task"


async def test_custom_fallback_keeps_legacy_ownership():
    fallback = AsyncMock()
    fallback.dequeue.return_value = ("custom", UnifiedRequest("r", "s", []), [], None)
    queue = RedisTaskQueue(RecoveringRedis(), fallback_queue=fallback)
    assert not isinstance(fallback, MemoryTaskQueue)
    assert (await queue.dequeue())[0] == "custom"
    fallback.dequeue.assert_awaited_once()


async def test_lost_pop_ack_does_not_replay_removed_envelope():
    redis, queue, storage, manager, lost, dispatcher, worker = await setup()
    # Insert only the first task before the ambiguous pop, then add a second
    # independent envelope after observing the lost acknowledgement.
    original = redis.zpopmin
    removed = []
    async def lose_first_ack(key, count):
        if not removed:
            member = next(iter(redis.rows))
            removed.append(member)
            redis.rows.pop(member)
            redis.calls += 1
            redis.first_fault.set()
            raise ConnectionError("acknowledgement lost after removal")
        return await original(key, count)
    redis.zpopmin = lose_first_ack
    await worker.start()
    try:
        await asyncio.wait_for(redis.first_fault.wait(), .5)
        next_task = await manager.create_task(
            UnifiedRequest("next-r", "s", [], user_id="u", tenant_id="t"), ["user"], None,
        )
        assert await settled(storage, next_task.task_id, TaskStatus.COMPLETED)
        assert (await storage.get(lost.task_id)).status == TaskStatus.PENDING
        dispatcher.invoke.assert_awaited_once()
        assert dispatcher.invoke.call_args.args[0].request_id == "next-r"
        assert len(redis.popped) == 1 and redis.popped[0] != removed[0]
        assert not redis.rows and queue.fallback._queue.empty()
    finally:
        await asyncio.wait_for(worker.stop(), .5)
