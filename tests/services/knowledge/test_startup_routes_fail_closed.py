"""Startup contracts for the knowledge-service API surface."""

from __future__ import annotations

import pytest


@pytest.fixture(autouse=True)
def _use_explicit_test_idempotency(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("ENVIRONMENT", "test")
    monkeypatch.setenv("INTERNAL_IDEMPOTENCY_BACKEND", "memory")


def test_storage_signing_key_is_loaded_as_a_redacted_secret(monkeypatch) -> None:
    monkeypatch.setenv(
        "AI_PLATFORM_INTERNAL_TOKEN",
        "unit-test-shared-secret",
    )
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")
    monkeypatch.setenv(
        "KNOWLEDGE_STORAGE__SIGNING_KEY",
        "unit-test-storage-signing-secret",
    )

    from knowledge_service import main

    settings = main.Settings(_env_file=None)

    assert (
        settings.storage.signing_key.get_secret_value()
        == "unit-test-storage-signing-secret"
    )
    assert "unit-test-storage-signing-secret" not in repr(settings)


def test_redis_idempotency_without_url_aborts_app_creation(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")

    from knowledge_service import main

    settings = main.Settings(
        _env_file=None,
        environment="production",
        internal_idempotency_backend="redis",
        internal_comm_redis_url="",
        redis_url="",
    )

    with pytest.raises(RuntimeError, match="neither INTERNAL_COMM_REDIS_URL nor REDIS_URL"):
        main.create_app(settings)


def test_memory_idempotency_is_rejected_outside_local_or_test(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")

    from knowledge_service import main

    settings = main.Settings(
        _env_file=None,
        environment="production",
        internal_idempotency_backend="memory",
    )

    with pytest.raises(RuntimeError, match="allowed only"):
        main.create_app(settings)


@pytest.mark.asyncio
async def test_unreachable_idempotency_redis_aborts_startup(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")

    import redis.asyncio as aioredis
    from knowledge_service import main

    events: list[str] = []

    class UnreachableRedis:
        async def ping(self) -> None:
            events.append("probe")
            raise ConnectionError("synthetic redis outage")

        async def aclose(self) -> None:
            events.append("closed")

    monkeypatch.setattr(
        aioredis,
        "from_url",
        lambda *_args, **_kwargs: UnreachableRedis(),
    )
    app = main.create_app(
        main.Settings(
            _env_file=None,
            environment="production",
            internal_idempotency_backend="redis",
            internal_comm_redis_url="redis://unreachable:6379/3",
        )
    )

    with pytest.raises(RuntimeError, match="Redis idempotency backend is unavailable"):
        async with app.router.lifespan_context(app):
            pytest.fail("lifespan yielded with an unreachable idempotency store")

    assert events == ["probe", "closed"]


@pytest.mark.parametrize("route_module", ["api.routes.eval", "api.routes.knowledge"])
def test_required_router_import_failure_aborts_app_creation(
    monkeypatch,
    route_module: str,
) -> None:
    """A broken required router must not produce a partially ready app."""
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")

    from knowledge_service import main
    from knowledge_service.api import route_registry

    real_import = route_registry.import_module

    def fail_required_router_import(name):
        if name == f"knowledge_service.{route_module}":
            raise ImportError("synthetic required-router failure")
        return real_import(name)

    monkeypatch.setattr(route_registry, "import_module", fail_required_router_import)

    with pytest.raises(ImportError, match="synthetic required-router failure"):
        main.create_app(main.Settings())


@pytest.mark.asyncio
async def test_core_worker_startup_failure_never_marks_service_ready(
    monkeypatch,
) -> None:
    """A failed core worker must abort lifespan and release opened resources."""
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")

    from ai_gateway_core import tracing
    from ai_gateway_core.proxy import drain
    from knowledge_service import main
    from knowledge_service.persistence import database as database_module
    from knowledge_service.services.knowledge import knowledge_service as service_module
    from knowledge_service.services.knowledge import worker as worker_module

    events: list[str] = []

    class FakeDatabaseStorage:
        def __init__(self, *_args, **_kwargs) -> None:
            pass

        async def connect(self) -> None:
            events.append("knowledge-db-open")

        async def close(self) -> None:
            events.append("knowledge-db-close")

    class FakeKnowledgeService:
        def __init__(self, *_args, **_kwargs) -> None:
            pass

        async def close(self) -> None:
            events.append("knowledge-service-close")

    class FailingKnowledgeWorker:
        is_running = False

        def __init__(self, *_args, **_kwargs) -> None:
            pass

        async def start(self) -> None:
            events.append("worker-start")
            raise RuntimeError("synthetic core startup failure")

        async def stop(self) -> None:
            events.append("worker-stop")

    class FakeQdrant:
        async def close(self) -> None:
            events.append("qdrant-close")

    async def fake_init_qdrant(_settings):
        events.append("qdrant-open")
        return FakeQdrant()

    monkeypatch.setattr(main, "_init_qdrant", fake_init_qdrant)
    monkeypatch.setattr(database_module, "DatabaseStorage", FakeDatabaseStorage)
    monkeypatch.setattr(service_module, "KnowledgeService", FakeKnowledgeService)
    monkeypatch.setattr(worker_module, "KnowledgeWorker", FailingKnowledgeWorker)
    monkeypatch.setattr(tracing, "init_tracing", lambda _service: None)
    monkeypatch.setattr(drain, "install_signal_handlers", lambda _loop: None)

    app = main.create_app(main.Settings())

    with pytest.raises(RuntimeError, match="synthetic core startup failure"):
        async with app.router.lifespan_context(app):
            pytest.fail("lifespan yielded after failed core startup")

    assert getattr(app.state, "_ready", False) is False
    assert events == [
        "knowledge-db-open",
        "qdrant-open",
        "worker-start",
        "worker-stop",
        "knowledge-service-close",
        "knowledge-db-close",
        "qdrant-close",
    ]


@pytest.mark.asyncio
async def test_unreachable_qdrant_probe_aborts_startup_client_initialization(
    monkeypatch,
) -> None:
    """A client object is not readiness evidence when its bounded probe fails."""
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "unit-test-shared-secret")
    monkeypatch.setenv("KNOWLEDGE_APP__ALLOW_ANONYMOUS", "false")

    import qdrant_client
    from knowledge_service import main

    events: list[str] = []

    class UnreachableQdrant:
        def __init__(self, **_kwargs) -> None:
            events.append("created")

        async def get_collections(self):
            events.append("probe")
            raise ConnectionError("synthetic qdrant outage")

        async def close(self) -> None:
            events.append("closed")

    monkeypatch.setattr(qdrant_client, "AsyncQdrantClient", UnreachableQdrant)

    with pytest.raises(ConnectionError, match="synthetic qdrant outage"):
        await main._init_qdrant(main.Settings())

    assert events == ["created", "probe", "closed"]
