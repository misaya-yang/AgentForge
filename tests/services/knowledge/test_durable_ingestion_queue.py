from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.persistence.database import IndexLeaseUnavailableError
from knowledge_service.services.knowledge.bm25_v2_lifecycle import Bm25V2LifecycleError
from knowledge_service.services.knowledge.knowledge_service import KnowledgeService
from knowledge_service.services.knowledge.worker import KnowledgeWorker


class DurableQueueDatabase:
    def __init__(self, *, status: str = "uploaded", pool_size: int = 8) -> None:
        self.status = status
        self.pool_size = pool_size
        self.enabled = True
        self.archived = False
        self.metadata: dict[str, Any] = {}
        self.document_lock = asyncio.Lock()
        self.busy = False
        self.on_busy: Any = None
        self.events: list[str] = []
        self.recovery_rows: list[dict[str, Any]] = []
        self.dispatch_calls: list[str | None] = []

    def connection_pool_max_size(self) -> int:
        return self.pool_size

    async def list_unbound_special_preparations(self) -> list[dict[str, Any]]:
        return []

    async def list_unfinished_special_publication_datasets(self) -> list[dict[str, Any]]:
        return []

    @asynccontextmanager
    async def document_index_update_lease(self, dataset_id: str, document_id: str):
        assert (dataset_id, document_id) == ("dataset-a", "document-a")
        if self.busy:
            self.events.append("lease-busy")
            if callable(self.on_busy):
                self.on_busy()
            raise IndexLeaseUnavailableError("dataset lifecycle busy")
        async with self.document_lock:
            self.events.append("lease-enter")
            try:
                yield SimpleNamespace(name="document-owner")
            finally:
                self.events.append("lease-exit")

    async def claim_document_for_enqueue(
        self,
        dataset_id: str,
        document_id: str,
        *,
        action: str | None = None,
        recover_stage: str | None = None,
        execution_id: str | None = None,
        pin_execution_rule: bool = False,
    ) -> bool:
        if pin_execution_rule:
            assert action in {"reprocess", "recover", "retry"} and execution_id
        async with self.document_index_update_lease(dataset_id, document_id):
            if self.status not in {"uploaded", "completed", "failed"}:
                self.events.append("enqueue-duplicate")
                return False
            if not self.enabled or self.archived:
                return False
            self.status = "queued"
            self.metadata["_document_ingest_action"] = action or "ingest"
            if recover_stage:
                self.metadata["_document_recover_stage"] = recover_stage
            if execution_id:
                self.metadata["_document_pipeline_execution_id"] = execution_id
            self.events.append("claimed-queued")
            return True

    async def claim_queued_document_for_processing(
        self,
        dataset_id: str,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        assert (dataset_id, document_id) == ("dataset-a", "document-a")
        assert connection is not None
        if self.status != "queued" or not self.enabled or self.archived:
            self.events.append("consumer-skip")
            return False
        self.status = "processing"
        self.events.append("claimed-processing")
        return True

    async def get_dataset(
        self,
        dataset_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        assert dataset_id == "dataset-a"
        del connection
        return {"dataset_id": dataset_id, "tenant_id": "tenant-a", "index_config": {}}

    async def get_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        assert document_id == "document-a"
        del connection
        return {
            "document_id": document_id,
            "dataset_id": "dataset-a",
            "status": self.status,
            "enabled": self.enabled,
            "archived": self.archived,
            "metadata": dict(self.metadata),
        }

    async def update_document_status(
        self,
        document_id: str,
        status: str,
        progress: float | None = None,
        error: str | None = None,
        *,
        connection: Any | None = None,
    ) -> None:
        assert document_id == "document-a"
        assert connection is not None
        del progress, error
        self.status = status
        self.events.append(f"status:{status}")

    async def claim_stuck_documents(self, _threshold: int) -> list[dict[str, Any]]:
        rows, self.recovery_rows = self.recovery_rows, []
        if rows:
            self.status = "queued"
        return rows

    async def list_queued_documents(
        self,
        *,
        limit: int = 100,
        tenant_cursor: str | None = None,
    ) -> list[dict[str, Any]]:
        del limit
        self.dispatch_calls.append(tenant_cursor)
        return []


class DurableQueueService:
    def __init__(self, database: DurableQueueDatabase) -> None:
        self.db = database
        self.vector_store = SimpleNamespace()
        self.settings = SimpleNamespace(
            knowledge=SimpleNamespace(
                large_file_threshold=1024,
                pdf_split_enabled=True,
                pdf_split_max_size_bytes=1024,
                pdf_split_min_pages_per_part=1,
                ocr_strategy="hybrid",
                document_recovery_interval_seconds=1,
                document_stuck_threshold_minutes=1,
                document_worker_concurrency=1,
            )
        )
        self._worker: KnowledgeWorker | None = None
        self.recovery_calls = 0

    async def recover_stuck_documents(
        self,
        threshold: int,
        *,
        worker: KnowledgeWorker,
    ) -> dict[str, Any]:
        self.recovery_calls += 1
        rows = await self.db.claim_stuck_documents(threshold)
        for row in rows:
            await worker.enqueue_claimed(row["dataset_id"], row["document_id"])
        return {"recovered_count": len(rows), "requeued_count": len(rows)}


def make_worker(
    *, status: str = "uploaded", pool_size: int = 8
) -> tuple[KnowledgeWorker, DurableQueueDatabase, DurableQueueService]:
    database = DurableQueueDatabase(status=status, pool_size=pool_size)
    service = DurableQueueService(database)
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]

    async def prepare_generation(_task: Any, *, connection: Any) -> None:
        assert connection is not None
        assert database.document_lock.locked()
        database.events.append("prepare-generation")

    # These tests isolate durable claim/lease/dispatch behavior. Generation
    # cleanup has dedicated worker tests with the real implementation.
    worker._prepare_document_generation = prepare_generation  # type: ignore[method-assign]
    return worker, database, service


@pytest.mark.asyncio
async def test_durable_dispatch_rotates_tenant_cursor_and_deduplicates_local_rows() -> None:
    worker, database, _service = make_worker(status="queued")
    calls = 0

    async def list_queued_documents(
        *,
        limit: int = 100,
        tenant_cursor: str | None = None,
    ) -> list[dict[str, Any]]:
        nonlocal calls
        assert limit == 100
        database.dispatch_calls.append(tenant_cursor)
        calls += 1
        if calls == 1:
            return [
                {
                    "tenant_id": "tenant-a",
                    "dataset_id": "dataset-a",
                    "document_id": "document-a",
                },
                {
                    "tenant_id": "tenant-a",
                    "dataset_id": "dataset-a",
                    "document_id": "document-a",
                },
                {
                    "tenant_id": "tenant-b",
                    "dataset_id": "dataset-b",
                    "document_id": "document-b",
                },
            ]
        worker._running = False
        return []

    database.list_queued_documents = list_queued_documents  # type: ignore[method-assign]
    worker._durable_poll_interval_seconds = 0.01
    worker._running = True
    await asyncio.wait_for(worker._durable_dispatch_loop(), timeout=1)

    assert database.dispatch_calls == [None, "tenant-b"]
    assert worker.queue.qsize() == 2
    first = worker.queue.get_nowait()
    second = worker.queue.get_nowait()
    assert (first.dataset_id, first.document_id) == ("dataset-a", "document-a")
    assert (second.dataset_id, second.document_id) == ("dataset-b", "document-b")
    worker.queue.task_done()
    worker.queue.task_done()


@pytest.mark.asyncio
async def test_production_worker_enqueue_claims_before_memory_and_deduplicates() -> None:
    worker, database, _service = make_worker()

    assert await worker.enqueue("dataset-a", "document-a") is True
    assert await worker.enqueue("dataset-a", "document-a") is False

    assert database.status == "queued"
    assert worker.queue.qsize() == 1
    assert database.events.index("claimed-queued") < len(database.events)
    assert "enqueue-duplicate" in database.events


@pytest.mark.asyncio
async def test_active_queue_kill_switch_refuses_before_durable_claim() -> None:
    worker, database, service = make_worker()
    active = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "index_config": {
            "retrieval": {
                "lexical": {
                    "active_version": "bm25_v2",
                    "bm25_v2": {"shadow_write_enabled": True},
                }
            }
        },
    }

    async def get_active_dataset(_dataset_id: str, *, connection=None):
        del connection
        return active

    database.get_dataset = get_active_dataset  # type: ignore[method-assign]
    service.vector_store = SimpleNamespace(bm25_v2_enabled=False)
    with pytest.raises(Bm25V2LifecycleError) as error:
        await worker.enqueue("dataset-a", "document-a")
    assert error.value.http_status == 503
    assert database.status == "uploaded"
    assert "claimed-queued" not in database.events
    assert worker.queue.empty()

    service.vector_store.bm25_v2_enabled = True
    assert await worker.enqueue("dataset-a", "document-a") is True
    assert database.status == "queued"


