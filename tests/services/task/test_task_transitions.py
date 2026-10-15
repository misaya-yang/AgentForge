"""Deterministic lifecycle races using the actual memory authority and worker."""
from __future__ import annotations

import asyncio
from copy import deepcopy
from unittest.mock import AsyncMock

import pytest

from src.models.request import UnifiedRequest
from src.models.response import UnifiedResponse
from src.models.task import TaskStatus
from src.services.task.task_manager import MemoryTaskStorage, TaskManager, TaskStateConflictError
from src.services.task.task_queue import MemoryTaskQueue
from src.services.task.task_worker import TaskWorker


def fixture(storage=None):
    storage = storage or MemoryTaskStorage()
    queue = MemoryTaskQueue()
    manager = TaskManager(storage, queue)
    request = UnifiedRequest(request_id="r", service_id="s", inputs=[], user_id="u", tenant_id="t")
    dispatcher = AsyncMock()
    dispatcher.invoke.return_value = UnifiedResponse(request_id="r", status="success", outputs=[])
    worker = TaskWorker(queue, storage, dispatcher, manager)
    return storage, manager, request, dispatcher, worker


async def admitted(storage=None):
    storage, manager, request, dispatcher, worker = fixture(storage)
    task = await manager.create_task(request, [], None)
    return storage, manager, request, dispatcher, worker, task


@pytest.mark.parametrize("before", list(TaskStatus))
@pytest.mark.parametrize("after", list(TaskStatus))
async def test_all_edges(before, after):
    storage, manager, _, _, _, task = await admitted()
    task.status = before
    await storage.save(task)
    allowed = (before, after) in {
        (TaskStatus.PENDING, TaskStatus.PROCESSING), (TaskStatus.PENDING, TaskStatus.CANCELLED),
        (TaskStatus.PROCESSING, TaskStatus.COMPLETED), (TaskStatus.PROCESSING, TaskStatus.FAILED),
        (TaskStatus.PROCESSING, TaskStatus.CANCELLED),
    }
    result = await manager.transition_task(task, after)
    assert (result is not None) == allowed
    row = await storage.get(task.task_id)
    assert row.status == (after if allowed else before)
    if allowed:
        assert row.updated_at is not None
        assert (row.started_at is not None) == (after == TaskStatus.PROCESSING)
        assert (row.completed_at is not None) == (after != TaskStatus.PROCESSING)
    else:
        assert row == task
        with pytest.raises(TaskStateConflictError):
            await storage.compare_status(task, after)


async def test_memory_all_boundaries_are_detached_and_patch_preserves_current_fields():
    storage, manager, _, _, _, task = await admitted()
    task.metadata = {"nested": [1]}
    task.result = {"nested": [2]}
    await storage.save(task)
    original = deepcopy(task)
    task.metadata["nested"].append(99)
    task.result["nested"].append(99)
    fetched = await storage.get(task.task_id)
    fetched.metadata["nested"].append(88)
    fetched.result["nested"].append(88)
    listed = (await storage.list())[0]
    listed.metadata["nested"].append(77)
    listed.result["nested"].append(77)
    assert await storage.get(task.task_id) == original
    current = await storage.get(task.task_id)
    current.metadata["concurrent"] = True
    current.progress = 20
    await storage.save(current)
    patch = {"nested": [3]}
    winner = await storage.compare_status(original, TaskStatus.PROCESSING, result=patch)
    patch["nested"].append(66)
    winner.result["nested"].append(55)
    winner.metadata["nested"].append(55)
    row = await storage.get(task.task_id)
    assert row.result == {"nested": [3]}
    assert row.metadata == {"nested": [1], "concurrent": True}
    assert row.progress == 20
    assert original.status == TaskStatus.PENDING
    assert await storage.compare_status(original, TaskStatus.PROCESSING) is None


