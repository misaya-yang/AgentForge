from __future__ import annotations

import asyncio
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.services.knowledge.embedding_migration_worker import (
    EmbeddingMigrationJobWorker,
)

_JOB_ID = "11111111-2222-4333-8444-555555555555"
_MIGRATION_ID = "3f2c1a4e-9b8d-4e6f-8a1b-2c3d4e5f6071"
_TOKEN = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"


class FakeJobStore:
    def __init__(self, *, action: str = "backfill", recovered: bool = False) -> None:
        self.job: dict[str, Any] | None = {
            "job_id": _JOB_ID,
            "migration_id": _MIGRATION_ID,
            "dataset_id": "dataset-a",
            "action": action,
            "state": "running",
            "payload": {},
            "claim_token": _TOKEN,
            "recovered_from_running": recovered,
        }
        self.migration = {
            "migration_id": _MIGRATION_ID,
            "dataset_id": "dataset-a",
            "state": "backfilling",
            "gate": None,
        }
        self.heartbeats = 0
        self.finished: list[dict[str, Any]] = []
        self.failed: list[str] = []
        self.requeued = 0
        self.finish_error = False
        self.requeue_started = asyncio.Event()
        self.requeue_release = asyncio.Event()
        self.requeue_release.set()

    async def require_action_job_store(self) -> None:
        return None

    async def claim_next_action_job(self, **_kwargs: Any) -> dict[str, Any] | None:
        job, self.job = self.job, None
        return job

    async def get_migration(self, _migration_id: str) -> dict[str, Any]:
        return dict(self.migration)

    async def heartbeat_action_job(self, *_args: Any, **_kwargs: Any) -> bool:
        self.heartbeats += 1
        return True

    async def finish_action_job(
        self, _job_id: str, *, result: dict[str, Any], **_kwargs: Any
    ) -> dict[str, Any]:
        if self.finish_error:
            raise RuntimeError("postgres result write unavailable")
        self.finished.append(result)
        return {"state": "succeeded"}

    async def fail_action_job(
        self, _job_id: str, *, error: str, **_kwargs: Any
    ) -> dict[str, Any]:
        self.failed.append(error)
        return {"state": "failed"}

    async def requeue_action_job(self, *_args: Any, **_kwargs: Any) -> bool:
        self.requeue_started.set()
        await self.requeue_release.wait()
        self.requeued += 1
        return True


class FakeMigrationService:
    def __init__(self, store: FakeJobStore) -> None:
        self.store = store
        self.backfill_calls = 0
        self.verify_calls = 0
        self.gate_calls = 0
        self.started = asyncio.Event()
        self.release = asyncio.Event()

    async def backfill(self, migration_id: str) -> dict[str, Any]:
        self.backfill_calls += 1
        self.started.set()
        await self.release.wait()
        return {"migration_id": migration_id, "simulated_duration_seconds": 31}

    async def verify(self, migration_id: str, **_kwargs: Any) -> dict[str, Any]:
        self.verify_calls += 1
        return {"migration_id": migration_id, "state": "verified"}

    async def run_gate(self, migration_id: str, _evaluate: Any) -> dict[str, Any]:
        self.gate_calls += 1
        return {"migration_id": migration_id, "passed": True}


def _worker(
    store: FakeJobStore,
) -> tuple[EmbeddingMigrationJobWorker, FakeMigrationService]:
    migration_service = FakeMigrationService(store)
    service = SimpleNamespace(
        embedding_migration_service=migration_service,
        db=SimpleNamespace(get_dataset=lambda _dataset_id: None),
    )
    return (
        EmbeddingMigrationJobWorker(
            service,
            worker_id="worker-a",
            lease_seconds=1,
            heartbeat_interval_seconds=0.01,
        ),
        migration_service,
    )


@pytest.mark.asyncio
async def test_simulated_over_30s_action_survives_client_disconnect() -> None:
    store = FakeJobStore()
    worker, migration_service = _worker(store)
    execution = asyncio.create_task(worker.run_once())
    await asyncio.wait_for(migration_service.started.wait(), timeout=1)

    # A disconnected HTTP client owns no reference to the durable execution.
    client = asyncio.create_task(asyncio.sleep(60))
    client.cancel()
    await asyncio.gather(client, return_exceptions=True)
    await asyncio.sleep(0.04)
    assert not execution.done()
    assert store.heartbeats >= 2

    migration_service.release.set()
    assert await asyncio.wait_for(execution, timeout=1) is True
    assert migration_service.backfill_calls == 1
    assert store.finished[0]["simulated_duration_seconds"] == 31
    assert store.failed == []