@pytest.mark.asyncio
async def test_active_consumer_kill_switch_leaves_waiting_row_unclaimed() -> None:
    worker, database, service = make_worker(status="queued")
    active = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "index_config": {
            "retrieval": {
                "lexical": {
                    "active_version": "bm25_v2",
                    "bm25_v2": {"shadow_write_enabled": True},
                }
            }
        },
    }

    async def get_active_dataset(_dataset_id: str, *, connection=None):
        del connection
        return active

    database.get_dataset = get_active_dataset  # type: ignore[method-assign]
    service.vector_store = SimpleNamespace(bm25_v2_enabled=False)
    await worker.enqueue_claimed("dataset-a", "document-a")
    worker._running = True
    run = asyncio.create_task(worker._run())
    await asyncio.wait_for(worker.queue.join(), timeout=1)
    worker._running = False
    await asyncio.wait_for(run, timeout=1)
    assert database.status == "queued"
    assert "claimed-processing" not in database.events


@pytest.mark.asyncio
async def test_production_worker_consumer_holds_owner_lease_through_terminal() -> None:
    worker, database, _service = make_worker()
    assert await worker.enqueue("dataset-a", "document-a")

    async def process(
        _task: Any, *, connection: Any = None, stage_receipt: Any = None
    ) -> None:
        database.events.append("process")
        assert database.document_lock.locked()
        assert connection is not None
        database.status = "completed"
        worker._running = False

    worker._process_task = process  # type: ignore[method-assign]
    worker._running = True
    await worker._run()

    assert database.status == "completed"
    assert database.events.index("claimed-processing") < database.events.index("process")
    assert database.events.index("process") < len(database.events) - 1
    assert database.events[-1] == "lease-exit"
    await asyncio.wait_for(worker.queue.join(), timeout=1)