@pytest.mark.parametrize("field", ["task_id", "request_id", "service_id", "user_id", "tenant_id"])
async def test_memory_compare_rejects_identity_replacement(field):
    storage, _, _, _, _, task = await admitted()
    replacement = deepcopy(task)
    setattr(replacement, field, "replacement")
    assert await storage.compare_status(replacement, TaskStatus.PROCESSING) is None
    assert (await storage.get(task.task_id)).status == TaskStatus.PENDING


@pytest.mark.parametrize("terminal", [TaskStatus.COMPLETED, TaskStatus.FAILED])
@pytest.mark.parametrize("cancel_first", [True, False])
async def test_terminal_winner_and_callbacks(terminal, cancel_first):
    storage, manager, request, _, _, task = await admitted()
    task.callback_url = "https://callback.invalid/fixture"
    await storage.save(task)
    manager._send_callback = AsyncMock()
    claimed = await manager.claim_task(task.task_id, request)
    started_at = claimed.started_at
    async def finish():
        return await manager.transition_task(claimed, terminal, result={"output": 1}, error="fixture")
    if cancel_first:
        await manager.cancel(task.task_id)
        assert await finish() is None
        manager._send_callback.assert_not_awaited()
        expected = TaskStatus.CANCELLED
    else:
        assert await finish() is not None
        with pytest.raises(TaskStateConflictError):
            await manager.cancel(task.task_id)
        manager._send_callback.assert_awaited_once()
        expected = terminal
    row = await storage.get(task.task_id)
    assert row.status == expected
    assert row.started_at == started_at
    assert row.completed_at is not None
    if cancel_first:
        assert row.result is None and row.error is None
    else:
        assert row.result == {"output": 1} and row.error == "fixture"


class HeldCancel(MemoryTaskStorage):
    def __init__(self):
        super().__init__()
        self.entered = asyncio.Event()
        self.release = asyncio.Event()
        self.held = False

    async def compare_status(self, expected, status, **kwargs):
        if status == TaskStatus.CANCELLED and not self.held:
            self.held = True
            self.entered.set()
            await self.release.wait()
        return await super().compare_status(expected, status, **kwargs)


async def test_cancel_reloads_after_claim_and_preserves_repeat_timestamp():
    storage, manager, request, _, _, task = await admitted(HeldCancel())
    cancelling = asyncio.create_task(manager.cancel(task.task_id, expected=task))
    await storage.entered.wait()
    assert await manager.claim_task(task.task_id, request) is not None
    storage.release.set()
    await cancelling
    row = await storage.get(task.task_id)
    assert row.status == TaskStatus.CANCELLED
    await manager.cancel(task.task_id)
    assert await storage.get(task.task_id) == row


async def test_cancel_never_adopts_replacement_identity_even_when_already_cancelled():
    storage, manager, _, _, _, task = await admitted(HeldCancel())
    cancelling = asyncio.create_task(manager.cancel(task.task_id, expected=task))
    await storage.entered.wait()
    changed = deepcopy(task)
    changed.tenant_id = "other"
    changed.status = TaskStatus.CANCELLED
    await storage.save(changed)
    storage.release.set()
    with pytest.raises(TaskStateConflictError):
        await cancelling
    assert await storage.get(task.task_id) == changed


async def test_cancel_bounded_when_broken_adapter_reports_perpetual_conflict():
    storage, manager, _, _, _, task = await admitted()
    storage.compare_status = AsyncMock(return_value=None)
    with pytest.raises(TaskStateConflictError):
        await manager.cancel(task.task_id)
    assert storage.compare_status.await_count == 3


