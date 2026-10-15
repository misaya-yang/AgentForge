from __future__ import annotations

import asyncio
from pathlib import Path
from types import SimpleNamespace

import httpx
import pytest
import yaml


@pytest.fixture(autouse=True)
def _use_explicit_test_idempotency(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("ENVIRONMENT", "test")
    monkeypatch.setenv("INTERNAL_IDEMPOTENCY_BACKEND", "memory")


def test_runtime_role_defaults_to_all_and_rejects_unknown() -> None:
    from knowledge_service.config import Settings

    assert Settings(_env_file=None).runtime_role == "all"
    with pytest.raises(ValueError):
        Settings(_env_file=None, runtime_role="invalid")


@pytest.mark.asyncio
async def test_database_readiness_runs_bounded_select(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")
    from knowledge_service.health import database_is_ready

    class Healthy:
        async def fetchval(self, query: str) -> int:
            assert query == "SELECT 1"
            return 1

    class Hung:
        async def fetchval(self, _query: str) -> int:
            await asyncio.sleep(1)
            return 1

    assert await database_is_ready(Healthy(), timeout_seconds=0.1)
    assert not await database_is_ready(Hung(), timeout_seconds=0.01)


@pytest.mark.asyncio
async def test_qdrant_readiness_runs_bounded_live_probe(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")
    from knowledge_service.health import qdrant_is_ready

    class Healthy:
        async def get_collections(self):
            return SimpleNamespace(collections=[])

    class Invalid:
        async def get_collections(self):
            return {"unexpected": []}

    class Hung:
        async def get_collections(self):
            await asyncio.sleep(1)
            return SimpleNamespace(collections=[])

    assert await qdrant_is_ready(Healthy(), timeout_seconds=0.1)
    assert not await qdrant_is_ready(Invalid(), timeout_seconds=0.1)
    assert not await qdrant_is_ready(Hung(), timeout_seconds=0.01)


class _HealthTask:
    def __init__(self, *, done: bool = False) -> None:
        self._done = done

    def done(self) -> bool:
        return self._done


class _HealthDatabase:
    async def fetchval(self, query: str) -> int:
        assert query == "SELECT 1"
        return 1


class _HealthQdrant:
    async def get_collections(self):
        return SimpleNamespace(collections=[])


def _health_app(*, worker_ready: bool) -> SimpleNamespace:
    task = _HealthTask(done=not worker_ready)
    document_worker = SimpleNamespace(
        _running=worker_ready,
        _workers=[task],
        _recovery_task=task,
        _durable_dispatch_task=task,
    )
    embedding_worker = SimpleNamespace(_running=worker_ready, _runner=task)
    return SimpleNamespace(
        state=SimpleNamespace(
            _ready=True,
            db=_HealthDatabase(),
            qdrant=_HealthQdrant(),
            knowledge_service=object(),
            knowledge_worker=document_worker,
            embedding_migration_worker=embedding_worker,
        )
    )


@pytest.mark.asyncio
async def test_api_role_does_not_require_local_worker_loops() -> None:
    from knowledge_service.health import readiness_snapshot

    app = _health_app(worker_ready=False)
    snapshot = await readiness_snapshot(
        app,
        runtime_role="api",
        draining=False,
        timeout_seconds=0.1,
    )

    assert snapshot["core_ready"] is True
    assert snapshot["core"] == {
        "startup": "healthy",
        "database": "healthy",
        "qdrant": "healthy",
        "drain": "healthy",
        "api": "healthy",
    }


@pytest.mark.asyncio
async def test_worker_role_reports_dead_loops_not_ready() -> None:
    from knowledge_service.health import readiness_snapshot

    snapshot = await readiness_snapshot(
        _health_app(worker_ready=False),
        runtime_role="worker",
        draining=False,
        timeout_seconds=0.1,
    )

    assert snapshot["core_ready"] is False
    assert snapshot["core"]["worker"] == "unavailable"


@pytest.mark.asyncio
async def test_worker_role_requires_both_durable_loops() -> None:
    from knowledge_service.health import readiness_snapshot

    snapshot = await readiness_snapshot(
        _health_app(worker_ready=True),
        runtime_role="worker",
        draining=False,
        timeout_seconds=0.1,
    )

    assert snapshot["core_ready"] is True
    assert snapshot["core"]["worker"] == "healthy"


def test_public_knowledge_readiness_hides_dependency_and_role_detail() -> None:
    from knowledge_service.health import public_readiness

    public = public_readiness(
        {
            "core_ready": False,
            "runtime_role": "worker",
            "core": {"database": "permission_denied", "qdrant": "timeout"},
        }
    )

    assert public == {
        "status": "not_ready",
        "service": "knowledge-service",
        "checks": {"core": "unavailable"},
    }
    serialized = str(public)
    for private_token in ("database", "qdrant", "worker", "permission_denied"):
        assert private_token not in serialized


@pytest.mark.asyncio
async def test_public_knowledge_route_returns_only_core_aggregate(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")
    from knowledge_service import main

    app = main.create_app(main.Settings(_env_file=None, runtime_role="worker"))

    async def private_snapshot(*_args, **_kwargs):
        return {
            "core_ready": False,
            "runtime_role": "worker",
            "core": {"database": "healthy", "qdrant": "timeout", "worker": "healthy"},
        }

    monkeypatch.setattr(main, "_knowledge_readiness_snapshot", private_snapshot)
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app),
        base_url="http://knowledge.test",
    ) as client:
        response = await client.get("/health/ready")

    assert response.status_code == 503
    assert response.json() == {
        "status": "not_ready",
        "service": "knowledge-service",
        "checks": {"core": "unavailable"},
    }
    assert "qdrant" not in response.text
    assert "worker" not in response.text


@pytest.mark.asyncio
async def test_knowledge_liveness_never_runs_dependency_probes(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")
    from knowledge_service import main

    app = main.create_app(main.Settings(_env_file=None, runtime_role="worker"))

    async def forbidden_probe(*_args, **_kwargs):
        raise AssertionError("liveness must not probe dependencies")

    monkeypatch.setattr(main, "_knowledge_readiness_snapshot", forbidden_probe)
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app),
        base_url="http://knowledge.test",
    ) as client:
        response = await client.get("/health/live")

    assert response.status_code == 200
    assert response.json() == {"status": "alive", "service": "knowledge-service"}


def test_worker_role_exposes_health_without_business_routes(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")

    from knowledge_service import main

    app = main.create_app(main.Settings(_env_file=None, runtime_role="worker"))
    paths = {route.path for route in app.routes}
    assert "/health/ready" in paths
    assert "/version" not in paths
    assert not any(path.startswith("/api/v1/") for path in paths)


def test_compose_splits_api_and_worker_roles() -> None:
    compose = yaml.safe_load(Path("docker-compose.yml").read_text())
    api = compose["services"]["knowledge-service"]
    worker = compose["services"]["knowledge-worker"]

    assert api["environment"]["KNOWLEDGE_RUNTIME_ROLE"] == "api"
    assert worker["environment"]["KNOWLEDGE_RUNTIME_ROLE"] == "worker"
    assert worker["image"] == api["image"]
    assert "ports" not in worker
    assert worker["volumes"] == api["volumes"]

    dev_compose = yaml.safe_load(Path("docker-compose.dev.yml").read_text())
    assert dev_compose["services"]["knowledge-worker"]["volumes"] == dev_compose[
        "services"
    ]["knowledge-service"]["volumes"]


def test_compose_defaults_knowledge_idempotency_to_redis() -> None:
    compose = yaml.safe_load(Path("docker-compose.yml").read_text())
    api_environment = compose["services"]["knowledge-service"]["environment"]
    worker_environment = compose["services"]["knowledge-worker"]["environment"]

    # Compose is a production-like runtime. A stale workstation .env must not
    # downgrade cross-process idempotency to per-process memory.
    assert api_environment["INTERNAL_IDEMPOTENCY_BACKEND"] == "redis"
    assert worker_environment["INTERNAL_IDEMPOTENCY_BACKEND"] == "redis"


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("role", "worker_starts"),
    [("api", False), ("worker", True)],
)
async def test_runtime_role_uses_one_pool_and_closes_owned_resources(
    monkeypatch,
    role: str,
    worker_starts: bool,
) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")

    from ai_gateway_core import tracing
    from ai_gateway_core.proxy import drain
    from knowledge_service import main
    from knowledge_service.persistence import database as database_module
    from knowledge_service.services.knowledge import (
        embedding_migration_worker as embedding_worker_module,
    )
    from knowledge_service.services.knowledge import knowledge_service as service_module
    from knowledge_service.services.knowledge import worker as worker_module

    events: list[str] = []

    class Database:
        def __init__(self, *_args, **_kwargs) -> None:
            pass

        async def connect(self) -> None:
            events.append("db-open")

        async def close(self) -> None:
            events.append("db-close")

    class Service:
        def __init__(self, *, database, **_kwargs) -> None:
            self.db = database
            events.append("service-open")

        async def close(self) -> None:
            events.append("service-close")

    class Worker:
        _running = False

        def __init__(self, *_args, **_kwargs) -> None:
            self._workers = []
            self.queue = asyncio.Queue()

        async def start(self) -> None:
            self._running = True
            events.append("worker-start")

        async def stop(self) -> None:
            events.append("worker-stop")

    class Producer(Worker):
        def __init__(self, *_args, **_kwargs) -> None:
            super().__init__()
            events.append("producer-open")

    class EmbeddingWorker(Worker):
        async def start(self) -> None:
            self._running = True
            events.append("embedding-worker-start")

        async def stop(self) -> None:
            events.append("embedding-worker-stop")

    class Qdrant:
        async def close(self) -> None:
            events.append("qdrant-close")

    async def init_qdrant(_settings):
        events.append("qdrant-open")
        return Qdrant()

    monkeypatch.setattr(main, "_init_qdrant", init_qdrant)
    monkeypatch.setattr(database_module, "DatabaseStorage", Database)
    monkeypatch.setattr(service_module, "KnowledgeService", Service)
    monkeypatch.setattr(worker_module, "KnowledgeWorker", Worker)
    monkeypatch.setattr(worker_module, "DurableEnqueueProxy", Producer)
    monkeypatch.setattr(
        embedding_worker_module,
        "EmbeddingMigrationJobWorker",
        EmbeddingWorker,
    )
    monkeypatch.setattr(tracing, "init_tracing", lambda _service: None)
    monkeypatch.setattr(drain, "install_signal_handlers", lambda _loop: None)

    app = main.create_app(main.Settings(_env_file=None, runtime_role=role))
    async with app.router.lifespan_context(app):
        assert app.state.db is app.state.knowledge_service.db
        assert app.state.knowledge_service._worker is app.state.knowledge_worker
        assert ("worker-start" in events) is worker_starts
        assert ("embedding-worker-start" in events) is worker_starts

    assert events.count("db-open") == 1
    assert events.count("db-close") == 1
    assert events[-3:] == ["service-close", "qdrant-close", "db-close"]