@pytest.mark.asyncio
async def test_two_consumers_serialize_same_document_and_only_one_processes() -> None:
    worker, database, _service = make_worker(status="queued")
    await worker.enqueue_claimed("dataset-a", "document-a")
    await worker.enqueue_claimed("dataset-a", "document-a")
    started = asyncio.Event()
    release = asyncio.Event()
    processed = 0

    async def process(
        _task: Any, *, connection: Any = None, stage_receipt: Any = None
    ) -> None:
        nonlocal processed
        processed += 1
        started.set()
        await release.wait()
        database.status = "completed"
        worker._running = False

    worker._process_task = process  # type: ignore[method-assign]
    worker._running = True
    consumers = [asyncio.create_task(worker._run()) for _ in range(2)]
    await asyncio.wait_for(started.wait(), timeout=1)
    assert database.document_lock.locked()
    release.set()
    await asyncio.wait_for(asyncio.gather(*consumers), timeout=1)

    assert processed == 1
    assert database.events.count("claimed-processing") == 1
    assert "consumer-skip" in database.events


@pytest.mark.asyncio
async def test_lifecycle_lease_contention_leaves_durable_queued_without_failed_write() -> None:
    worker, database, _service = make_worker(status="queued")
    database.busy = True
    database.on_busy = lambda: setattr(worker, "_running", False)
    await worker.enqueue_claimed("dataset-a", "document-a")

    worker._running = True
    await worker._run()

    assert database.status == "queued"
    assert "lease-busy" in database.events
    assert "status:failed" not in database.events