@pytest.mark.asyncio
async def test_worker_cancellation_requeues_owned_job() -> None:
    store = FakeJobStore()
    worker, migration_service = _worker(store)
    execution = asyncio.create_task(worker.run_once())
    await asyncio.wait_for(migration_service.started.wait(), timeout=1)

    execution.cancel()
    with pytest.raises(asyncio.CancelledError):
        await execution

    assert store.requeued == 1
    assert store.finished == []
    assert store.failed == []


@pytest.mark.asyncio
async def test_worker_stop_survives_repeated_cancellation_and_requeues() -> None:
    store = FakeJobStore()
    store.requeue_release.clear()
    worker, migration_service = _worker(store)
    await worker.start()
    await asyncio.wait_for(migration_service.started.wait(), timeout=1)

    stopping = asyncio.create_task(worker.stop())
    await asyncio.wait_for(store.requeue_started.wait(), timeout=1)
    assert worker._runner is not None
    worker._runner.cancel()
    await asyncio.sleep(0)
    assert not stopping.done()
    store.requeue_release.set()
    await asyncio.wait_for(stopping, timeout=1)

    assert worker._runner is None
    assert store.requeued == 1
    assert store.finished == []
    assert store.failed == []


@pytest.mark.asyncio
async def test_recovered_gate_postcondition_does_not_repeat_evaluator() -> None:
    store = FakeJobStore(action="gate", recovered=True)
    store.migration.update(
        {
            "state": "gate_failed",
            "gate": {
                "passed": False,
                "reason": "quality floor",
                "action_job_id": _JOB_ID,
            },
        }
    )
    worker, migration_service = _worker(store)

    assert await worker.run_once() is True
    assert migration_service.gate_calls == 0
    assert store.finished[0]["recovered"] is True
    assert store.finished[0]["passed"] is False


@pytest.mark.asyncio
async def test_recovered_gate_after_rollback_does_not_repeat_evaluator() -> None:
    store = FakeJobStore(action="gate", recovered=True)
    store.migration.update(
        {
            "state": "rolled_back",
            "gate": {
                "passed": True,
                "action_job_id": _JOB_ID,
            },
        }
    )
    worker, migration_service = _worker(store)

    assert await worker.run_once() is True
    assert migration_service.gate_calls == 0
    assert store.finished[0]["recovered"] is True
    assert store.finished[0]["passed"] is True


@pytest.mark.asyncio
async def test_recovered_verify_postcondition_does_not_repeat_scan() -> None:
    store = FakeJobStore(action="verify", recovered=True)
    store.migration.update(
        {
            "state": "verified",
            "totals": {"verify_action_job_id": _JOB_ID},
        }
    )
    worker, migration_service = _worker(store)

    assert await worker.run_once() is True
    assert migration_service.verify_calls == 0
    assert store.finished[0]["recovered"] is True


@pytest.mark.asyncio
async def test_simultaneous_action_success_beats_heartbeat_error() -> None:
    store = FakeJobStore(action="verify")
    worker, migration_service = _worker(store)
    release = asyncio.Event()

    async def verify(_migration_id: str, **_kwargs: Any) -> dict[str, Any]:
        await release.wait()
        return {"state": "verified"}

    async def heartbeat(_job_id: str, _claim_token: str) -> None:
        await release.wait()
        raise RuntimeError("transient heartbeat failure")

    migration_service.verify = verify  # type: ignore[method-assign]
    worker._heartbeat = heartbeat  # type: ignore[method-assign]
    execution = asyncio.create_task(worker.run_once())
    await asyncio.sleep(0)
    release.set()

    assert await asyncio.wait_for(execution, timeout=1) is True
    assert store.finished == [{"state": "verified"}]
    assert store.failed == []


@pytest.mark.asyncio
async def test_action_failure_is_terminal_but_retryable_not_wedged() -> None:
    store = FakeJobStore(action="verify")
    worker, migration_service = _worker(store)

    async def fail_verify(_migration_id: str, **_kwargs: Any) -> dict[str, Any]:
        raise RuntimeError("target scope unavailable")

    migration_service.verify = fail_verify  # type: ignore[method-assign]
    assert await worker.run_once() is True
    assert store.finished == []
    assert len(store.failed) == 1
    assert "target scope unavailable" in store.failed[0]
    assert store.requeued == 0


@pytest.mark.asyncio
async def test_result_persistence_failure_uses_lease_recovery_not_failed_state() -> None:
    store = FakeJobStore(action="verify")
    store.finish_error = True
    worker, _migration_service = _worker(store)

    assert await worker.run_once() is True
    assert store.finished == []
    assert store.failed == []
    assert store.requeued == 0
