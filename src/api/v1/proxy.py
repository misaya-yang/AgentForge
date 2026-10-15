"""
透明代理路由

提供通配符路由 /proxy/{service_name}/{path:path}，支持：
- 动态路由转发
- SSE 流式传输
- 鉴权和限流
- 上下文注入
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import time
from typing import Any

from ai_gateway_core.logging import get_logger
from ai_gateway_core.proxy.sse_heartbeat import with_sse_heartbeat
from fastapi import APIRouter, Depends, HTTPException, Request, Response
from fastapi.responses import StreamingResponse

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
from ...core.auth.service_access_resolver import load_service_access_constraints
from ...core.auth.user_resolver import UserContext
from ...core.client_ip import get_client_ip_from_request
from ...core.gateway.admission import _finish_cleanup
from ...core.gateway.multi_dimension_rate_limiter import (
    MultiDimensionRateLimiter,
    RateLimitContext,
    RateLimitHeaders,
    RateLimitResult,
)
from ...core.gateway.rate_policy import RatePolicyResolver
from ...core.observability.metrics import get_metrics
from ...proxy import (
    ProxyConfigLoader,
    ProxyRequest,
    ProxyServiceConfig,
    RequestContext,
    TransparentProxy,
)
from ...proxy.langgraph_governance import (
    apply_langgraph_run_governance,
)
from ...proxy.langgraph_governance import (
    apply_quota_policy as _apply_quota_policy,
)
from ...proxy.langgraph_governance import (
    enforce_model_allowlist as _enforce_model_allowlist,
)
from ...proxy.langgraph_governance import (
    estimate_tokens_from_payload as _estimate_tokens_from_payload,  # noqa: F401
)
from ...proxy.langgraph_governance import (
    resolve_effective_provider as _resolve_effective_provider,
)
from ...proxy.langgraph_run_body import (
    billing_request_snapshot,
    encode_json_body,
    normalize_domain_policy,
    prepare_langgraph_run_body,
    should_prepare_langgraph_run_body,
)
from ...services.metrics.usage_parser import extract_model
from ..deps import (
    AuthContext,
    get_auth_context,
    get_rate_limiter,
    get_user_context,
)
from ._route_trace import current_trace_id

logger = get_logger(__name__)

router = APIRouter(prefix="/proxy", tags=["Transparent Proxy"])


class _OwnedProxyStreamResponse(StreamingResponse):
    """Close the eager upstream even if ASGI fails before body iteration."""

    def __init__(self, source, **kwargs):
        self._source = source
        super().__init__(with_sse_heartbeat(source), **kwargs)

    async def __call__(self, scope, receive, send):
        delivery = asyncio.create_task(super().__call__(scope, receive, send))
        closed = asyncio.create_task(self._source.wait_closed())
        try:
            await asyncio.wait({delivery, closed}, return_when=asyncio.FIRST_COMPLETED)
            if closed.done():
                # Lease loss must also interrupt a blocked downstream send.
                closed.result()
            await delivery
        finally:
            async def close():
                delivery.cancel()
                closed.cancel()
                await asyncio.gather(delivery, closed, return_exceptions=True)
                try:
                    await self.body_iterator.aclose()
                finally:
                    await self._source.aclose()
            await _finish_cleanup(asyncio.create_task(close()))


def _normalize_domain_policy(value: Any) -> str:
    return normalize_domain_policy(value)


async def _record_security_event(
    event_type: str,
    tenant_id: str,
    user_id: str,
    service_id: str | None,
    metadata: dict[str, Any] | None = None,
) -> None:
    try:
        from ...services.metrics import get_security_event_recorder

        recorder = get_security_event_recorder()
        await recorder.record_event(
            tenant_id=tenant_id or "public",
            user_id=user_id or None,
            service_id=service_id,
            event_type=event_type,
            metadata=metadata,
        )
    except Exception:
        pass


def _current_trace_id(request: Request) -> str:
    return current_trace_id(request)


def _record_rate_limit_decision(dimension: str, service_name: str, allowed: bool) -> None:
    try:
        metrics = get_metrics()
        result = "allowed" if allowed else "blocked"
        if hasattr(metrics.request_metrics, "record_rate_limit_decision"):
            metrics.request_metrics.record_rate_limit_decision(
                dimension=dimension,
                service_id=service_name,
                result=result,
            )
        if not allowed:
            metrics.request_metrics.record_rate_limit(
                dimension=dimension,
                service_id=service_name,
            )
    except Exception:
        # Metrics must not break proxy path.
        pass


# ============ 依赖注入 ============


def get_transparent_proxy(request: Request) -> TransparentProxy:
    """获取透明代理实例"""
    proxy = getattr(request.app.state, "transparent_proxy", None)
    if proxy is None:
        raise HTTPException(
            status_code=503,
            detail="Transparent proxy is not initialized",
        )
    return proxy


def get_proxy_config_loader(request: Request) -> ProxyConfigLoader:
    """获取代理配置加载器"""
    loader = getattr(request.app.state, "proxy_config_loader", None)
    if loader is None:
        raise HTTPException(
            status_code=503,
            detail="Proxy config loader is not initialized",
        )
    return loader


# ============ 权限和限流检查 ============


def _decode_json_body(body: bytes | None) -> Any | None:
    if not body:
        return None
    try:
        return json.loads(body.decode("utf-8"))
    except Exception:
        return None


async def _resolve_service_definition(registry, service_name: str):
    if not registry:
        return None
    service = await registry.get(service_name)
    if service:
        return service
    try:
        services = await registry.list()
    except Exception:
        return None
    for svc in services:
        if getattr(svc, "name", None) == service_name:
            return svc
    return None


def _service_allowed(allowed: list[str], candidates: set) -> bool:
    return service_scope_matches(allowed, list(candidates))


async def _load_service_access_constraints(
    request: Request,
    user: UserContext,
) -> tuple[list[tuple[str, list[str]]], ServiceAccessPolicy]:
    return await load_service_access_constraints(request, user)


async def _enforce_service_access_constraints(
    request: Request,
    service_name: str,
    service_aliases: set,
    user: UserContext,
    auth: AuthContext,
) -> None:
    allowed_sources, user_policy = await _load_service_access_constraints(request, user)

    for source, allowed in allowed_sources:
        if _service_allowed(allowed, service_aliases):
            continue
        logger.warning(
            f"[ProxyAuth] Service {service_name} blocked by {source} allowed_services for user {user.user_id}"
        )
        await _record_security_event(
            event_type="auth_failed",
            tenant_id=auth.tenant_id or user.tenant_id,
            user_id=user.user_id,
            service_id=service_name,
            metadata={
                "permission_check": {
                    "scope_source": source,
                    "scope": allowed,
                    "service_aliases": sorted(service_aliases),
                }
            },
        )
        raise HTTPException(
            status_code=403,
            detail=f"Permission denied: service {service_name} not in allowed services",
        )

    if "admin" in auth.roles:
        return

    permitted, reason = evaluate_service_access(user_policy, list(service_aliases))
    if permitted:
        return

    logger.warning(
        f"[ProxyAuth] Service {service_name} blocked by user service_access policy for user {user.user_id}: {reason}"
    )
    await _record_security_event(
        event_type="auth_failed",
        tenant_id=auth.tenant_id or user.tenant_id,
        user_id=user.user_id,
        service_id=service_name,
        metadata={
            "permission_check": {
                "scope_source": "user_policy",
                "reason": reason,
                "policy_mode": user_policy.mode.value,
                "allowed_services": list(user_policy.allowed_services),
                "denied_services": list(user_policy.denied_services),
                "service_aliases": sorted(service_aliases),
            }
        },
    )
    raise HTTPException(
        status_code=403,
        detail=f"Permission denied: service {service_name} blocked by user policy",
    )


async def check_service_authorization(
    request: Request,
    service_name: str,
    user: UserContext,
    auth: AuthContext,
) -> None:
    """
    检查用户对服务的访问权限

    验证顺序:
    1. 基础 capability 权限 (AgentInvoke)
    2. 服务级别认证配置 (allowed_roles, public)
    3. 用户/API Key 级别 allowed_services (如果配置)
    """
    # 1. Capability 检查（兼容 canonical + legacy alias）
    decision = check_capability(
        rbac=request.app.state.dispatcher.rbac,
        roles=auth.roles,
        permissions=auth.permissions,
        capability=Capability.AGENT_INVOKE,
    )
    if not decision.allowed:
        trace_id = _current_trace_id(request)
        await _record_security_event(
            event_type="auth_failed",
            tenant_id=auth.tenant_id or user.tenant_id,
            user_id=user.user_id,
            service_id=service_name,
            metadata={
                "permission_check": {
                    "required_capability": Capability.AGENT_INVOKE.value,
                    "required_permission": decision.required_permission,
                    "accepted_permissions": list(decision.accepted_permissions),
                    "trace_id": trace_id,
                }
            },
        )
        logger.warning(
            f"[ProxyAuth] User {user.user_id} lacks {decision.required_permission} "
            f"for {service_name} trace={trace_id}"
        )
        raise HTTPException(
            status_code=403,
            detail=build_permission_denied_detail(
                capability=Capability.AGENT_INVOKE,
                trace_id=trace_id,
            ),
        )

    # 2. 服务级别认证配置检查
    registry = getattr(request.app.state, "registry", None)
    service = await _resolve_service_definition(registry, service_name)
    service_aliases = {service_name}
    if service:
        request.state.service_id = getattr(service, "service_id", None) or service_name
        if getattr(service, "service_id", None):
            service_aliases.add(service.service_id)
        if getattr(service, "name", None):
            service_aliases.add(service.name)

        service_config = (
            service.get_service_config() if hasattr(service, "get_service_config") else None
        )
        if service_config and service_config.auth and service_config.auth.enabled:
            auth_config = service_config.auth

            # public 服务跳过服务级别检查，但仍需执行 allowed_services 约束
            if not auth_config.public:
                # require_auth
                if auth_config.require_auth and not user.is_authenticated:
                    await _record_security_event(
                        event_type="auth_failed",
                        tenant_id=auth.tenant_id or user.tenant_id,
                        user_id=user.user_id,
                        service_id=getattr(service, "service_id", None) or service_name,
                    )
                    raise HTTPException(
                        status_code=403,
                        detail=f"Permission denied: authentication required for {service_name}",
                    )

                # allowed_roles
                if auth_config.allowed_roles:
                    has_role = any(role in auth.roles for role in auth_config.allowed_roles)
                    # admin 始终有权限
                    if not has_role and "admin" not in auth.roles:
                        logger.warning(
                            f"[ProxyAuth] User {user.user_id} role not in allowed_roles for {service_name}"
                        )
                        await _record_security_event(
                            event_type="auth_failed",
                            tenant_id=auth.tenant_id or user.tenant_id,
                            user_id=user.user_id,
                            service_id=getattr(service, "service_id", None) or service_name,
                        )
                        raise HTTPException(
                            status_code=403,
                            detail=f"Permission denied: not authorized for service {service_name}",
                        )

                # allowed_api_keys (prefix allowlist)
                if auth_config.allowed_api_keys:
                    settings = getattr(request.app.state, "settings", None)
                    api_key_header = None
                    if settings and settings.authentication.api_key.enabled:
                        api_key_header = settings.authentication.api_key.header_name
                    api_key_value = request.headers.get(api_key_header) if api_key_header else None

                    if not api_key_value:
                        await _record_security_event(
                            event_type="auth_failed",
                            tenant_id=auth.tenant_id or user.tenant_id,
                            user_id=user.user_id,
                            service_id=getattr(service, "service_id", None) or service_name,
                        )
                        raise HTTPException(
                            status_code=403,
                            detail=f"Permission denied: API key required for service {service_name}",
                        )

                    if "*" not in auth_config.allowed_api_keys:
                        matched = any(
                            api_key_value.startswith(prefix)
                            for prefix in auth_config.allowed_api_keys
                        )
                        if not matched:
                            await _record_security_event(
                                event_type="auth_failed",
                                tenant_id=auth.tenant_id or user.tenant_id,
                                user_id=user.user_id,
                                service_id=getattr(service, "service_id", None) or service_name,
                            )
                            raise HTTPException(
                                status_code=403,
                                detail=f"Permission denied: API key not allowed for service {service_name}",
                            )

    # 3. 用户/API Key/租户级访问约束
    await _enforce_service_access_constraints(
        request=request,
        service_name=service_name,
        service_aliases=service_aliases,
        user=user,
        auth=auth,
    )


async def check_proxy_rate_limit(
    user: UserContext,
    rate_limiter: MultiDimensionRateLimiter | None,
    service_name: str,
    operation: str = "proxy",
    service_config: ProxyServiceConfig | None = None,
    request: Request | None = None,
) -> dict[str, str]:
    """检查代理限流"""
    if not rate_limiter:
        return {"X-Gateway-Policy-Exempt": "rate_limit"}

    if request is not None:
        resolver = getattr(request.app.state, "rate_policy_resolver", None)
        if resolver is None:
            resolver = RatePolicyResolver()
            request.app.state.rate_policy_resolver = resolver
        policies = await resolver.resolve(
            request=request,
            user=user,
            service_name=service_name,
            operation=operation,
            service_config=service_config,
        )
        admitted_results: dict[int, RateLimitResult] = {}
        policy_groups: list[list[Any]] = []
        for policy in policies:
            if policy_groups and policy_groups[-1][0].window == policy.window:
                policy_groups[-1].append(policy)
            else:
                policy_groups.append([policy])

        for window_policies in policy_groups:
            batch_check = getattr(rate_limiter, "check_custom_limits", None)
            if callable(batch_check):
                results = await batch_check(policies=window_policies)
            else:
                results = []
                for policy in window_policies:
                    result = await rate_limiter.check_custom_limit(
                        key=policy.key,
                        limit=policy.requests,
                        window=policy.window,
                        dimension=policy.dimension,
                    )
                    results.append(result)
                    if not result.allowed:
                        break

            for policy, result in zip(window_policies, results, strict=False):
                _record_rate_limit_decision(
                    result.dimension or policy.dimension, service_name, result.allowed
                )
                if not result.allowed:
                    await _record_security_event(
                        event_type="rate_limited",
                        tenant_id=user.tenant_id,
                        user_id=user.user_id,
                        service_id=service_name,
                        metadata={
                            "permission_check": {
                                "dimension": result.dimension,
                                "limit": result.limit,
                                "remaining": result.remaining,
                                "retry_after": result.retry_after,
                            }
                        },
                    )
                    raise HTTPException(
                        status_code=429,
                        detail=RateLimitHeaders.build_exceeded_response(result),
                        headers=RateLimitHeaders.build(result),
                    )
                admitted_results[id(policy)] = result
        if policies:
            return RateLimitHeaders.build(admitted_results[id(policies[-1])])
    elif (
        service_config
        and service_config.rate_limit_enabled
        and int(service_config.rate_limit_requests or 0) > 0
        and int(service_config.rate_limit_window or 0) > 0
    ):
        tenant_scope = (user.tenant_id or "public").strip() or "public"
        subject = (user.user_id or user.ip or "anonymous").strip() or "anonymous"
        safe_operation = str(operation or "proxy").strip() or "proxy"
        key = f"ratelimit:service:{service_name}:{tenant_scope}:{subject}:{safe_operation}"
        result = await rate_limiter.check_custom_limit(
            key=key,
            limit=int(service_config.rate_limit_requests),
            window=int(service_config.rate_limit_window),
            dimension=f"service:{service_name}",
        )
        _record_rate_limit_decision(result.dimension or "service", service_name, result.allowed)
        if not result.allowed:
            await _record_security_event(
                event_type="rate_limited",
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                service_id=service_name,
                metadata={
                    "permission_check": {
                        "dimension": result.dimension,
                        "limit": result.limit,
                        "remaining": result.remaining,
                        "retry_after": result.retry_after,
                    }
                },
            )
            raise HTTPException(
                status_code=429,
                detail=RateLimitHeaders.build_exceeded_response(result),
                headers=RateLimitHeaders.build(result),
            )
        return RateLimitHeaders.build(result)

    context = RateLimitContext.from_user_context(
        user=user,
        assistant_id=service_name,  # 复用 assistant_id 作为服务标识
        operation=operation,
    )

    result = await rate_limiter.check(context)
    _record_rate_limit_decision(result.dimension or "composite", service_name, result.allowed)
    if not result.allowed:
        await _record_security_event(
            event_type="rate_limited",
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            service_id=service_name,
        )
        raise HTTPException(
            status_code=429,
            detail=RateLimitHeaders.build_exceeded_response(result),
            headers=RateLimitHeaders.build(result),
        )
    return (
        RateLimitHeaders.build(result)
        if result.limit > 0
        else {"X-Gateway-Policy-Exempt": "rate_limit"}
    )


# ============ 主路由处理 ============


@router.api_route(
    "/{service_name}/{path:path}",
    methods=["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"],
    summary="透明代理",
    description="""
    透明代理路由，将请求转发至目标服务。

    - `service_name`: 服务名称（对应数据库中的 service_id 或 name）
    - `path`: 请求路径（将被转发至上游服务）

    支持：
    - 所有 HTTP 方法
    - SSE 流式响应（自动检测）
    - 请求体透传
    - 查询参数透传
    """,
)
async def transparent_proxy_handler(
    service_name: str,
    path: str,
    request: Request,
    proxy: TransparentProxy = Depends(get_transparent_proxy),
    config_loader: ProxyConfigLoader = Depends(get_proxy_config_loader),
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
    rate_limiter: MultiDimensionRateLimiter | None = Depends(get_rate_limiter),
):
    """
    透明代理主处理函数

    处理流程：
    1. 权限检查（RBAC + 服务级别 + allowed_services）
    2. 限流检查
    3. 提取请求上下文
    4. 构建代理请求
    5. 执行代理并返回响应
    """
    if path == "_health":
        await check_service_authorization(request, service_name, user, auth)
        return await proxy_service_health(
            service_name=service_name,
            request=request,
            proxy=proxy,
            config_loader=config_loader,
            user=user,
            auth=auth,
        )
    if path == "_selftest":
        return await proxy_service_selftest(
            service_name=service_name,
            request=request,
            proxy=proxy,
            config_loader=config_loader,
            user=user,
            auth=auth,
            rate_limiter=rate_limiter,
        )

    # Performance timing
    t_start = time.perf_counter()
    t_auth_done = t_start  # User context already resolved via Depends

    # 1. 权限检查
    await check_service_authorization(request, service_name, user, auth)
    t_auth_check = time.perf_counter()

    # 2. 检测操作类型（用于限流）
    operation = TransparentProxy.detect_operation_type(request.method, path)

    # 3. 读取服务配置（用于限流覆盖、默认模型、策略判定）
    service_config = await config_loader.get_config(service_name)
    t_config = time.perf_counter()

    # 4. 限流检查
    rate_limit_headers = await check_proxy_rate_limit(
        user=user,
        rate_limiter=rate_limiter,
        service_name=service_name,
        operation=operation,
        service_config=service_config,
        request=request,
    )
    t_rate_limit = time.perf_counter()

    # 5. 提取请求上下文
    context = _build_request_context(request, user)
    t_context = time.perf_counter()

    # 6. 读取请求体
    body = await request.body() if request.method in ("POST", "PUT", "PATCH") else None
    parsed_body: dict[str, Any] | None = None
    langgraph_body_prepared = False
    tenant_id = auth.tenant_id or user.tenant_id or "default"

    body_changed = False
    if should_prepare_langgraph_run_body(request.method, path, service_config):
        body, parsed_body, body_changed = prepare_langgraph_run_body(
            body=body,
            method=request.method,
            path=path,
            request=request,
            user=user,
            auth=auth,
            service_config=service_config,
        )
        langgraph_body_prepared = True
    elif body and request.method in ("POST", "PUT", "PATCH"):
        decoded = _decode_json_body(body)
        if isinstance(decoded, dict):
            parsed_body = decoded
    t_body = time.perf_counter()

    # 7. 解析请求模型并应用服务默认值
    request_payload = parsed_body if parsed_body is not None else _decode_json_body(body)
    requested_model = extract_model(request_payload)
    if not requested_model and service_config:
        requested_model = service_config.default_model

    # 8-9. LangGraph governance or generic quota for non-run bodies
    effective_model: str | None = None
    effective_provider: str | None = None
    if langgraph_body_prepared and isinstance(parsed_body, dict):
        effective_model, effective_provider, governance_mutated = await apply_langgraph_run_governance(
            request=request,
            user=user,
            payload=parsed_body,
            path=path,
            service_config=service_config,
            auth=auth,
            service_name=service_name,
            operation=operation,
            tenant_id=tenant_id,
        )
        if body_changed or governance_mutated:
            body = encode_json_body(parsed_body)
    else:
        await _enforce_model_allowlist(
            request=request,
            service_name=service_name,
            user=user,
            auth=auth,
            model=requested_model,
        )
        defer_encode = isinstance(parsed_body, dict)
        body, effective_model, quota_mutated = await _apply_quota_policy(
            request=request,
            user=user,
            auth=auth,
            service_name=service_name,
            operation=operation,
            path=path,
            body=body,
            model_hint=requested_model,
            payload=parsed_body if defer_encode else None,
            defer_encode=defer_encode,
        )
        if defer_encode and quota_mutated and isinstance(parsed_body, dict):
            body = encode_json_body(parsed_body)
        effective_provider = _resolve_effective_provider(parsed_body, service_config)
        if effective_model:
            request.state.effective_model = effective_model
        if effective_provider:
            request.state.effective_provider = effective_provider
    t_policy = time.perf_counter()

    # 10b. 检查是否期望流式响应
    wants_stream = _wants_streaming(request, path)

    # 11. 构建代理请求
    proxy_request = ProxyRequest(
        service_name=service_name,
        path=path,
        method=request.method,
        body=body,
        query_params=dict(request.query_params),
        context=context,
        stream=wants_stream,
        parsed_body=billing_request_snapshot(parsed_body),
        langgraph_body_prepared=langgraph_body_prepared,
        preloaded_config=service_config,
        effective_model=effective_model,
        effective_provider=effective_provider,
    )
    t_build = time.perf_counter()

    # Performance logging
    logger.info(
        f"[ProxyRoute][TIMING] {request.method} /proxy/{service_name}/{path} "
        f"auth_check={((t_auth_check - t_auth_done) * 1000):.1f}ms "
        f"config={((t_config - t_auth_check) * 1000):.1f}ms "
        f"rate_limit={((t_rate_limit - t_config) * 1000):.1f}ms "
        f"context={((t_context - t_rate_limit) * 1000):.1f}ms "
        f"body_read={((t_body - t_context) * 1000):.1f}ms "
        f"policy={((t_policy - t_body) * 1000):.1f}ms "
        f"build={((t_build - t_policy) * 1000):.1f}ms "
        f"total_prep={((t_build - t_start) * 1000):.1f}ms"
    )

    # 10. 执行代理
    logger.info(
        f"[ProxyRoute] {request.method} /proxy/{service_name}/{path} "
        f"user={user.user_id} op={operation} stream={wants_stream}"
    )

    response = await proxy.proxy(proxy_request)
    t_proxy_done = time.perf_counter()
    logger.info(
        f"[ProxyRoute][TIMING] proxy_call={((t_proxy_done - t_build) * 1000):.1f}ms "
        f"total={((t_proxy_done - t_start) * 1000):.1f}ms "
        f"status={response.status_code} streaming={response.is_streaming}"
    )

    # 8. 处理网关内部错误（如服务不存在、配置错误）
    # 注意：上游 4xx/5xx 错误不在此处理，直接透传
    if response.error:
        raise HTTPException(
            status_code=response.status_code,
            detail=response.error,
        )

    # 9. 返回响应（包括上游的 4xx/5xx 错误，原样透传）
    if response.is_streaming and response.stream:
        headers = {**response.headers, **rate_limit_headers}
        return _OwnedProxyStreamResponse(
            response.stream,
            status_code=response.status_code,
            headers=headers,
            media_type="text/event-stream",
        )
    else:
        # 确定 content-type，保留原始响应的 content-type
        content_type = response.headers.get("content-type", "application/json")
        headers = {**response.headers, **rate_limit_headers}

        # 错误透传：即使是 4xx/5xx，也原样返回上游的响应内容
        return Response(
            content=response.body or b"",
            status_code=response.status_code,
            headers=headers,
            media_type=content_type,
        )


@router.api_route(
    "/{service_name}",
    methods=["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"],
    summary="透明代理（根路径）",
    description="透明代理路由，转发至服务根路径。",
)
async def transparent_proxy_root_handler(
    service_name: str,
    request: Request,
    proxy: TransparentProxy = Depends(get_transparent_proxy),
    config_loader: ProxyConfigLoader = Depends(get_proxy_config_loader),
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
    rate_limiter: MultiDimensionRateLimiter | None = Depends(get_rate_limiter),
):
    """处理根路径请求"""
    return await transparent_proxy_handler(
        service_name=service_name,
        path="",
        request=request,
        proxy=proxy,
        config_loader=config_loader,
        user=user,
        auth=auth,
        rate_limiter=rate_limiter,
    )


# ============ 服务发现端点 ============


def _safe_model_override_debug(model_override: Any) -> dict[str, Any]:
    """Expose only non-secret model override fields for frontend diagnostics."""
    source = model_override if isinstance(model_override, dict) else {}
    provider_id = str(source.get("provider_id") or "").strip() or None
    model_id = str(source.get("model_id") or "").strip() or None
    cache_epoch = source.get("cache_epoch")
    if not isinstance(cache_epoch, (int, str)):
        cache_epoch = None
    temperature = source.get("temperature")
    if isinstance(temperature, bool) or not isinstance(temperature, (int, float)):
        temperature = None

    failover = source.get("failover")
    safe_failover: dict[str, Any] | None = None
    if isinstance(failover, dict):
        raw_candidates = failover.get("candidates")
        safe_candidates = []
        if isinstance(raw_candidates, list):
            for raw_candidate in raw_candidates:
                if not isinstance(raw_candidate, dict):
                    continue
                safe_candidates.append(
                    {
                        "provider_id": str(raw_candidate.get("provider_id") or "").strip()
                        or None,
                        "model_id": str(raw_candidate.get("model_id") or "").strip() or None,
                    }
                )
        safe_failover = {
            "enabled": bool(failover.get("enabled")),
            "max_attempts": failover.get("max_attempts")
            if isinstance(failover.get("max_attempts"), int)
            else None,
            "candidates": safe_candidates,
            "candidate_count": len(safe_candidates),
        }

    debug = {
        "enabled": bool(source.get("enabled")),
        "provider_id": provider_id,
        "model_id": model_id,
        "cache_epoch": cache_epoch,
        "temperature": temperature,
    }
    if safe_failover is not None:
        debug["failover"] = safe_failover
    return debug


@router.get(
    "",
    summary="列出代理服务",
    description="列出所有可用的透明代理服务。",
)
async def list_proxy_services(
    request: Request,
    proxy: TransparentProxy = Depends(get_transparent_proxy),
    config_loader: ProxyConfigLoader = Depends(get_proxy_config_loader),
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    """列出可用的代理服务"""
    # 权限检查：需要 AgentInvoke capability（兼容 legacy alias）
    decision = check_capability(
        rbac=request.app.state.dispatcher.rbac,
        roles=auth.roles,
        permissions=auth.permissions,
        capability=Capability.AGENT_INVOKE,
    )
    if not decision.allowed:
        raise HTTPException(
            status_code=403,
            detail=build_permission_denied_detail(
                capability=Capability.AGENT_INVOKE,
                trace_id=_current_trace_id(request),
            ),
        )

    services = await config_loader.list_services()
    is_admin = "admin" in auth.roles
    allowed_sources, user_policy = await _load_service_access_constraints(request, user)

    visible_services: list[dict[str, Any]] = []
    for svc in services:
        aliases = {
            str(getattr(svc, "service_id", "") or "").strip(),
            str(getattr(svc, "service_name", "") or "").strip(),
        }
        aliases = {alias for alias in aliases if alias}

        blocked = False
        for _, allowed in allowed_sources:
            if _service_allowed(allowed, aliases):
                continue
            blocked = True
            break
        if blocked:
            continue

        if not is_admin:
            permitted, _ = evaluate_service_access(user_policy, list(aliases))
            if not permitted:
                continue

        raw_metadata = svc.metadata if isinstance(svc.metadata, dict) else {}
        adapter_type = str(raw_metadata.get("adapter_type") or "").strip().lower()
        inferred_service_type = str(raw_metadata.get("service_type") or "").strip().lower()
        if not inferred_service_type:
            if adapter_type == "langgraph" or bool(
                (svc.assistant_id or "").strip() or (svc.graph_id or "").strip()
            ):
                inferred_service_type = "langgraph"
            else:
                inferred_service_type = "proxy"

        safe_metadata: dict[str, Any] = {}
        effective_adapter_type = adapter_type or (
            "langgraph" if inferred_service_type == "langgraph" else ""
        )
        if effective_adapter_type:
            safe_metadata["adapter_type"] = effective_adapter_type
        proxy_mode = str(raw_metadata.get("proxy_mode") or "").strip().lower()
        if not proxy_mode and inferred_service_type == "langgraph":
            proxy_mode = "transparent"
        if proxy_mode:
            safe_metadata["proxy_mode"] = proxy_mode
        domain_policy = _normalize_domain_policy(raw_metadata.get("domain_policy"))
        if domain_policy != "none":
            safe_metadata["domain_policy"] = domain_policy
        ui_preferences = raw_metadata.get("ui_preferences")
        if isinstance(ui_preferences, dict):
            safe_metadata["ui_preferences"] = dict(ui_preferences)
        safe_metadata["model_override"] = _safe_model_override_debug(svc.model_override)
        availability = await proxy.get_service_availability(svc)

        visible_services.append(
            {
                "service_id": svc.service_id,
                "service_name": svc.service_name,
                "service_type": inferred_service_type,
                "metadata": safe_metadata,
                # 仅管理员可见完整 URL
                "upstream_url": svc.upstream_url if is_admin else None,
                "graph_id": svc.graph_id if is_admin else None,
                "assistant_id": svc.assistant_id if is_admin else None,
                "enabled": svc.enabled,
                "availability_status": availability.get("availability_status", "unknown"),
                "last_health_check_at": availability.get("last_health_check_at"),
                "last_health_error": availability.get("last_health_error"),
            }
        )

    # 非管理员只能看到基本信息，管理员可以看到完整信息
    return {
        "services": visible_services,
        "count": len(visible_services),
    }


@router.get(
    "/{service_name}/_health",
    summary="服务健康检查",
    description="检查指定服务的健康状态。",
)
async def proxy_service_health(
    service_name: str,
    request: Request,
    proxy: TransparentProxy = Depends(get_transparent_proxy),
    config_loader: ProxyConfigLoader = Depends(get_proxy_config_loader),
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    """检查服务健康状态"""
    await check_service_authorization(request, service_name, user, auth)
    healthy, _message = await proxy.health_check(service_name)
    config = await config_loader.get_config(service_name)
    snapshot = await proxy.get_service_availability(config) if config else {}

    if healthy:
        return {
            "status": "healthy",
            "service": service_name,
            "message": "HEALTHY",
            "availability_status": snapshot.get("availability_status"),
            "last_health_check_at": snapshot.get("last_health_check_at"),
            "last_health_error": snapshot.get("last_health_error"),
        }
    else:
        raise HTTPException(
            status_code=503,
            detail={
            "status": "unhealthy",
            "service": service_name,
            "message": "UNAVAILABLE",
                "availability_status": snapshot.get("availability_status"),
                "last_health_check_at": snapshot.get("last_health_check_at"),
                "last_health_error": snapshot.get("last_health_error"),
            },
        )


@router.get(
    "/{service_name}/_selftest",
    summary="代理自检",
    description="验证鉴权头透传与 SSE 流式输出是否正常。",
)
async def proxy_service_selftest(
    service_name: str,
    request: Request,
    proxy: TransparentProxy = Depends(get_transparent_proxy),
    config_loader: ProxyConfigLoader = Depends(get_proxy_config_loader),
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
    rate_limiter: MultiDimensionRateLimiter | None = Depends(get_rate_limiter),
):
    # 权限检查：仅管理员可访问 _selftest 端点
    if "admin" not in auth.roles:
        raise HTTPException(status_code=403, detail="Admin access required for selftest endpoint")

    config = await config_loader.get_config(service_name)
    await check_proxy_rate_limit(
        user=user,
        rate_limiter=rate_limiter,
        service_name=service_name,
        operation="proxy_selftest",
        service_config=config,
        request=request,
    )

    context = _build_request_context(request, user)
    auth_present = any(k.lower() == "authorization" for k in request.headers)

    result: dict[str, Any] = {
        "service": service_name,
        "auth_header_present": auth_present,
    }

    if config:
        # 脱敏处理：只显示部分信息，避免暴露敏感配置
        masked_assistant_id = (
            (config.assistant_id[:8] + "...")
            if config.assistant_id and len(config.assistant_id) > 8
            else config.assistant_id
        )
        # 只显示 host 部分
        masked_upstream = None
        if config.upstream_url:
            try:
                from urllib.parse import urlparse

                parsed = urlparse(config.upstream_url)
                masked_upstream = parsed.netloc
            except Exception:
                masked_upstream = "[masked]"
        result.update(
            {
                "assistant_id": masked_assistant_id,
                "upstream_host": masked_upstream,
                "enabled": config.enabled,
            }
        )

    # 1) Basic upstream auth/route check (assistants list)
    # LangGraph /assistants is POST in current Agent Server API.
    list_request = ProxyRequest(
        service_name=service_name,
        path="assistants/search",
        method="POST",
        body=json.dumps({}).encode("utf-8"),
        query_params={},
        context=context,
        stream=False,
    )
    list_response = await proxy.proxy(list_request)
    list_preview = None
    if list_response.body:
        try:
            list_preview = list_response.body[:200].decode("utf-8", errors="ignore")
        except Exception:
            list_preview = None
    result["assistant_list"] = {
        "status_code": list_response.status_code,
        "ok": list_response.status_code < 500 and not list_response.error,
        "error": list_response.error,
        "body_preview": list_preview,
    }

    # 2) Streaming check (runs/stream)
    payload = {
        "input": {"messages": [{"role": "user", "content": "ping"}]},
    }
    stream_request = ProxyRequest(
        service_name=service_name,
        path="runs/stream",
        method="POST",
        body=json.dumps(payload).encode("utf-8"),
        query_params={},
        context=context,
        stream=True,
    )

    stream_response = await proxy.proxy(stream_request)
    aiter = None
    if stream_response.is_streaming and stream_response.stream:
        t0 = time.perf_counter()
        try:
            aiter = stream_response.stream.__aiter__()
            chunk = await asyncio.wait_for(aiter.__anext__(), timeout=5.0)
            first_ms = (time.perf_counter() - t0) * 1000
            result["stream"] = {
                "ok": True,
                "first_chunk_ms": round(first_ms, 2),
                "chunk_bytes": len(chunk) if isinstance(chunk, (bytes, bytearray)) else None,
            }
        except StopAsyncIteration:
            result["stream"] = {"ok": False, "error": "no chunks"}
        except asyncio.TimeoutError:
            result["stream"] = {"ok": False, "error": "timeout waiting for first chunk"}
        except Exception as exc:
            result["stream"] = {"ok": False, "error": str(exc)}
        finally:
            # 显式关闭流，避免连接泄露
            if aiter and hasattr(aiter, "aclose"):
                with contextlib.suppress(Exception):
                    await aiter.aclose()
    else:
        result["stream"] = {
            "ok": False,
            "status_code": stream_response.status_code,
            "error": stream_response.error or "not streaming",
        }

    return result


# ============ 辅助函数 ============


def _build_request_context(request: Request, user: UserContext) -> RequestContext:
    """从请求构建上下文"""
    # 提取原始请求头
    original_headers = dict(request.headers)

    # 提取客户端 IP
    client_ip = get_client_ip_from_request(request)

    # 从 request.state 获取追踪信息
    request_id = getattr(request.state, "request_id", "")
    trace_id = getattr(request.state, "trace_id", "")
    span_id = getattr(request.state, "span_id", "")
    traceparent = getattr(request.state, "traceparent", "")
    api_key_id = str(getattr(request.state, "api_key_hash", "") or "")

    return RequestContext(
        user_id=user.user_id,
        api_key_id=api_key_id,
        tenant_id=user.tenant_id,
        user_tier=user.tier,
        is_authenticated=user.is_authenticated,
        roles=list(user.roles) if hasattr(user, "roles") else [],
        request_id=request_id,
        trace_id=trace_id,
        span_id=span_id,
        traceparent=traceparent,
        client_ip=client_ip,
        user_agent=request.headers.get("user-agent", ""),
        original_headers=original_headers,
    )


def _wants_streaming(request: Request, path: str) -> bool:
    """判断是否期望流式响应"""
    # 检查 Accept 头
    accept = request.headers.get("accept", "")
    if "text/event-stream" in accept:
        return True

    # 检查路径
    path_lower = path.lower()
    streaming_indicators = [
        "/stream",
        "/runs/stream",
        "/sse",
        "stream=true",
    ]

    for indicator in streaming_indicators:
        if indicator in path_lower:
            return True

    # 检查查询参数
    return request.query_params.get("stream") in ("true", "1", "yes")
