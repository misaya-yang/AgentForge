"""Gateway live/core-ready/capability health projections."""

from __future__ import annotations

import asyncio
import os
from typing import Any

import httpx
from ai_gateway_core.tracing import internal_http_headers


async def probe_http_service(
    name: str,
    base_url: str | None,
    checks: dict[str, str],
    *,
    path: str = "/health",
    required: bool = False,
    expected_status: str | None = None,
    require_core_ready: bool = False,
    client: httpx.AsyncClient | None = None,
) -> bool:
    """Probe one explicit service contract and reject malformed success payloads."""

    if not base_url:
        checks[name] = "not_configured"
        return not required

    url = f"{base_url.rstrip('/')}/{path.lstrip('/')}"
    try:
        timeout = httpx.Timeout(2.0, connect=1.0)
        headers = internal_http_headers()
        if client is None:
            async with httpx.AsyncClient(timeout=timeout, trust_env=False) as owned_client:
                response = await owned_client.get(url, headers=headers)
        else:
            response = await client.get(url, headers=headers)
        if 200 <= response.status_code < 300:
            if expected_status is not None:
                try:
                    payload = response.json()
                except (TypeError, ValueError):
                    checks[name] = "schema_mismatch"
                    return False
                if (
                    not isinstance(payload, dict)
                    or payload.get("status") != expected_status
                    or (require_core_ready and payload.get("core_ready") is not True)
                ):
                    checks[name] = "schema_mismatch"
                    return False
            checks[name] = "healthy"
            return True
        checks[name] = f"status_{response.status_code}"
        return False
    except Exception as exc:
        checks[name] = f"error: {type(exc).__name__}"
        return False


async def gateway_database_is_ready(
    database: object,
    *,
    timeout_seconds: float = 1.0,
) -> bool:
    """Run a bounded read against the live Gateway pool."""

    async def probe() -> bool:
        fetchval = getattr(database, "fetchval", None)
        if callable(fetchval):
            return await fetchval("SELECT 1") == 1
        pool = getattr(database, "_pool", None)
        if pool is None:
            return False
        async with pool.acquire() as connection:
            return await connection.fetchval("SELECT 1") == 1

    try:
        return await asyncio.wait_for(probe(), timeout=max(float(timeout_seconds), 0.05))
    except Exception:
        return False


async def gateway_redis_is_ready(redis: object, *, timeout_seconds: float = 1.0) -> bool:
    ping = getattr(redis, "ping", None)
    if not callable(ping):
        return False
    try:
        return bool(
            await asyncio.wait_for(ping(), timeout=max(float(timeout_seconds), 0.05))
        )
    except Exception:
        return False


def gateway_auth_config_is_ready(settings: object) -> bool:
    authentication = getattr(settings, "authentication", None)
    if authentication is None:
        return False
    jwt = getattr(authentication, "jwt", None)
    if getattr(jwt, "enabled", False) and (
        not str(getattr(jwt, "secret", "") or "").strip()
        or not getattr(jwt, "algorithms", None)
    ):
        return False
    api_key = getattr(authentication, "api_key", None)
    return not (
        getattr(api_key, "enabled", False) and not getattr(api_key, "keys", None)
    )


def _background_task_status(owner: object | None, task_name: str) -> str:
    task = getattr(owner, task_name, None)
    if task is None or getattr(task, "done", lambda: True)():
        return "unavailable"
    if getattr(owner, "_drain", False):
        return "draining"
    return "healthy"


def _model_plane_is_ready(app: Any) -> bool:
    plane = getattr(app.state, "agent_model_plane", None)
    if (
        plane is None
        or getattr(app.state, "provider_service", None) is None
        or getattr(app.state, "model_service", None) is None
    ):
        return False
    client = getattr(plane, "http_client", None)
    return client is None or getattr(client, "is_closed", False) is False


def _qdrant_url(settings: Any) -> str | None:
    knowledge = getattr(settings, "knowledge", None)
    qdrant = getattr(knowledge, "qdrant", None)
    if qdrant is None or getattr(qdrant, "enabled", True) is False:
        return None
    return str(getattr(qdrant, "url", "") or "").strip() or None


def _knowledge_worker_url() -> str | None:
    configured = os.environ.get("KB_WORKER_URL", "").strip()
    if configured:
        return configured
    if os.environ.get("AI_PLATFORM_TOPOLOGY_MODE", "").strip() in {"full", "scale"}:
        return "http://knowledge-worker:8092"
    return None