async def test_worker_duplicate_claims_invoke_once_and_keep_typed_memory_result():
    storage, manager, request, dispatcher, worker, task = await admitted()
    entered, release = asyncio.Event(), asyncio.Event()
    response = dispatcher.invoke.return_value
    async def invoke(*args, **kwargs):
        entered.set()
        await release.wait()
        return response
    dispatcher.invoke.side_effect = invoke
    first = asyncio.create_task(worker._process(task.task_id, request, [], None))
    await entered.wait()
    await worker._process(task.task_id, request, [], None)
    release.set()
    await first
    await worker._process(task.task_id, request, [], None)
    dispatcher.invoke.assert_awaited_once()
    assert await manager.get_result(task.task_id) == response
    assert (await storage.get(task.task_id)).status == TaskStatus.COMPLETED


@pytest.mark.parametrize("field", ["request_id", "service_id", "user_id", "tenant_id"])
async def test_mismatched_envelope_never_invokes(field):
    storage, _, request, dispatcher, worker, task = await admitted()
    setattr(request, field, "wrong")
    with pytest.raises(TaskStateConflictError):
        await worker._process(task.task_id, request, [], None)
    dispatcher.invoke.assert_not_awaited()
    assert (await storage.get(task.task_id)).status == TaskStatus.PENDING


@pytest.mark.parametrize("status", [TaskStatus.COMPLETED, TaskStatus.FAILED, TaskStatus.CANCELLED])
async def test_worker_never_dispatches_terminal_duplicate(status):
    storage, _, request, dispatcher, worker, task = await admitted()
    task.status = status
    await storage.save(task)
    await worker._process(task.task_id, request, [], None)
    dispatcher.invoke.assert_not_awaited()
    assert await storage.get(task.task_id) == task


async def test_mutated_shared_envelope_cannot_change_verified_dispatch():
    storage, _, request, dispatcher, worker, task = await admitted()
    entered, release = asyncio.Event(), asyncio.Event()
    compare = storage.compare_status
    async def held(expected, status, **kwargs):
        if status == TaskStatus.PROCESSING:
            entered.set()
            await release.wait()
        return await compare(expected, status, **kwargs)
    storage.compare_status = held
    request.parameters = {"nested": [1]}
    roles = ["user"]
    processing = asyncio.create_task(worker._process(task.task_id, request, roles, None))
    await entered.wait()
    request.tenant_id = "wrong"
    request.parameters["nested"].append(2)
    roles.append("admin")
    release.set()
    await processing
    invoked = dispatcher.invoke.await_args.args[0]
    assert invoked.tenant_id == "t"
    assert invoked.parameters == {"nested": [1]}
    assert dispatcher.invoke.await_args.kwargs["roles"] == ["user"]


@pytest.mark.parametrize("commit_before_error", [True, False])
async def test_uncertain_completion_only_conditionally_projects_failure(commit_before_error):
    storage, manager, request, dispatcher, worker, task = await admitted()
    task.callback_url = "https://callback.invalid/fixture"
    await storage.save(task)
    manager._send_callback = AsyncMock()
    compare = storage.compare_status
    async def uncertain(expected, status, **kwargs):
        if status == TaskStatus.COMPLETED:
            if commit_before_error:
                await compare(expected, status, **kwargs)
            raise OSError("synthetic lost acknowledgement")
        return await compare(expected, status, **kwargs)
    storage.compare_status = uncertain
    await worker._process(task.task_id, request, [], None)
    dispatcher.invoke.assert_awaited_once()
    assert (await storage.get(task.task_id)).status == (
        TaskStatus.COMPLETED if commit_before_error else TaskStatus.FAILED
    )
    assert manager._send_callback.await_count == (0 if commit_before_error else 1)


async def test_cancel_while_provider_runs_discards_late_result():
    storage, manager, request, dispatcher, worker, task = await admitted()
    entered, release = asyncio.Event(), asyncio.Event()
    async def invoke(*args, **kwargs):
        entered.set()
        await release.wait()
        return {"late": True}
    dispatcher.invoke.side_effect = invoke
    processing = asyncio.create_task(worker._process(task.task_id, request, [], None))
    await entered.wait()
    await manager.cancel(task.task_id)
    release.set()
    await processing
    row = await storage.get(task.task_id)
    assert row.status == TaskStatus.CANCELLED and row.result is None


