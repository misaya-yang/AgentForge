from __future__ import annotations

from collections.abc import Sequence
from dataclasses import asdict

from ai_gateway_core.enums import ServiceType
from fastapi import APIRouter, Body, Depends, HTTPException, Query, Request

from ...core.auth.permissions import (
    Capability,
    build_permission_denied_detail,
    check_capability,
)
from ...core.auth.service_access import (
    ServiceAccessPolicy,
    evaluate_service_access,
    service_scope_matches,
)
from ...core.auth.service_access_resolver import (
    clear_service_access_constraint_cache,
    load_service_access_constraints,
)
from ...core.auth.user_resolver import UserContext
from ...services.registry.service_registry import ServiceRegistry
from ..deps import (
    AuthContext,
    get_auth_context,
    get_registry,
    get_user_context,
    require_platform_admin,
)
from ._langgraph_connector_config import _normalize_langgraph_connector_config
from ._langgraph_model_policy import _validate_langgraph_model_override
from ._route_trace import current_trace_id

router = APIRouter()


_SERVICE_MUTABLE_FIELDS = {
    "name",
    "description",
    "version",
    "service_type",
    "supported_modes",
    "connector_type",
    "connector_config",
    "input_schema",
    "output_schema",
    "accepted_content_types",
    "output_content_types",
    "session_enabled",
    "session_adapter",
    "timeout",
    "max_retries",
    "retry_delay",
    "circuit_breaker_enabled",
    "failure_threshold",
    "recovery_timeout",
    "rate_limit",
    "concurrency_limit",
    "status",
    "tags",
    "metadata",
    "async_config",
    "service_config",
}


# 敏感配置字段（需要脱敏）
_SENSITIVE_CONNECTOR_FIELDS = {
    "auth_token",
    "api_key",
    "api_secret",
    "password",
    "secret",
    "access_token",
    "refresh_token",
    "private_key",
    "credentials",
    "langsmith_api_key",
    "headers",
}


def _current_trace_id(request: Request) -> str:
    return current_trace_id(request)


def _invalidate_proxy_config_cache(request: Request, service_id: str | None = None) -> None:
    loader = getattr(request.app.state, "proxy_config_loader", None)
    invalidate = getattr(loader, "invalidate", None)
    if callable(invalidate):
        invalidate(service_id)
    clear_service_access_constraint_cache()


def _require_capability(request: Request, auth: AuthContext, capability: Capability) -> None:
    decision = check_capability(
        rbac=request.app.state.dispatcher.rbac,
        roles=auth.roles,
        permissions=auth.permissions,
        capability=capability,
    )
    if decision.allowed:
        return

    raise HTTPException(
        status_code=403,
        detail=build_permission_denied_detail(
            capability=capability,
            trace_id=_current_trace_id(request),
        ),
    )


def _normalize_candidates(values: Sequence[str]) -> list[str]:
    normalized: list[str] = []
    for value in values:
        token = str(value or "").strip().lower()
        if token and token not in normalized:
            normalized.append(token)
    return normalized


def _service_visible_with_scopes(
    *,
    aliases: Sequence[str],
    allowed_sources: Sequence[tuple[str, list[str]]],
    user_policy: ServiceAccessPolicy,
    is_admin: bool,
) -> bool:
    normalized_aliases = _normalize_candidates(aliases)
    if not normalized_aliases:
        return False

    for _, allowed in allowed_sources:
        if service_scope_matches(allowed, normalized_aliases):
            continue
        return False

    if is_admin:
        return True

    permitted, _ = evaluate_service_access(user_policy, normalized_aliases)
    return permitted


async def _load_access_constraints(
    request: Request,
    user: UserContext,
) -> tuple[list[tuple[str, list[str]]], ServiceAccessPolicy]:
    return await load_service_access_constraints(request, user)


def _mask_sensitive_config(config: dict) -> dict:
    """脱敏敏感配置字段"""
    if not config:
        return {}
    masked = {}
    for key, value in config.items():
        if key.lower() in _SENSITIVE_CONNECTOR_FIELDS:
            if isinstance(value, str) and value:
                masked[key] = f"***{value[-4:]}" if len(value) > 4 else "***"
            elif isinstance(value, dict):
                masked[key] = "***[hidden]***"
            else:
                masked[key] = "***"
        elif key == "gateway_model" and isinstance(value, dict):
            masked[key] = _mask_sensitive_config(value)
        else:
            if isinstance(value, dict):
                masked[key] = _mask_sensitive_config(value)
            else:
                masked[key] = value
    return masked


