from __future__ import annotations

import asyncio
import uuid
from contextlib import nullcontext, suppress
from typing import Any

from ai_gateway_core.logging import get_logger
from ai_gateway_core.persistence.repositories.agent_trace_repository import (
    AgentTraceRepository,
    EvalLeaseLost,
)

from .evaluator_executor import EvaluatorExecutor
from .online_sampling import schedule_online_eval_for_trace

logger = get_logger(__name__)


class EvalOutboxWorker:
    """Poll agent_trace_outbox and execute eval jobs off the request path."""

    def __init__(
        self,
        repository: AgentTraceRepository,
        executor: EvaluatorExecutor,
        *,
        poll_interval_s: float = 2.0,
        batch_size: int = 4,
        max_attempts: int = 5,
        lease_seconds: int = 60,
        admission_hook: Any | None = None,
    ) -> None:
        self.repository = repository
        self.executor = executor
        self.poll_interval_s = poll_interval_s
        self.batch_size = batch_size
        self.max_attempts = max_attempts
        self.lease_seconds = max(lease_seconds, 3)
        self.admission_hook = admission_hook
        self.owner_id = f"eval-worker-{uuid.uuid4()}"
        self._running = False
        self._tasks: list[asyncio.Task[None]] = []

    @property
    def running(self) -> bool:
        return self._running

    async def start(self, *, concurrency: int = 2) -> None:
        if self._running:
            return
        self._running = True
        workers = max(1, concurrency)
        self._tasks = [
            asyncio.create_task(self._poll_loop(worker_id=index)) for index in range(workers)
        ]
        logger.info("EvalOutboxWorker started with %s workers", workers)

    async def stop(self) -> None:
        if not self._running:
            return
        self._running = False
        for task in self._tasks:
            task.cancel()
        if self._tasks:
            await asyncio.gather(*self._tasks, return_exceptions=True)
        self._tasks = []
        logger.info("EvalOutboxWorker stopped")

    async def _poll_loop(self, *, worker_id: int) -> None:
        while self._running:
            try:
                jobs = await self.repository.claim_outbox_jobs(
                    limit=1,
                    max_attempts=self.max_attempts,
                    owner_id=f"{self.owner_id}:{worker_id}",
                    lease_seconds=self.lease_seconds,
                )
                if not jobs:
                    await asyncio.sleep(self.poll_interval_s)
                    continue
                for job in jobs:
                    if not self._running:
                        break
                    await self._handle_job(job)
            except asyncio.CancelledError:
                raise
            except Exception as exc:  # noqa: BLE001 - worker must stay alive
                logger.warning("EvalOutboxWorker %s poll error: %s", worker_id, exc)
                await asyncio.sleep(self.poll_interval_s)

    async def _handle_job(self, job: dict[str, Any]) -> None:
        bind = getattr(self.repository, "bind_outbox_claim", None)
        claim_context = bind(job) if callable(bind) else nullcontext()
        lost = asyncio.Event()
        with claim_context:
            execution = asyncio.create_task(self._execute_admitted_job(job))
            heartbeat = None
            renew = getattr(self.repository, "renew_outbox_claim", None)
            if callable(renew):
                async def keep_claim() -> None:
                    try:
                        while True:
                            await asyncio.sleep(self.lease_seconds / 3)
                            valid = await asyncio.wait_for(
                                renew(job, lease_seconds=self.lease_seconds),
                                timeout=self.lease_seconds / 3,
                            )
                            if not valid:
                                raise EvalLeaseLost("eval_outbox_lease_lost")
                    except asyncio.CancelledError:
                        raise
                    except Exception:
                        lost.set()
                        execution.cancel()
                heartbeat = asyncio.create_task(keep_claim())
            try:
                await execution
            except EvalLeaseLost:
                logger.warning("Eval outbox claim lost; stale worker stopped")
            except Exception:
                # Admission failed before executor entry; release the claim to
                # the durable retry queue instead of stranding it until TTL.
                with suppress(EvalLeaseLost):
                    await self.repository.mark_outbox_failed(
                        str(job["job_id"]), error="eval_job_admission_unavailable",
                        retry_after_seconds=5, max_attempts=self.max_attempts,
                    )
            except asyncio.CancelledError:
                if not lost.is_set():
                    # Leave the persisted handle in place for the next owner.
                    abandon = getattr(self.repository, "abandon_outbox_claim", None)
                    if callable(abandon):
                        with suppress(EvalLeaseLost):
                            await abandon()
                    raise
            finally:
                if heartbeat is not None:
                    heartbeat.cancel()
                    await asyncio.gather(heartbeat, return_exceptions=True)

    async def _execute_admitted_job(self, job: dict[str, Any]) -> None:
        if self.admission_hook is None:
            await self._execute_job(job)
            return
        lease = await self.admission_hook(job)
        async with lease:
            await self._execute_job(job)

    async def _execute_job(self, job: dict[str, Any]) -> None:
        job_id = str(job.get("job_id") or "")
        tenant_id = str(job.get("tenant_id") or "")
        job_type = str(job.get("job_type") or "")
        payload = job.get("payload") or {}
        try:
            if job_type == "eval.evaluator.run":
                result = await self.executor.run_job(tenant_id=tenant_id, job_payload=payload)
                if result.status == "failed":
                    raise RuntimeError(result.error_message or "evaluator run failed")
            elif job_type == "trace.ingested":
                await schedule_online_eval_for_trace(
                    self.repository,
                    tenant_id=tenant_id,
                    payload=payload if isinstance(payload, dict) else {},
                    created_by="eval-online-sampler",
                )
            else:
                logger.info("Skipping unsupported outbox job_type=%s", job_type)
            await self.repository.mark_outbox_succeeded(job_id)
        except EvalLeaseLost:
            raise
        except Exception as exc:  # noqa: BLE001 - retry via outbox state
            attempts = int(job.get("attempts") or 1)
            retry_after = min(300, 5 * attempts)
            terminal = attempts >= self.max_attempts
            run_id = str(payload.get("run_id") or "") if isinstance(payload, dict) else ""
            if run_id and job_type == "eval.evaluator.run":
                await self.repository.update_experiment_run(
                    tenant_id=tenant_id,
                    run_id=run_id,
                    status="failed" if terminal else "queued",
                    error_message=f"eval_job_failed:{type(exc).__name__}",
                    mark_finished=terminal,
                )
            await self.repository.mark_outbox_failed(
                job_id,
                error=f"eval_job_failed:{type(exc).__name__}",
                retry_after_seconds=retry_after,
                max_attempts=self.max_attempts,
            )
            logger.warning("Eval outbox job %s failed (attempt %s): %s", job_id, attempts, type(exc).__name__)