async def gateway_readiness_snapshot(
    app: Any,
    settings: Any,
    container: Any,
    *,
    http_client: httpx.AsyncClient | None = None,
    draining: bool = False,
) -> dict[str, object]:
    """Return private core/capability detail for public and admin projections."""

    core: dict[str, str] = {
        "auth_config": "healthy" if gateway_auth_config_is_ready(settings) else "misconfigured",
        "database": "unavailable",
        "redis": "unavailable",
        "agent_runtime": "unavailable",
        "model_plane": "healthy" if _model_plane_is_ready(app) else "unavailable",
        "traffic_acceptance": "unavailable" if draining else "healthy",
    }
    capabilities: dict[str, str] = {
        "knowledge_service": "not_configured",
        "knowledge_worker": "not_configured",
        "qdrant": "not_configured",
        "capability_worker": "not_configured",
        "image_worker": _background_task_status(
            getattr(app.state, "image_task_worker", None), "_loop_task"
        ),
    }

    database_enabled = bool(getattr(getattr(settings, "database", None), "enabled", False))
    redis_enabled = bool(getattr(getattr(settings, "redis", None), "enabled", False))
    control = getattr(app.state, "agent_runtime_control", None)
    runtime_url = getattr(control, "runtime_url", None) if control is not None else None
    runtime_checks: dict[str, str] = {}
    knowledge_checks: dict[str, str] = {}
    knowledge_worker_checks: dict[str, str] = {}
    qdrant_checks: dict[str, str] = {}
    capability_worker_checks: dict[str, str] = {}
    catalog_service = getattr(app.state, "agent_capability_catalog_service", None)
    capability_worker_url = (
        getattr(catalog_service, "worker_url", None)
        if catalog_service is not None
        else os.environ.get("AI_PLATFORM_CAPABILITY_WORKER_URL")
    )

    (
        database_ready,
        redis_ready,
        _runtime_ready,
        _capability_worker_ready,
        _knowledge_worker_ready,
        _qdrant_ready,
        _knowledge_ready,
    ) = await asyncio.gather(
        gateway_database_is_ready(container.database)
        if database_enabled
        else asyncio.sleep(0, result=False),
        gateway_redis_is_ready(container.redis)
        if redis_enabled
        else asyncio.sleep(0, result=False),
        probe_http_service(
            "agent_runtime",
            runtime_url,
            runtime_checks,
            path="/health/ready",
            required=True,
            expected_status="ready",
            require_core_ready=True,
            client=http_client,
        ),
        probe_http_service(
            "capability_worker",
            capability_worker_url,
            capability_worker_checks,
            path="/health/ready",
            expected_status="ready",
            require_core_ready=True,
            client=http_client,
        ),
        probe_http_service(
            "knowledge_worker",
            _knowledge_worker_url(),
            knowledge_worker_checks,
            path="/health/ready",
            expected_status="ready",
            client=http_client,
        ),
        probe_http_service(
            "qdrant",
            _qdrant_url(settings),
            qdrant_checks,
            path="/healthz",
            client=http_client,
        ),
        probe_http_service(
            "knowledge_service",
            os.environ.get("KB_SERVICE_URL"),
            knowledge_checks,
            path="/health/ready",
            expected_status="ready",
            client=http_client,
        ),
    )
    core["database"] = "healthy" if database_ready else (
        "disabled" if not database_enabled else "unavailable"
    )
    core["redis"] = "healthy" if redis_ready else (
        "disabled" if not redis_enabled else "unavailable"
    )
    core["agent_runtime"] = runtime_checks.get("agent_runtime", "unavailable")
    capabilities["knowledge_service"] = knowledge_checks.get(
        "knowledge_service", "not_configured"
    )
    capabilities["knowledge_worker"] = knowledge_worker_checks.get(
        "knowledge_worker", "not_configured"
    )
    capabilities["qdrant"] = qdrant_checks.get("qdrant", "not_configured")
    capabilities["capability_worker"] = capability_worker_checks.get(
        "capability_worker", "not_configured"
    )

    core_ready = all(value == "healthy" for value in core.values())
    snapshot: dict[str, object] = {
        "status": "ready" if core_ready else "not_ready",
        "core_ready": core_ready,
        "degraded": any(
            value not in {"healthy", "not_configured", "disabled"}
            for value in capabilities.values()
        ),
        "core": core,
        "capabilities": capabilities,
    }
    app.state.gateway_health_snapshot = snapshot
    return snapshot


def public_gateway_readiness(snapshot: dict[str, object]) -> dict[str, object]:
    """Project private health detail to the non-fingerprinting public contract."""

    ready = snapshot.get("core_ready") is True
    return {
        "status": "ready" if ready else "not_ready",
        "checks": {"core": "healthy" if ready else "unavailable"},
    }


__all__ = [
    "gateway_auth_config_is_ready",
    "gateway_database_is_ready",
    "gateway_readiness_snapshot",
    "gateway_redis_is_ready",
    "probe_http_service",
    "public_gateway_readiness",
]
