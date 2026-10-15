from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import httpx
import pytest


class _Task:
    def __init__(self, *, done: bool = False) -> None:
        self._done = done

    def done(self) -> bool:
        return self._done


class _Database:
    def __init__(self, *, error: Exception | None = None) -> None:
        self.error = error
        self.queries: list[str] = []

    async def fetchval(self, query: str) -> int:
        self.queries.append(query)
        if self.error is not None:
            raise self.error
        return 1


class _Redis:
    def __init__(self, ready: bool = True) -> None:
        self.ready = ready

    async def ping(self) -> bool:
        return self.ready


def _settings(*, jwt_secret: str = "unit-test-jwt-secret") -> SimpleNamespace:
    return SimpleNamespace(
        database=SimpleNamespace(enabled=True),
        redis=SimpleNamespace(enabled=True),
        knowledge=SimpleNamespace(qdrant=SimpleNamespace(enabled=False, url="")),
        authentication=SimpleNamespace(
            jwt=SimpleNamespace(enabled=True, secret=jwt_secret, algorithms=["HS256"]),
            api_key=SimpleNamespace(enabled=False, keys=[]),
            guest_session_enabled=False,
        ),
    )


def _app(*, model_plane: object | None = None) -> SimpleNamespace:
    return SimpleNamespace(
        state=SimpleNamespace(
            agent_runtime_control=SimpleNamespace(runtime_url="http://runtime.test"),
            agent_model_plane=model_plane if model_plane is not None else object(),
            provider_service=object(),
            model_service=object(),
            image_task_worker=SimpleNamespace(_loop_task=_Task(), _drain=False),
        )
    )


def _transport(
    *,
    runtime_status: int = 200,
    runtime_payload: object = None,
    knowledge_status: int = 200,
    capability_worker_status: int = 200,
) -> httpx.MockTransport:
    runtime_payload = runtime_payload or {
        "status": "ready",
        "core_ready": True,
        "kernel": "agent-runtime",
    }

    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.host == "runtime.test":
            return httpx.Response(runtime_status, request=request, json=runtime_payload)
        if request.url.host == "knowledge.test":
            return httpx.Response(
                knowledge_status,
                request=request,
                json={"status": "ready" if knowledge_status == 200 else "not_ready"},
            )
        if request.url.host == "capability-worker.test":
            return httpx.Response(
                capability_worker_status,
                request=request,
                json={
                    "status": (
                        "ready" if capability_worker_status == 200 else "not_ready"
                    ),
                    "core_ready": capability_worker_status == 200,
                },
            )
        if request.url.host == "knowledge-worker.test":
            return httpx.Response(200, request=request, json={"status": "ready"})
        if request.url.host == "qdrant.test":
            return httpx.Response(200, request=request, text="healthz check passed")
        raise AssertionError(f"unexpected health probe: {request.url}")

    return httpx.MockTransport(handler)


