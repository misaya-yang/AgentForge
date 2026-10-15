from __future__ import annotations

from ai_gateway_core.logging import get_logger
from fastapi import APIRouter, Depends, HTTPException, Request

from ...core.auth.user_resolver import UserContext
from ...services.llm.provider_health import provider_probe_status
from ...services.llm.provider_setup import configured_providers
from ...services.registry.health_monitor import HealthMonitor
from ...services.registry.service_registry import ServiceRegistry
from ..deps import get_health_monitor, get_registry, get_user_context

logger = get_logger(__name__)

router = APIRouter()


def require_admin(user: UserContext) -> UserContext:
    """Require authenticated admin or ops user.

    Args:
        user: User context from authentication

    Returns:
        The validated user context

    Raises:
        HTTPException: 401 if not authenticated, 403 if not admin/ops
    """
    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="Authentication required")

    allowed_roles = {"admin", "ops", "operator", "devops"}
    user_roles = set(user.roles or [])

    if not allowed_roles.intersection(user_roles):
        logger.warning(
            f"[Security] Non-admin user attempted admin health endpoint: "
            f"user_id={user.user_id}, roles={user.roles}"
        )
        raise HTTPException(status_code=403, detail="Admin access required")

    return user


@router.get("/health")
async def gateway_health(
    registry: ServiceRegistry = Depends(get_registry),
):
    """Public health check - minimal info only.

    Does NOT expose service count to prevent infrastructure fingerprinting.
    """
    # Don't expose service count - just status
    return {"status": "ok"}


async def _gateway_dependency_snapshot(request: Request) -> dict:
    """Resolve the private snapshot through the Gateway-owned probe seam."""

    probe = getattr(request.app.state, "gateway_health_probe", None)
    if callable(probe):
        value = await probe()
        if isinstance(value, dict):
            return value
    cached = getattr(request.app.state, "gateway_health_snapshot", None)
    if isinstance(cached, dict):
        return cached

    # Narrow compatibility fallback for tests/partial app construction. It
    # never feeds the public readiness endpoint.
    runtime_ready = getattr(request.app.state, "agent_runtime_control", None) is not None
    model_plane_ready = getattr(request.app.state, "agent_model_plane", None) is not None
    worker = getattr(request.app.state, "image_task_worker", None)
    worker_ready = (
        worker is not None
        and getattr(getattr(worker, "_loop_task", None), "done", lambda: True)() is False
    )
    return {
        "status": "ready" if runtime_ready and model_plane_ready else "not_ready",
        "core_ready": runtime_ready and model_plane_ready,
        "degraded": not worker_ready,
        "core": {
            "agent_runtime": "healthy" if runtime_ready else "unavailable",
            "model_plane": "healthy" if model_plane_ready else "unavailable",
        },
        "capabilities": {
            "knowledge_service": "not_configured",
            "image_worker": "healthy" if worker_ready else "unavailable",
        },
    }


def _dependency_service_status(snapshot: dict, service_id: str) -> dict:
    core = snapshot.get("core") if isinstance(snapshot.get("core"), dict) else {}
    capabilities = (
        snapshot.get("capabilities")
        if isinstance(snapshot.get("capabilities"), dict)
        else {}
    )
    if service_id == "gateway_core":
        return {
            "status": "healthy" if snapshot.get("core_ready") is True else "unavailable",
            "dependencies": dict(core),
        }
    source = core if service_id in core else capabilities
    status = source.get(service_id, "unavailable")
    return {
        "status": "healthy" if status == "healthy" else status,
        "required": source is core,
    }


