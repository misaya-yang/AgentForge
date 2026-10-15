from __future__ import annotations

import asyncio
import threading

import pytest
from knowledge_service.services.knowledge.embedding import _await_bounded_thread


async def _wait_until(predicate, *, timeout: float = 1.0) -> None:
    async def poll() -> None:
        while not predicate():
            await asyncio.sleep(0.001)

    await asyncio.wait_for(poll(), timeout=timeout)


@pytest.mark.asyncio
async def test_bounded_thread_releases_slot_after_success() -> None:
    semaphore = asyncio.Semaphore(1)

    result = await _await_bounded_thread(
        lambda: "ok",
        timeout=1.0,
        semaphore=semaphore,
    )
    await _wait_until(lambda: semaphore._value == 1)

    assert result == "ok"


@pytest.mark.asyncio
async def test_cancelled_waiter_holds_slot_until_executor_finishes() -> None:
    semaphore = asyncio.Semaphore(1)
    first_started = threading.Event()
    finish_first = threading.Event()
    second_started = threading.Event()

    def first_call() -> str:
        first_started.set()
        assert finish_first.wait(timeout=2.0)
        return "first"

    def second_call() -> str:
        second_started.set()
        return "second"

    first_task = asyncio.create_task(
        _await_bounded_thread(
            first_call,
            timeout=2.0,
            semaphore=semaphore,
        )
    )
    await _wait_until(first_started.is_set)

    first_task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await first_task

    second_task = asyncio.create_task(
        _await_bounded_thread(
            second_call,
            timeout=2.0,
            semaphore=semaphore,
        )
    )
    await asyncio.sleep(0.05)

    assert semaphore.locked()
    assert not second_started.is_set()

    finish_first.set()
    assert await second_task == "second"
    await _wait_until(lambda: semaphore._value == 1)


@pytest.mark.asyncio
async def test_timeout_holds_slot_until_executor_finishes() -> None:
    semaphore = asyncio.Semaphore(1)
    started = threading.Event()
    finish = threading.Event()

    def slow_call() -> None:
        started.set()
        assert finish.wait(timeout=2.0)

    with pytest.raises(asyncio.TimeoutError):
        await _await_bounded_thread(
            slow_call,
            timeout=0.01,
            semaphore=semaphore,
        )

    assert started.is_set()
    assert semaphore.locked()

    finish.set()
    await _wait_until(lambda: semaphore._value == 1)


@pytest.mark.asyncio
async def test_worker_exception_releases_slot() -> None:
    semaphore = asyncio.Semaphore(1)

    def fail() -> None:
        raise RuntimeError("provider failed")

    with pytest.raises(RuntimeError, match="provider failed"):
        await _await_bounded_thread(fail, timeout=1.0, semaphore=semaphore)
    await _wait_until(lambda: semaphore._value == 1)


@pytest.mark.asyncio
async def test_executor_scheduling_failure_releases_slot(monkeypatch) -> None:
    semaphore = asyncio.Semaphore(1)
    loop = asyncio.get_running_loop()

    def fail_to_schedule(*_args, **_kwargs):
        raise RuntimeError("executor unavailable")

    monkeypatch.setattr(loop, "run_in_executor", fail_to_schedule)

    with pytest.raises(RuntimeError, match="executor unavailable"):
        await _await_bounded_thread(lambda: None, timeout=1.0, semaphore=semaphore)

    assert semaphore._value == 1
