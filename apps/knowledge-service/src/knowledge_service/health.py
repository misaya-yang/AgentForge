"""Role-aware Knowledge Service liveness/readiness helpers."""

from __future__ import annotations

import asyncio
from typing import Any


async def database_is_ready(database: Any, *, timeout_seconds: float = 1.0) -> bool:
    """Run bounded authority-backed readiness instead of checking object presence."""

    fetchval = getattr(database, "fetchval", None)
    if not callable(fetchval):
        return False
    try:
        return (
            await asyncio.wait_for(
                fetchval("SELECT 1"),
                timeout=max(float(timeout_seconds), 0.05),
            )
            == 1
        )
    except Exception:
        return False


async def qdrant_is_ready(qdrant: Any, *, timeout_seconds: float = 1.0) -> bool:
    """Run a bounded live Qdrant query; object presence is not readiness."""

    get_collections = getattr(qdrant, "get_collections", None)
    if not callable(get_collections):
        return False
    try:
        response = await asyncio.wait_for(
            get_collections(),
            timeout=max(float(timeout_seconds), 0.05),
        )
    except Exception:
        return False
    collections = (
        response.get("collections")
        if isinstance(response, dict)
        else getattr(response, "collections", None)
    )
    return isinstance(collections, (list, tuple))


def _task_is_running(task: Any) -> bool:
    return task is not None and getattr(task, "done", lambda: True)() is False


def _document_worker_is_ready(worker: Any) -> bool:
    if not bool(getattr(worker, "_running", False)):
        return False
    workers = getattr(worker, "_workers", None)
    if not isinstance(workers, list) or not workers or not all(
        _task_is_running(task) for task in workers
    ):
        return False
    return _task_is_running(getattr(worker, "_recovery_task", None)) and _task_is_running(
        getattr(worker, "_durable_dispatch_task", None)
    )


def _embedding_worker_is_ready(worker: Any) -> bool:
    return bool(getattr(worker, "_running", False)) and _task_is_running(
        getattr(worker, "_runner", None)
    )


async def readiness_snapshot(
    app: Any,
    *,
    runtime_role: str,
    draining: bool,
    timeout_seconds: float = 1.0,
) -> dict[str, Any]:
    """Return private role-aware core detail for the public aggregate probe."""

    database_ready, qdrant_ready = await asyncio.gather(
        database_is_ready(
            getattr(app.state, "db", None),
            timeout_seconds=timeout_seconds,
        ),
        qdrant_is_ready(
            getattr(app.state, "qdrant", None),
            timeout_seconds=timeout_seconds,
        ),
    )
    startup_ready = bool(getattr(app.state, "_ready", False))
    api_ready = getattr(app.state, "knowledge_service", None) is not None
    worker_ready = _document_worker_is_ready(
        getattr(app.state, "knowledge_worker", None)
    ) and _embedding_worker_is_ready(
        getattr(app.state, "embedding_migration_worker", None)
    )
    core: dict[str, str] = {
        "startup": "healthy" if startup_ready else "unavailable",
        "database": "healthy" if database_ready else "unavailable",
        "qdrant": "healthy" if qdrant_ready else "unavailable",
        "drain": "unavailable" if draining else "healthy",
    }
    if runtime_role in {"api", "all"}:
        core["api"] = "healthy" if api_ready else "unavailable"
    if runtime_role in {"worker", "all"}:
        core["worker"] = "healthy" if worker_ready else "unavailable"
    ready = all(value == "healthy" for value in core.values())
    snapshot = {
        "status": "ready" if ready else "not_ready",
        "core_ready": ready,
        "runtime_role": runtime_role,
        "core": core,
    }
    app.state.knowledge_health_snapshot = snapshot
    return snapshot


def public_readiness(snapshot: dict[str, Any]) -> dict[str, Any]:
    ready = snapshot.get("core_ready") is True
    return {
        "status": "ready" if ready else "not_ready",
        "service": "knowledge-service",
        "checks": {"core": "healthy" if ready else "unavailable"},
    }


__all__ = [
    "database_is_ready",
    "public_readiness",
    "qdrant_is_ready",
    "readiness_snapshot",
]