def _service_to_dict(service, mask_secrets: bool = False) -> dict:
    """
    将 Service 转换为字典

    Args:
        service: Service 对象
        mask_secrets: 是否脱敏敏感字段（非管理员时应为 True）
    """
    connector_config = service.connector_config or {}
    if mask_secrets:
        connector_config = _mask_sensitive_config(connector_config)

    service_type_value = (
        service.service_type.value
        if hasattr(service.service_type, "value")
        else service.service_type
    )
    metadata = dict(service.metadata or {})
    if str(service_type_value) == "langgraph":
        adapter_type = str(
            metadata.get("adapter_type") or connector_config.get("adapter_type") or ""
        ).strip()
        if not adapter_type:
            metadata["adapter_type"] = "langgraph"

        proxy_mode = str(
            metadata.get("proxy_mode") or connector_config.get("proxy_mode") or ""
        ).strip()
        if proxy_mode and not metadata.get("proxy_mode"):
            metadata["proxy_mode"] = proxy_mode

    data = {
        "service_id": service.service_id,
        "name": service.name,
        "description": service.description,
        "version": service.version,
        "service_type": service_type_value,
        "supported_modes": [
            m.value if hasattr(m, "value") else m for m in (service.supported_modes or [])
        ],
        "connector_type": service.connector_type.value
        if hasattr(service.connector_type, "value")
        else service.connector_type,
        "connector_config": connector_config,
        "input_schema": service.input_schema or {},
        "output_schema": service.output_schema or {},
        "accepted_content_types": [
            t.value if hasattr(t, "value") else t for t in (service.accepted_content_types or [])
        ],
        "output_content_types": [
            t.value if hasattr(t, "value") else t for t in (service.output_content_types or [])
        ],
        "session_enabled": bool(service.session_enabled),
        "session_adapter": service.session_adapter,
        "timeout": service.timeout,
        "max_retries": service.max_retries,
        "retry_delay": service.retry_delay,
        "circuit_breaker_enabled": service.circuit_breaker_enabled,
        "failure_threshold": service.failure_threshold,
        "recovery_timeout": service.recovery_timeout,
        "rate_limit": service.rate_limit,
        "concurrency_limit": service.concurrency_limit,
        "status": service.status,
        "tags": service.tags or [],
        "metadata": metadata,
        "async_config": service.async_config,
    }
    if getattr(service, "service_config", None):
        data["service_config"] = asdict(service.service_config)
    return data


@router.post("/services")
async def register_service(
    request: Request,
    definition: dict = Body(...),
    registry: ServiceRegistry = Depends(get_registry),
    auth: AuthContext = Depends(get_auth_context),
):
    # 权限：ServiceConfigWrite capability
    require_platform_admin(request, auth, Capability.SERVICE_CONFIG_WRITE)
    try:
        normalized_definition = dict(definition)
        if isinstance(definition.get("connector_config"), dict):
            normalized_definition["connector_config"] = dict(definition["connector_config"])
        if isinstance(definition.get("metadata"), dict):
            normalized_definition["metadata"] = dict(definition["metadata"])
        _normalize_langgraph_connector_config(normalized_definition)
        await _validate_langgraph_model_override(
            request,
            tenant_id=auth.tenant_id or "default",
            definition=normalized_definition,
        )
        service = registry._service_from_dict(normalized_definition)
        await registry.register(service)
        _invalidate_proxy_config_cache(request, service.service_id)
    except HTTPException:
        raise
    except Exception as exc:
        raise HTTPException(status_code=400, detail=str(exc))
    return {"service_id": service.service_id, "status": "registered"}