async def test_callback_failure_cannot_trigger_failed_projection_and_runs_outside_lock():
    storage, manager, request, _, worker, task = await admitted()
    task.callback_url = "https://callback.invalid/fixture"
    await storage.save(task)
    async def callback(receipt):
        assert not storage._lock.locked()
        assert (await storage.get(receipt.task_id)).status == TaskStatus.COMPLETED
        raise OSError("synthetic callback failure")
    manager._send_callback = AsyncMock(side_effect=callback)
    await worker._process(task.task_id, request, [], None)
    assert (await storage.get(task.task_id)).status == TaskStatus.COMPLETED
    manager._send_callback.assert_awaited_once()


async def test_claim_error_never_invokes_and_worker_lane_continues():
    storage, manager, request, dispatcher, worker, task = await admitted()
    original = manager.claim_task
    calls = 0
    async def claim(*args):
        nonlocal calls
        calls += 1
        if calls == 1:
            raise OSError("synthetic authority error")
        return await original(*args)
    manager.claim_task = claim
    await worker.queue.enqueue(task.task_id, deepcopy(request), [], None)
    async def invoke(*args, **kwargs):
        worker._running = False
        return {"output": 1}
    dispatcher.invoke.side_effect = invoke
    worker._running = True
    await worker._run()
    assert calls == 2
    dispatcher.invoke.assert_awaited_once()
    assert (await storage.get(task.task_id)).status == TaskStatus.COMPLETED


@pytest.mark.parametrize("claim_committed", [True, False])
async def test_uncertain_claim_never_dispatches(claim_committed):
    storage, _, request, dispatcher, worker, task = await admitted()
    compare = storage.compare_status
    async def uncertain(expected, status, **kwargs):
        if claim_committed:
            await compare(expected, status, **kwargs)
        raise OSError("synthetic uncertain claim")
    storage.compare_status = uncertain
    with pytest.raises(OSError):
        await worker._process(task.task_id, request, [], None)
    dispatcher.invoke.assert_not_awaited()
    assert (await storage.get(task.task_id)).status == (
        TaskStatus.PROCESSING if claim_committed else TaskStatus.PENDING
    )


async def test_completion_error_after_cancellation_cannot_project_failure():
    storage, manager, request, dispatcher, worker, task = await admitted()
    compare = storage.compare_status
    async def uncertain(expected, status, **kwargs):
        if status == TaskStatus.COMPLETED:
            await compare(expected, TaskStatus.CANCELLED)
            raise OSError("synthetic uncertainty after cancel")
        return await compare(expected, status, **kwargs)
    storage.compare_status = uncertain
    await worker._process(task.task_id, request, [], None)
    dispatcher.invoke.assert_awaited_once()
    row = await storage.get(task.task_id)
    assert row.status == TaskStatus.CANCELLED and row.result is None and row.error is None


async def test_admission_owns_envelope_before_save_and_dequeue():
    storage, manager, request, dispatcher, worker = fixture()
    entered, release = asyncio.Event(), asyncio.Event()
    save = storage.save
    async def held(task):
        entered.set()
        await release.wait()
        await save(task)
    storage.save = held
    roles = ["user"]
    request.parameters = {"nested": [1]}
    creating = asyncio.create_task(manager.create_task(request, roles, None))
    await entered.wait()
    request.tenant_id = "replacement"
    request.parameters["nested"].append(2)
    roles.append("admin")
    release.set()
    task = await creating
    assert task.tenant_id == "t"
    envelope = await worker.queue.dequeue()
    assert envelope[1].tenant_id == "t"
    assert envelope[1].parameters == {"nested": [1]}
    assert envelope[2] == ["user"]
    await worker._process(*envelope)
    dispatcher.invoke.assert_awaited_once()
    assert (await storage.get(task.task_id)).status == TaskStatus.COMPLETED