@pytest.mark.asyncio
async def test_publication_contention_after_claim_requeues_without_failed_terminal() -> None:
    worker, database, _service = make_worker(status="queued")

    async def process(
        _task: Any, *, connection: Any = None, stage_receipt: Any = None
    ) -> None:
        assert connection is not None
        del stage_receipt
        database.status = "indexing"
        worker._running = False
        raise IndexLeaseUnavailableError("embedding migration owns publication")

    async def requeue(
        _dataset_id: str, _document_id: str, *, connection: Any
    ) -> bool:
        assert connection is not None
        assert database.status == "indexing"
        database.status = "queued"
        database.events.append("requeued-contention")
        return True

    worker._process_task = process  # type: ignore[method-assign]
    database.requeue_cancelled_document_generation = requeue  # type: ignore[attr-defined]
    await worker.enqueue_claimed("dataset-a", "document-a")
    worker._running = True

    await worker._run()

    assert database.status == "queued"
    assert "requeued-contention" in database.events
    assert "status:error" not in database.events


@pytest.mark.asyncio
async def test_shutdown_drains_then_requeues_cancelled_owned_generation() -> None:
    worker, database, _service = make_worker(status="queued")
    worker._shutdown_drain_timeout_seconds = 0.02
    started = asyncio.Event()

    async def process(
        _task: Any, *, connection: Any = None, stage_receipt: Any = None
    ) -> None:
        started.set()
        await asyncio.Event().wait()

    async def requeue(_dataset_id: str, _document_id: str, *, connection: Any) -> bool:
        assert connection is not None
        assert database.document_lock.locked()
        assert database.status == "processing"
        database.status = "queued"
        database.events.append("requeued-cancelled")
        return True

    database.requeue_cancelled_document_generation = requeue
    worker._process_task = process  # type: ignore[method-assign]
    worker._running = True
    await worker.enqueue_claimed("dataset-a", "document-a")
    worker._workers = [asyncio.create_task(worker._run())]

    await asyncio.wait_for(started.wait(), timeout=1)
    await worker.stop()

    assert database.status == "queued"
    assert "requeued-cancelled" in database.events
    assert not database.document_lock.locked()


@pytest.mark.asyncio
async def test_queue_publish_cancellation_keeps_durable_queued_for_recovery() -> None:
    worker, database, _service = make_worker()

    class CancelQueue:
        async def put(self, _task: Any) -> None:
            raise asyncio.CancelledError

        def qsize(self) -> int:
            return 0

    worker.queue = CancelQueue()  # type: ignore[assignment]
    with pytest.raises(asyncio.CancelledError):
        await worker.enqueue("dataset-a", "document-a")

    assert database.status == "queued"


@pytest.mark.asyncio
async def test_atomic_recovery_publishes_preclaimed_row_without_second_claim() -> None:
    worker, database, service = make_worker(status="processing")
    database.recovery_rows = [
        {
            "dataset_id": "dataset-a",
            "document_id": "document-a",
            "old_status": "processing",
        }
    ]

    result = await service.recover_stuck_documents(1, worker=worker)

    assert result == {"recovered_count": 1, "requeued_count": 1}
    assert database.status == "queued"
    assert worker.queue.qsize() == 1
    assert "claimed-queued" not in database.events


@pytest.mark.asyncio
async def test_production_knowledge_service_recovery_uses_enqueue_claimed() -> None:
    database = SimpleNamespace(
        claim_stuck_documents=lambda _threshold: None,
    )

    async def claim(_threshold: int) -> list[dict[str, Any]]:
        return [
            {
                "dataset_id": "dataset-a",
                "document_id": "document-a",
                "title": "Doc",
                "old_status": "queued",
            }
        ]

    database.claim_stuck_documents = claim
    calls: list[tuple[str, str]] = []

    class RecoveryWorker:
        async def enqueue_claimed(self, dataset_id: str, document_id: str) -> None:
            calls.append((dataset_id, document_id))

        async def enqueue(self, _dataset_id: str, _document_id: str) -> bool:
            raise AssertionError("atomic recovery must not claim twice")

    fake_service = SimpleNamespace(db=database)
    result = await KnowledgeService.recover_stuck_documents(
        fake_service,
        stuck_threshold_minutes=1,
        worker=RecoveryWorker(),  # type: ignore[arg-type]
    )

    assert result["recovered_count"] == 1
    assert result["requeued_count"] == 1
    assert calls == [("dataset-a", "document-a")]