@pytest.mark.asyncio
async def test_optional_knowledge_failure_degrades_without_unreadying_core(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from src.main import _gateway_readiness_snapshot, _public_gateway_readiness

    monkeypatch.setenv("KB_SERVICE_URL", "http://knowledge.test")
    database = _Database()
    container = SimpleNamespace(database=database, redis=_Redis())
    app = _app()
    async with httpx.AsyncClient(transport=_transport(knowledge_status=503)) as client:
        snapshot = await _gateway_readiness_snapshot(
            app,
            _settings(),
            container,
            http_client=client,
        )

    assert snapshot["core_ready"] is True
    assert snapshot["degraded"] is True
    assert snapshot["capabilities"] == {
        "knowledge_service": "status_503",
        "knowledge_worker": "not_configured",
        "qdrant": "not_configured",
        "capability_worker": "not_configured",
        "image_worker": "healthy",
    }
    assert database.queries == ["SELECT 1"]
    assert _public_gateway_readiness(snapshot) == {
        "status": "ready",
        "checks": {"core": "healthy"},
    }


@pytest.mark.asyncio
async def test_optional_image_worker_failure_does_not_unready_core(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from src.main import _gateway_readiness_snapshot

    monkeypatch.setenv("KB_SERVICE_URL", "http://knowledge.test")
    app = _app()
    app.state.image_task_worker._loop_task = _Task(done=True)
    async with httpx.AsyncClient(transport=_transport()) as client:
        snapshot = await _gateway_readiness_snapshot(
            app,
            _settings(),
            SimpleNamespace(database=_Database(), redis=_Redis()),
            http_client=client,
        )

    assert snapshot["core_ready"] is True
    assert snapshot["degraded"] is True
    assert snapshot["capabilities"]["image_worker"] == "unavailable"


@pytest.mark.asyncio
async def test_optional_capability_worker_failure_degrades_only_its_capability(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from src.main import _gateway_readiness_snapshot

    monkeypatch.delenv("KB_SERVICE_URL", raising=False)
    app = _app()
    app.state.agent_capability_catalog_service = SimpleNamespace(
        worker_url="http://capability-worker.test"
    )
    async with httpx.AsyncClient(
        transport=_transport(capability_worker_status=503)
    ) as client:
        snapshot = await _gateway_readiness_snapshot(
            app,
            _settings(),
            SimpleNamespace(database=_Database(), redis=_Redis()),
            http_client=client,
        )

    assert snapshot["core_ready"] is True
    assert snapshot["degraded"] is True
    assert snapshot["capabilities"]["capability_worker"] == "status_503"


@pytest.mark.asyncio
async def test_full_topology_reports_worker_and_qdrant_from_live_probes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from src.main import _gateway_readiness_snapshot

    monkeypatch.setenv("AI_PLATFORM_TOPOLOGY_MODE", "full")
    monkeypatch.setenv("KB_WORKER_URL", "http://knowledge-worker.test")
    settings = _settings()
    settings.knowledge.qdrant = SimpleNamespace(
        enabled=True,
        url="http://qdrant.test",
    )
    async with httpx.AsyncClient(transport=_transport()) as client:
        snapshot = await _gateway_readiness_snapshot(
            _app(),
            settings,
            SimpleNamespace(database=_Database(), redis=_Redis()),
            http_client=client,
        )

    assert snapshot["capabilities"]["knowledge_worker"] == "healthy"
    assert snapshot["capabilities"]["qdrant"] == "healthy"


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("runtime_status", "runtime_payload", "expected_detail"),
    [
        (503, {"status": "not_ready"}, "status_503"),
        (200, {"status": "ok"}, "schema_mismatch"),
        (200, {"status": "ready", "core_ready": False}, "schema_mismatch"),
        (200, ["ready"], "schema_mismatch"),
    ],
)
async def test_runtime_http_or_schema_failure_is_core_not_ready(
    monkeypatch: pytest.MonkeyPatch,
    runtime_status: int,
    runtime_payload: object,
    expected_detail: str,
) -> None:
    from src.main import _gateway_readiness_snapshot, _public_gateway_readiness

    monkeypatch.delenv("KB_SERVICE_URL", raising=False)
    async with httpx.AsyncClient(
        transport=_transport(
            runtime_status=runtime_status,
            runtime_payload=runtime_payload,
        )
    ) as client:
        snapshot = await _gateway_readiness_snapshot(
            _app(),
            _settings(),
            SimpleNamespace(database=_Database(), redis=_Redis()),
            http_client=client,
        )

    assert snapshot["core_ready"] is False
    assert snapshot["core"]["agent_runtime"] == expected_detail
    public = _public_gateway_readiness(snapshot)
    assert public == {"status": "not_ready", "checks": {"core": "unavailable"}}
    assert "agent_runtime" not in str(public)
    assert expected_detail not in str(public)


@pytest.mark.asyncio
async def test_database_permission_denial_and_auth_misconfiguration_fail_core(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from src.main import _gateway_readiness_snapshot

    monkeypatch.delenv("KB_SERVICE_URL", raising=False)
    async with httpx.AsyncClient(transport=_transport()) as client:
        snapshot = await _gateway_readiness_snapshot(
            _app(),
            _settings(jwt_secret=""),
            SimpleNamespace(
                database=_Database(error=PermissionError("denied")),
                redis=_Redis(),
            ),
            http_client=client,
        )

    assert snapshot["core_ready"] is False
    assert snapshot["core"]["auth_config"] == "misconfigured"
    assert snapshot["core"]["database"] == "unavailable"
    assert "denied" not in str(snapshot)


@pytest.mark.asyncio
async def test_redis_failure_is_core_not_ready(monkeypatch: pytest.MonkeyPatch) -> None:
    from src.main import _gateway_readiness_snapshot

    monkeypatch.delenv("KB_SERVICE_URL", raising=False)
    async with httpx.AsyncClient(transport=_transport()) as client:
        snapshot = await _gateway_readiness_snapshot(
            _app(),
            _settings(),
            SimpleNamespace(database=_Database(), redis=_Redis(ready=False)),
            http_client=client,
        )

    assert snapshot["core_ready"] is False
    assert snapshot["core"]["redis"] == "unavailable"


@pytest.mark.asyncio
async def test_missing_model_plane_is_core_failure(monkeypatch: pytest.MonkeyPatch) -> None:
    from src.main import _gateway_readiness_snapshot

    monkeypatch.delenv("KB_SERVICE_URL", raising=False)
    app = _app()
    app.state.agent_model_plane = None
    async with httpx.AsyncClient(transport=_transport()) as client:
        snapshot = await _gateway_readiness_snapshot(
            app,
            _settings(),
            SimpleNamespace(database=_Database(), redis=_Redis()),
            http_client=client,
        )

    assert snapshot["core_ready"] is False
    assert snapshot["core"]["model_plane"] == "unavailable"


@pytest.mark.asyncio
async def test_draining_gateway_is_not_core_ready(monkeypatch: pytest.MonkeyPatch) -> None:
    from src.main import _gateway_readiness_snapshot

    monkeypatch.delenv("KB_SERVICE_URL", raising=False)
    async with httpx.AsyncClient(transport=_transport()) as client:
        snapshot = await _gateway_readiness_snapshot(
            _app(),
            _settings(),
            SimpleNamespace(database=_Database(), redis=_Redis()),
            http_client=client,
            draining=True,
        )

    assert snapshot["core_ready"] is False
    assert snapshot["core"]["traffic_acceptance"] == "unavailable"


def test_public_projection_never_contains_private_dependency_detail() -> None:
    from src.main import _public_gateway_readiness

    private = {
        "core_ready": False,
        "core": {"database": "permission_denied", "agent_runtime": "schema_mismatch"},
        "capabilities": {"knowledge_service": "timeout"},
    }

    public = _public_gateway_readiness(private)

    assert public == {"status": "not_ready", "checks": {"core": "unavailable"}}
    serialized = str(public)
    for private_token in ("database", "agent_runtime", "knowledge_service", "permission_denied"):
        assert private_token not in serialized


@pytest.mark.asyncio
async def test_public_readiness_route_returns_only_core_aggregate(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from src import main

    app = main.create_app()

    async def private_snapshot(*_args, **_kwargs):
        return {
            "core_ready": True,
            "core": {"database": "healthy", "agent_runtime": "healthy"},
            "capabilities": {"knowledge_service": "status_503"},
            "degraded": True,
        }

    monkeypatch.setattr(main, "_gateway_readiness_snapshot", private_snapshot)
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app),
        base_url="http://gateway.test",
    ) as client:
        response = await client.get("/health/ready")

    assert response.status_code == 200
    assert response.json() == {"status": "ready", "checks": {"core": "healthy"}}
    assert "knowledge_service" not in response.text


@pytest.mark.asyncio
async def test_liveness_never_runs_dependency_probes(monkeypatch: pytest.MonkeyPatch) -> None:
    from src import main

    app = main.create_app()

    async def forbidden_probe(*_args, **_kwargs):
        raise AssertionError("liveness must not probe dependencies")

    monkeypatch.setattr(main, "_gateway_readiness_snapshot", forbidden_probe)
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app),
        base_url="http://gateway.test",
    ) as client:
        response = await client.get("/health/live")

    assert response.status_code == 200
    assert response.json() == {"status": "alive"}


