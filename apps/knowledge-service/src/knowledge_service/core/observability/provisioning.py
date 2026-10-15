"""Provisioning reliability: durable-queue truth for status endpoints (PRD T7-2).

``/knowledge/worker/status`` historically reported the in-process
``asyncio`` queue only — which is nearly always empty on an API-role pod
(ingestions live in PostgreSQL since T1) and told operators nothing about the
real backlog. These helpers read the durable queue through the persistence
layer's existing public methods (they never add SQL here — the queue's
definition of "dispatchable" must stay in exactly one place,
``DatasetsMixin.count_queued_documents``).

``build_worker_status`` is consumed by the route (integrated separately,
since ``api/routes/knowledge.py`` has another owner) and must degrade to an
honest ``"queue": "unavailable"`` instead of raising: a diagnostics endpoint
that 500s when the pool is exhausted hides the exact outage it exists to
show.
"""

from __future__ import annotations

import asyncio
from typing import Any


async def durable_queue_status(db: Any, *, timeout_seconds: float = 2.0) -> dict[str, Any]:
    """Read dispatchable backlog depth from the PostgreSQL durable queue.

    Returns ``{"queue": "ok"|"unavailable", "queue_depth": int|None,
    "error": str|None}``. Never raises and never blocks longer than
    ``timeout_seconds`` — status reporting is best-effort by contract.
    """
    counter = getattr(db, "count_queued_documents", None)
    if not callable(counter):
        return {"queue": "unavailable", "queue_depth": None, "error": "no durable queue accessor"}

    try:
        depth = await asyncio.wait_for(counter(), timeout=max(float(timeout_seconds), 0.05))
        return {"queue": "ok", "queue_depth": int(depth), "error": None}
    except Exception as exc:  # noqa: BLE001 - diagnostics must not fail on read errors
        return {
            "queue": "unavailable",
            "queue_depth": None,
            "error": f"{type(exc).__name__}: {str(exc)[:200]}",
        }


def build_worker_status(worker: Any, queue_status: dict[str, Any]) -> dict[str, Any]:
    """Compose the ``/knowledge/worker/status`` payload from both planes.

    In-process fields keep their historical names (the web console parses
    them); ``durable_queue`` carries the authoritative Postgres view, and
    ``queue_size`` is upgraded to the durable depth when the queue read
    succeeded so old clients finally see the real backlog.
    """
    workers = list(getattr(worker, "_workers", None) or [])
    payload: dict[str, Any] = {
        "running": bool(getattr(worker, "_running", False)),
        "queue_size": queue_status.get("queue_depth")
        if queue_status.get("queue") == "ok"
        else getattr(getattr(worker, "queue", None), "qsize", lambda: None)(),
        "worker_count": len(workers),
        "workers_alive": [not task.done() for task in workers],
        "durable_queue": dict(queue_status),
    }
    # A worker-role pod's memory queue is prefetch state, not backlog; expose
    # it separately instead of hiding the durable numbers under it.
    local_qsize = getattr(getattr(worker, "queue", None), "qsize", None)
    payload["local_queue_size"] = local_qsize() if callable(local_qsize) else None
    return payload