@router.get("/health/services")
async def all_services_health(
    request: Request,
    monitor: HealthMonitor = Depends(get_health_monitor),
    user: UserContext = Depends(get_user_context),
):
    """获取所有服务的健康状态，包括虚拟服务（AI 助手）

    Requires admin authentication to prevent infrastructure fingerprinting.
    """
    require_admin(user)
    snapshot = await _gateway_dependency_snapshot(request)
    # 获取数据库服务的健康状态
    health_status = {
        service_id: {
            "status": s.status,
            "latency": s.latency,
            "last_check": s.last_check,
            "error": s.error,
        }
        for service_id, s in monitor.all_status().items()
    }
    health_status["gateway_core"] = _dependency_service_status(snapshot, "gateway_core")
    for service_id in (
        "auth_config",
        "database",
        "redis",
        "agent_runtime",
        "model_plane",
        "knowledge_service",
        "image_worker",
    ):
        health_status[service_id] = _dependency_service_status(snapshot, service_id)

    optional_degraded = snapshot.get("degraded") is True or any(
        getattr(status, "status", None) not in {None, "healthy"}
        for status in monitor.all_status().values()
    )
    # The built-in Assistant is published by ``/api/v1/services`` under the
    # ``assistant`` service_id. Key its health the same way, or every consumer
    # of this map (the Services console among them) reports it as unhealthy
    # because the lookup misses. Same readiness signal as the catalog uses.
    health_status["assistant"] = {
        "status": (
            "unavailable"
            if snapshot.get("core_ready") is not True
            else "degraded"
            if optional_degraded
            else "healthy"
        ),
    }

    return health_status


@router.get("/health/services/{service_id}")
async def service_health(
    service_id: str,
    request: Request,
    monitor: HealthMonitor = Depends(get_health_monitor),
    user: UserContext = Depends(get_user_context),
):
    """获取单个服务的健康状态

    Requires admin authentication.
    """
    require_admin(user)
    known_dependencies = {
        "gateway_core",
        "auth_config",
        "database",
        "redis",
        "agent_runtime",
        "model_plane",
        "knowledge_service",
        "image_worker",
    }
    if service_id in known_dependencies:
        snapshot = await _gateway_dependency_snapshot(request)
        return {
            "service_id": service_id,
            **_dependency_service_status(snapshot, service_id),
        }

    # 处理数据库中的服务
    status = monitor.get_status(service_id)
    if not status:
        raise HTTPException(status_code=404, detail="service health not found")
    return {
        "service_id": status.service_id,
        "status": status.status,
        "latency": status.latency,
        "last_check": status.last_check,
        "error": status.error,
    }


@router.get("/health/providers")
async def all_providers_health(
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """获取所有模型供应商的健康状态

    返回各供应商是否已配置 API Key，以及可用模型数量。
    Requires admin authentication to prevent provider enumeration.
    """
    require_admin(user)
    from ai_gateway_core.enums import ModelProvider

    model_meta = getattr(request.app.state, "model_meta", None)
    if not model_meta:
        raise HTTPException(
            status_code=503,
            detail={"code": "PROVIDER_HEALTH_COLLECTION_UNAVAILABLE"},
        )

    # Per-provider display names. Keyed by the ``ModelProvider`` enum
    # value (which matches ``llm_providers.provider_id`` in the DB).
    provider_names = {
        ModelProvider.OPENAI.value: "OpenAI",
        ModelProvider.ANTHROPIC.value: "Anthropic",
        ModelProvider.DEEPSEEK.value: "DeepSeek",
        ModelProvider.DASHSCOPE.value: "阿里云 DashScope",
        ModelProvider.GOOGLE.value: "Google Gemini",
        ModelProvider.GOOGLE_VERTEX.value: "Google Vertex AI",
    }

    tenant_id = user.tenant_id or "default"
    try:
        configured = set(await configured_providers(model_meta, tenant_id))
        model_counts = await model_meta.count_enabled_models_by_provider(tenant_id)
    except Exception as exc:
        logger.warning("Provider health collection failed (%s)", type(exc).__name__)
        raise HTTPException(status_code=503, detail={"code": "PROVIDER_HEALTH_COLLECTION_UNAVAILABLE"}) from None

    providers_status = {}
    for pid in sorted(set(provider_names) | configured | set(model_counts)):
        is_configured = pid in configured
        providers_status[pid] = {
            "name": provider_names.get(pid, pid),
            **provider_probe_status(request, tenant_id, pid, configured=is_configured),
            "configured": is_configured,
            "model_count": model_counts.get(pid, 0) if is_configured else 0,
        }

    return providers_status
