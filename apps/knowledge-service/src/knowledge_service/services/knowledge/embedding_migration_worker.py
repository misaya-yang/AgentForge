"""Cross-process worker for durable T3 embedding migration actions."""

from __future__ import annotations

import asyncio
import os
import uuid
from typing import TYPE_CHECKING, Any

from ...core.observability.logging import get_logger

if TYPE_CHECKING:
    from .knowledge_service import KnowledgeService

logger = get_logger(__name__)


class EmbeddingMigrationJobWorker:
    """Claim and execute PostgreSQL-backed backfill/verify/gate jobs.

    No task is spawned from an HTTP request. API-role processes only insert
    rows; any worker-role process can claim them with SKIP LOCKED. Renewable
    token leases make a dead worker's running row recoverable while preventing
    a late owner from publishing a result after its lease was reclaimed.
    """

    def __init__(
        self,
        service: KnowledgeService,
        *,
        worker_id: str | None = None,
        poll_interval_seconds: float = 0.25,
        lease_seconds: int = 120,
        heartbeat_interval_seconds: float = 20.0,
    ) -> None:
        self.service = service
        self.worker_id = worker_id or (
            f"embedding-migration:{os.getpid()}:{uuid.uuid4().hex[:12]}"
        )
        self.poll_interval_seconds = max(float(poll_interval_seconds), 0.01)
        self.lease_seconds = max(int(lease_seconds), 1)
        self.heartbeat_interval_seconds = min(
            max(float(heartbeat_interval_seconds), 0.01),
            max(self.lease_seconds / 2, 0.01),
        )
        self._running = False
        self._runner: asyncio.Task[None] | None = None

    @property
    def migration_service(self) -> Any:
        value = self.service.embedding_migration_service
        if value is None:
            raise RuntimeError("embedding migration service is unavailable")
        return value

    async def start(self) -> None:
        if self._running:
            return
        # Resolve eagerly so a worker-role process never reports ready while
        # the durable queue authority is absent.
        await self.migration_service.store.require_action_job_store()
        self._running = True
        self._runner = asyncio.create_task(
            self._run(),
            name="embedding-migration-job-worker",
        )

    async def stop(self) -> None:
        self._running = False
        if self._runner is None:
            return
        self._runner.cancel()
        await asyncio.gather(self._runner, return_exceptions=True)
        self._runner = None

    async def _run(self) -> None:
        while self._running:
            try:
                claimed = await self.run_once()
            except asyncio.CancelledError:
                raise
            except Exception:
                logger.exception("Embedding migration job claim loop failed")
                claimed = False
            if claimed:
                continue
            try:
                await asyncio.sleep(self.poll_interval_seconds)
            except asyncio.CancelledError:
                raise

    async def run_once(self) -> bool:
        job = await self.migration_service.store.claim_next_action_job(
            worker_id=self.worker_id,
            lease_seconds=self.lease_seconds,
        )
        if job is None:
            return False
        await self._execute_claimed(job)
        return True

    async def _heartbeat(self, job_id: str, claim_token: str) -> None:
        while True:
            await asyncio.sleep(self.heartbeat_interval_seconds)
            renewed = await self.migration_service.store.heartbeat_action_job(
                job_id,
                claim_token=claim_token,
                lease_seconds=self.lease_seconds,
            )
            if not renewed:
                raise RuntimeError("embedding migration job lease ownership was lost")

    async def _execute_claimed(self, job: dict[str, Any]) -> None:
        job_id = str(job["job_id"])
        claim_token = str(job.get("claim_token") or "")
        if not claim_token:
            raise RuntimeError("claimed embedding migration job has no token")

        action_task = asyncio.create_task(self._execute_action(job))
        heartbeat_task = asyncio.create_task(
            self._heartbeat(job_id, claim_token),
            name=f"embedding-migration-heartbeat:{job_id[:8]}",
        )
        action_completed = False
        try:
            done, _pending = await asyncio.wait(
                {action_task, heartbeat_task},
                return_when=asyncio.FIRST_COMPLETED,
            )
            # If both complete in the same loop turn, prefer the durable action
            # result and let its token CAS decide ownership. Treating a
            # simultaneous transient heartbeat error first could incorrectly
            # mark an already-committed verify/gate result as failed.
            if action_task not in done:
                heartbeat_error = heartbeat_task.exception()
                action_task.cancel()
                await asyncio.gather(action_task, return_exceptions=True)
                raise heartbeat_error or RuntimeError(
                    "embedding migration heartbeat stopped unexpectedly"
                )

            result = await action_task
            action_completed = True
            heartbeat_task.cancel()
            await asyncio.gather(heartbeat_task, return_exceptions=True)
            finished = await self.migration_service.store.finish_action_job(
                job_id,
                claim_token=claim_token,
                result=result,
            )
            if finished is None:
                raise RuntimeError(
                    "embedding migration job result lost its ownership CAS"
                )
        except asyncio.CancelledError as cancellation:
            action_task.cancel()
            heartbeat_task.cancel()
            await asyncio.gather(
                action_task,
                heartbeat_task,
                return_exceptions=True,
            )
            requeue = asyncio.create_task(
                self.migration_service.store.requeue_action_job(
                    job_id,
                    claim_token=claim_token,
                )
            )
            while not requeue.done():
                try:
                    await asyncio.shield(requeue)
                except asyncio.CancelledError:
                    continue
            if not requeue.cancelled() and requeue.exception() is not None:
                logger.error(
                    "Embedding migration job could not be requeued during shutdown",
                    extra={"job_id": job_id},
                    exc_info=requeue.exception(),
                )
            raise cancellation
        except Exception as exc:
            action_task.cancel()
            heartbeat_task.cancel()
            await asyncio.gather(
                action_task,
                heartbeat_task,
                return_exceptions=True,
            )
            if action_completed:
                # The migration state/receipts already committed. Do not mark
                # the job failed merely because its result CAS was transiently
                # unavailable; leave the running lease to expire so a
                # replacement recognizes the correlated postcondition and
                # finishes without repeating paid work.
                logger.exception(
                    "Embedding migration job result persistence deferred to lease recovery",
                    extra={"job_id": job_id},
                )
                return
            await self.migration_service.store.fail_action_job(
                job_id,
                claim_token=claim_token,
                error=f"{type(exc).__name__}: {exc}",
            )
            logger.exception(
                "Embedding migration action failed",
                extra={
                    "job_id": job_id,
                    "migration_id": str(job.get("migration_id") or ""),
                    "action": str(job.get("action") or ""),
                },
            )

    async def _execute_action(self, job: dict[str, Any]) -> dict[str, Any]:
        migration_id = str(job["migration_id"])
        action = str(job["action"])
        migration = await self.migration_service.store.get_migration(migration_id)
        if migration is None:
            raise RuntimeError("embedding migration disappeared before execution")

        # A worker may die after the action committed but before its job-result
        # CAS. A lease-recovering worker recognizes those durable postconditions
        # and finishes the same job without repeating a paid evaluator call.
        recovered = bool(job.get("recovered_from_running"))
        state = str(migration.get("state") or "")
        if recovered and action == "verify" and state in {
            "verified",
            "gating",
            "gate_failed",
            "ready",
            "completed",
        } and str((migration.get("totals") or {}).get("verify_action_job_id") or "") == str(
            job["job_id"]
        ):
            return {"migration": migration, "recovered": True}
        if recovered and action == "gate" and state in {
            "gate_failed",
            "ready",
            "completed",
            "rolled_back",
        } and isinstance(migration.get("gate"), dict) and str(
            migration["gate"].get("action_job_id") or ""
        ) == str(job["job_id"]):
            verdict = dict(migration["gate"])
            return {
                "migration": migration,
                "verdict": verdict,
                "passed": bool(verdict.get("passed")),
                "recovered": True,
            }

        if action == "backfill":
            return await self.migration_service.backfill(migration_id)
        if action == "verify":
            return await self.migration_service.verify(
                migration_id,
                action_job_id=str(job["job_id"]),
            )
        if action != "gate":  # database CHECK + store validation should make this unreachable
            raise RuntimeError(f"unsupported embedding migration action '{action}'")

        dataset = await self.service.db.get_dataset(str(migration["dataset_id"]))
        if dataset is None:
            raise RuntimeError("embedding migration dataset disappeared")
        from .embedding_gate import shadow_serving_gate_evaluator

        payload = job.get("payload") if isinstance(job.get("payload"), dict) else {}
        evaluate = await shadow_serving_gate_evaluator(
            self.service,
            dataset,
            **{
                key: payload[key]
                for key in ("sample_size", "top_k", "tolerance", "floor")
                if payload.get(key) is not None
            },
        )

        async def evaluate_for_job(context: dict[str, Any]) -> Any:
            verdict = await evaluate(context)
            if not isinstance(verdict, dict):
                return verdict
            return {**verdict, "action_job_id": str(job["job_id"])}

        return await self.migration_service.run_gate(
            migration_id,
            evaluate_for_job,
        )