_READINESS_CASES = json.loads(
    (Path(__file__).resolve().parents[1] / "fixtures/deployment/gateway-readiness.v1.json")
    .read_text(encoding="utf-8")
)["cases"]


@pytest.mark.parametrize("case", _READINESS_CASES, ids=lambda case: case["id"])
async def test_deployment_readiness_failures_return_503_without_failing_liveness(
    monkeypatch: pytest.MonkeyPatch,
    case: dict,
) -> None:
    from src import main

    monkeypatch.setenv("KB_SERVICE_URL", "http://knowledge.test")
    monkeypatch.delenv("KB_WORKER_URL", raising=False)
    monkeypatch.delenv("AI_PLATFORM_TOPOLOGY_MODE", raising=False)
    monkeypatch.delenv("AI_PLATFORM_CAPABILITY_WORKER_URL", raising=False)
    probe_app = _app()
    if not case.get("runtime_configured", True):
        probe_app.state.agent_runtime_control.runtime_url = None
    container = SimpleNamespace(
        database=_Database(
            error=None if case.get("database_ready", True) else PermissionError("denied")
        ),
        redis=_Redis(ready=case.get("redis_ready", True)),
    )
    actual_snapshot = main._gateway_readiness_snapshot
    snapshots = []
    async with httpx.AsyncClient(
        transport=_transport(
            runtime_status=case.get("runtime_status", 200),
            runtime_payload=case.get("runtime_payload"),
            knowledge_status=case.get("knowledge_status", 200),
        )
    ) as dependency_client:
        async def configured_snapshot(*_args, **_kwargs):
            snapshot = await actual_snapshot(
                probe_app, _settings(), container, http_client=dependency_client,
            )
            snapshots.append(snapshot)
            return snapshot

        monkeypatch.setattr(main, "_gateway_readiness_snapshot", configured_snapshot)
        app = main.create_app()
        async with httpx.AsyncClient(
            transport=httpx.ASGITransport(app=app), base_url="http://gateway.test",
        ) as client:
            readiness = await client.get("/health/ready")
            liveness = await client.get("/health/live")

    assert readiness.status_code == case["expected_status"]
    expected_ready = case["expected_status"] == 200
    assert readiness.json() == {
        "status": "ready" if expected_ready else "not_ready",
        "checks": {"core": "healthy" if expected_ready else "unavailable"},
    }
    assert len(snapshots) == 1, "liveness must not run dependency probes"
    assert snapshots[0]["core"]["agent_runtime"] == case["expected_detail"]
    assert snapshots[0]["degraded"] is case.get("expected_degraded", False)
    assert liveness.status_code == 200
    assert liveness.json() == {"status": "alive"}