@router.get("/services")
async def list_services(
    request: Request,
    service_type: str | None = Query(default=None),
    tags: list[str] | None = Query(default=None),
    registry: ServiceRegistry = Depends(get_registry),
    auth: AuthContext = Depends(get_auth_context),
    user: UserContext = Depends(get_user_context),
):
    """列出服务（需要 ServiceListRead capability）

    返回所有服务，包括：
    - 虚拟服务（AI 助手）：内置多功能助手
    - LangGraph 服务：外部 LangGraph Agent
    - 代理服务：LLM API 代理
    """
    # 权限检查：ServiceListRead capability（兼容 legacy alias）
    _require_capability(request, auth, Capability.SERVICE_LIST_READ)

    is_admin = "admin" in auth.roles
    allowed_sources, user_policy = await _load_access_constraints(request, user)
    transparent_proxy = getattr(request.app.state, "transparent_proxy", None)

    # 构建虚拟服务列表（内置服务，不在数据库中）
    virtual_services = []

    # AI 助手虚拟服务
    if service_type is None or service_type == "assistant":
        assistant_aliases = ["assistant", "ai 助手", "ai assistant"]
        if _service_visible_with_scopes(
            aliases=assistant_aliases,
            allowed_sources=allowed_sources,
            user_policy=user_policy,
            is_admin=is_admin,
        ):
            runtime_ready = getattr(request.app.state, "agent_runtime_control", None) is not None
            worker = getattr(request.app.state, "image_task_worker", None)
            worker_ready = worker is not None and not getattr(
                getattr(worker, "_loop_task", None), "done", lambda: True
            )()
            runtime_health = {
                "status": "healthy" if runtime_ready else "unavailable",
                "worker": "healthy" if worker_ready else "unavailable",
            }
            virtual_services.append(
                {
                    "service_id": "assistant",
                    "name": "AI 助手",
                    "description": "内置多功能 AI 助手，支持知识库检索、网页搜索、工具调用、图像生成",
                    "version": "2.0.0",
                    "service_type": "assistant",
                    "supported_modes": ["chat", "stream"],
                    "accepted_content_types": ["application/json"],
                    "output_content_types": ["application/json", "text/event-stream"],
                    "status": "active" if runtime_ready and worker_ready else "unavailable",
                    "tags": ["builtin", "assistant", "rag", "tools"],
                    "metadata": {
                        "is_virtual": True,
                        "endpoint": "/api/v1/assistant",
                        "health": runtime_health,
                        "features": [
                            "chat",
                            "stream",
                            "rag",
                            "web_search",
                            "tools",
                            "image_generation",
                        ],
                    },
                }
            )

    # 从数据库获取注册的服务
    st = ServiceType(service_type) if service_type and service_type not in ["assistant"] else None
    db_services = await registry.list(service_type=st, tags=tags)

    # 转换数据库服务为字典
    db_service_list = []
    for s in db_services:
        aliases = [s.service_id, s.name]
        if not _service_visible_with_scopes(
            aliases=aliases,
            allowed_sources=allowed_sources,
            user_policy=user_policy,
            is_admin=is_admin,
        ):
            continue

        availability: dict | None = None
        connector_config = getattr(s, "connector_config", None) or {}
        if (
            transparent_proxy is not None
            and str(getattr(s, "service_type", "")).lower() == "langgraph"
            and str((connector_config or {}).get("proxy_mode") or (s.metadata or {}).get("proxy_mode") or "").lower() == "transparent"
        ):
            try:
                from ...proxy.config_loader import ProxyServiceConfig

                proxy_cfg = ProxyServiceConfig(
                    service_id=s.service_id,
                    service_name=s.name,
                    upstream_url=str((connector_config or {}).get("upstream_url") or (connector_config or {}).get("base_url") or ""),
                    upstream_urls=list((connector_config or {}).get("upstream_urls") or []),
                    assistant_id=(connector_config or {}).get("assistant_id"),
                    graph_id=(connector_config or {}).get("graph_id"),
                    metadata=dict(s.metadata or {}),
                    enabled=str(getattr(s, "status", "")).lower() == "active",
                )
                availability = await transparent_proxy.get_service_availability(proxy_cfg)
            except Exception:
                availability = None

        db_service_list.append(
            {
                "service_id": s.service_id,
                "name": s.name,
                "description": s.description,
                "version": s.version,
                "service_type": s.service_type,
                "supported_modes": s.supported_modes,
                "accepted_content_types": s.accepted_content_types,
                "output_content_types": s.output_content_types,
                "status": s.status,
                "tags": s.tags,
                "metadata": s.metadata,
                "availability_status": availability.get("availability_status") if availability else None,
                "last_health_check_at": availability.get("last_health_check_at") if availability else None,
                "last_health_error": availability.get("last_health_error") if availability else None,
            }
        )

    # 合并虚拟服务和数据库服务（虚拟服务在前）
    return virtual_services + db_service_list


