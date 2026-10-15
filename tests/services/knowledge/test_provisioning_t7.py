"""PRD T7-2: durable-queue status collectors that feed
``/knowledge/worker/status`` (and any future readiness surface). The route
wiring itself lands in api/routes/knowledge.py via the integrator; these
tests pin the collector contract it will rely on.
"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

from knowledge_service.core.observability import provisioning

# ---------------------------------------------------------------------------
# durable_queue_status
# ---------------------------------------------------------------------------


async def test_durable_queue_status_reads_postgres_depth() -> None:
    db = SimpleNamespace(count_queued_documents=lambda: _async_value(12))
    status = await provisioning.durable_queue_status(db)
    assert status == {"queue": "ok", "queue_depth": 12, "error": None}


async def test_durable_queue_status_reports_missing_accessor() -> None:
    status = await provisioning.durable_queue_status(SimpleNamespace())
    assert status["queue"] == "unavailable"
    assert status["queue_depth"] is None
    assert "accessor" in (status["error"] or "")


async def test_durable_queue_status_contains_db_failure(monkeypatch) -> None:
    async def _explode() -> int:
        raise RuntimeError("pool exhausted")

    db = SimpleNamespace(count_queued_documents=_explode)
    status = await provisioning.durable_queue_status(db)

    # Honest degraded reporting instead of a 500: the outage is the payload.
    assert status["queue"] == "unavailable"
    assert status["queue_depth"] is None
    assert "pool exhausted" in (status["error"] or "")


async def test_durable_queue_status_bounds_a_hanging_read() -> None:
    async def _hang() -> int:
        await asyncio.sleep(30)
        return 0

    db = SimpleNamespace(count_queued_documents=_hang)
    # wait_for() cancels the hung read; the floor (0.05s) keeps the test fast.
    status = await provisioning.durable_queue_status(db, timeout_seconds=0.0)
    assert status["queue"] == "unavailable"
    assert status["queue_depth"] is None


async def _async_value(value):
    return value


# ---------------------------------------------------------------------------
# build_worker_status
# ---------------------------------------------------------------------------


def _fake_worker(alive_count: int = 2, dead_count: int = 1):
    tasks = [SimpleNamespace(done=lambda: False) for _ in range(alive_count)]
    tasks += [SimpleNamespace(done=lambda: True) for _ in range(dead_count)]

    return SimpleNamespace(
        _running=True,
        _workers=tasks,
        queue=SimpleNamespace(qsize=lambda: 4),
    )


def test_build_worker_status_prefers_durable_depth_for_queue_size() -> None:
    worker = _fake_worker()
    payload = provisioning.build_worker_status(
        worker, {"queue": "ok", "queue_depth": 37, "error": None}
    )
    assert payload["running"] is True
    # queue_size is the authoritative Postgres backlog, not the empty in-pod
    # memory queue (the bug T7-2 exists to fix).
    assert payload["queue_size"] == 37
    assert payload["local_queue_size"] == 4
    assert payload["durable_queue"] == {
        "queue": "ok",
        "queue_depth": 37,
        "error": None,
    }
    assert payload["worker_count"] == 3
    assert payload["workers_alive"] == [True, True, False]


def test_build_worker_status_falls_back_to_local_when_queue_unavailable() -> None:
    worker = _fake_worker(alive_count=1, dead_count=0)
    payload = provisioning.build_worker_status(
        worker, {"queue": "unavailable", "queue_depth": None, "error": "boom"}
    )
    assert payload["queue_size"] == 4
    assert payload["durable_queue"]["queue"] == "unavailable"


def test_build_worker_status_survives_proxy_without_internals() -> None:
    # DurableEnqueueProxy has the same duck type; a half-built object must
    # still produce a status payload rather than raising in the route.
    payload = provisioning.build_worker_status(
        object(), {"queue": "ok", "queue_depth": 0, "error": None}
    )
    assert payload["running"] is False
    assert payload["queue_size"] == 0
    assert payload["worker_count"] == 0
    assert payload["local_queue_size"] is None