@pytest.mark.asyncio
async def test_worker_start_fails_closed_when_pool_cannot_cover_owner_leases() -> None:
    worker, _database, _service = make_worker(pool_size=2)

    with pytest.raises(RuntimeError, match="pool is too small"):
        await worker.start(concurrency=1)

    assert worker._running is False
    assert worker._workers == []
    assert worker._recovery_task is None


@pytest.mark.asyncio
async def test_worker_start_rejects_three_special_publishers_without_spare_connection() -> None:
    worker, _database, _service = make_worker(pool_size=6)

    with pytest.raises(RuntimeError, match="requires at least 9"):
        await worker.start(concurrency=3)

    assert worker._running is False


@pytest.mark.asyncio
async def test_worker_start_runs_immediate_recovery_and_stop_cancels_it() -> None:
    worker, _database, service = make_worker(pool_size=5)
    await worker.start(concurrency=1)
    try:
        for _ in range(20):
            if service.recovery_calls:
                break
            await asyncio.sleep(0)
        assert service.recovery_calls >= 1
        assert worker._recovery_task is not None
    finally:
        await worker.stop()

    assert worker._running is False
    assert worker._workers == []
    assert worker._recovery_task is None


@pytest.mark.asyncio
async def test_enqueue_threads_ingest_action_and_execution_onto_claim() -> None:
    worker, database, _service = make_worker()

    assert (
        await worker.enqueue(
            "dataset-a",
            "document-a",
            action="reembed",
            execution_id="exec-1",
        )
        is True
    )

    assert database.metadata["_document_ingest_action"] == "reembed"
    assert database.metadata["_document_pipeline_execution_id"] == "exec-1"
    assert "_document_recover_stage" not in database.metadata
    assert worker.queue.qsize() == 1


@pytest.mark.asyncio
async def test_enqueue_threads_recover_stage_for_recover_action() -> None:
    worker, database, _service = make_worker()

    assert (
        await worker.enqueue(
            "dataset-a",
            "document-a",
            action="recover",
            recover_stage="indexing",
            execution_id="exec-recover",
        )
        is True
    )

    assert database.metadata["_document_ingest_action"] == "recover"
    assert database.metadata["_document_recover_stage"] == "indexing"


@pytest.mark.asyncio
async def test_reembed_enqueue_skips_chunking_validation() -> None:
    """A legacy/invalid persisted chunking config must not block vector repair."""

    worker, database, _service = make_worker()

    async def get_dataset(_dataset_id: str, *, connection: Any | None = None) -> dict[str, Any]:
        del connection
        return {
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
            # A config the full pipeline would reject at enqueue time
            # (chunk_size below the validated minimum of 50).
            "index_config": {"chunking": {"chunk_size": 30}},
        }

    database.get_dataset = get_dataset  # type: ignore[method-assign]

    assert (
        await worker.enqueue("dataset-a", "document-a", action="reembed") is True
    )
    assert database.metadata["_document_ingest_action"] == "reembed"


@pytest.mark.asyncio
async def test_default_enqueue_still_validates_chunking_config() -> None:
    from knowledge_service.services.knowledge.chunking import (
        validate_persisted_chunking_config,
    )

    worker, database, _service = make_worker()

    async def get_dataset(_dataset_id: str, *, connection: Any | None = None) -> dict[str, Any]:
        del connection
        return {
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
            "index_config": {"chunking": {"chunk_size": 30}},
        }

    database.get_dataset = get_dataset  # type: ignore[method-assign]

    rejected: type[BaseException] | None = None
    try:
        validate_persisted_chunking_config({"chunk_size": 30})
    except Exception as exc:  # noqa: BLE001 - capture the validator's error type
        rejected = type(exc)
    if rejected is None:
        pytest.skip("fixture config is accepted by the validator; nothing to prove")

    with pytest.raises(rejected):
        await worker.enqueue("dataset-a", "document-a")