@router.get("/services/{service_id}")
async def get_service(
    service_id: str,
    request: Request,
    registry: ServiceRegistry = Depends(get_registry),
    auth: AuthContext = Depends(get_auth_context),
    user: UserContext = Depends(get_user_context),
):
    """获取服务详情（需要 ServiceListRead capability，非管理员脱敏敏感字段）"""
    # 权限检查：ServiceListRead capability（兼容 legacy alias）
    _require_capability(request, auth, Capability.SERVICE_LIST_READ)

    service = await registry.get(service_id)
    if not service:
        raise HTTPException(status_code=404, detail="service not found")

    is_admin = "admin" in auth.roles
    allowed_sources, user_policy = await _load_access_constraints(request, user)
    if not _service_visible_with_scopes(
        aliases=[service.service_id, service.name],
        allowed_sources=allowed_sources,
        user_policy=user_policy,
        is_admin=is_admin,
    ):
        raise HTTPException(status_code=403, detail="service not accessible")

    # 非管理员需要脱敏敏感配置
    return _service_to_dict(service, mask_secrets=not is_admin)


@router.put("/services/{service_id}")
async def update_service(
    service_id: str,
    request: Request,
    patch: dict = Body(...),
    registry: ServiceRegistry = Depends(get_registry),
    auth: AuthContext = Depends(get_auth_context),
):
    # 权限：ServiceConfigWrite capability
    require_platform_admin(request, auth, Capability.SERVICE_CONFIG_WRITE)

    service = await registry.get(service_id)
    if not service:
        raise HTTPException(status_code=404, detail="service not found")

    # 禁止修改 service_id
    if "service_id" in patch and str(patch["service_id"]) != service_id:
        raise HTTPException(status_code=400, detail="service_id is immutable")

    # 只允许白名单字段更新
    filtered = {k: v for k, v in patch.items() if k in _SERVICE_MUTABLE_FIELDS}
    if not filtered:
        raise HTTPException(status_code=400, detail="no mutable fields provided")

    base = _service_to_dict(service)
    for k, v in filtered.items():
        base[k] = v

    _normalize_langgraph_connector_config(base)
    await _validate_langgraph_model_override(
        request,
        tenant_id=auth.tenant_id or "default",
        definition=base,
        previous_connector_config=service.connector_config or {},
    )

    try:
        updated = registry._service_from_dict(base)
        await registry.register(updated)
        _invalidate_proxy_config_cache(request, service_id)
    except HTTPException:
        raise
    except Exception as exc:
        raise HTTPException(status_code=400, detail=str(exc))

    return {"status": "success", "service_id": service_id}


@router.delete("/services/{service_id}")
async def delete_service(
    service_id: str,
    request: Request,
    registry: ServiceRegistry = Depends(get_registry),
    auth: AuthContext = Depends(get_auth_context),
):
    # 权限：ServiceConfigWrite capability
    require_platform_admin(request, auth, Capability.SERVICE_CONFIG_WRITE)

    deleted = False
    if hasattr(registry.storage, "delete"):
        deleted = await registry.storage.delete(service_id)
    else:
        if hasattr(registry.storage, "_services"):
            deleted = registry.storage._services.pop(service_id, None) is not None

    registry._cache.pop(service_id, None)
    _invalidate_proxy_config_cache(request, service_id)

    if not deleted:
        raise HTTPException(status_code=404, detail="service not found")

    return {"status": "success", "service_id": service_id}


@router.get("/services/{service_id}/schema")
async def get_service_schema(
    service_id: str,
    request: Request,
    registry: ServiceRegistry = Depends(get_registry),
    auth: AuthContext = Depends(get_auth_context),
):
    _require_capability(request, auth, Capability.SERVICE_LIST_READ)
    service = await registry.get(service_id)
    if not service:
        raise HTTPException(status_code=404, detail="service not found")
    return {
        "input_schema": service.input_schema,
        "output_schema": service.output_schema,
        "accepted_content_types": service.accepted_content_types,
        "output_content_types": service.output_content_types,
    }
