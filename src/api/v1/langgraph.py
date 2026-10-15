"""
LangGraph API 路由

代理 LangGraph Server 官方 API：
- Assistants: /assistants/*
- Threads: /threads/*
- Runs: /threads/{thread_id}/runs/*, /runs/*
- Store: /store/*
"""

from __future__ import annotations

import json
import os
import time
from typing import Any

from ai_gateway_core.proxy.sse_heartbeat import (
    DEFAULT_HEARTBEAT_INTERVAL_S,
    with_sse_heartbeat,
)
from fastapi import APIRouter, Depends, HTTPException, Query, Request
from fastapi.responses import Response, StreamingResponse
from pydantic import BaseModel, Field
from sse_starlette.sse import EventSourceResponse

from ...adapters.langgraph_proxy import (
    AssistantAccessDeniedError,
    AssistantNotFoundError,
    ForbiddenError,
    LangGraphProxy,
    NoHealthyInstanceError,
    QuotaExceededError,
    ThreadNotFoundError,
)
from ...core.auth.user_resolver import UserContext
from ...core.gateway.multi_dimension_rate_limiter import MultiDimensionRateLimiter
from ...proxy.context_injector import ContextInjector, RequestContext
from ...proxy.langgraph_run_body import (
    decode_json_body,
    is_langgraph_run_path,
    prepare_langgraph_run_body_for_passthrough,
)
from ._langgraph_route_utils import check_langgraph_rate_limit, handle_langgraph_proxy_error
from ...services.eval.langgraph_trace_capture import record_langgraph_proxy_trace
from ..deps import AuthContext, get_auth_context, get_rate_limiter, get_user_context, require_langgraph_proxy

router = APIRouter(prefix="/langgraph", tags=["LangGraph"])

_PASSTHROUGH_BLOCKED_HEADERS = (
    ContextInjector.BLOCKED_HEADERS
    | ContextInjector.SENSITIVE_HEADERS
    | {
        "authorization",
        "cookie",
        "x-api-key",
    }
)


def _build_langgraph_passthrough_headers(
    request: Request,
    user: UserContext,
    proxy: LangGraphProxy,
) -> dict[str, str]:
    original_headers = {
        name: value
        for name, value in request.headers.items()
        if name.lower() not in _PASSTHROUGH_BLOCKED_HEADERS
    }
    context = RequestContext(
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        user_tier=user.tier,
        is_authenticated=user.is_authenticated,
        roles=user.roles or [],
        request_id=getattr(request.state, "request_id", "") or "",
        trace_id=getattr(request.state, "trace_id", "") or "",
        traceparent=request.headers.get("traceparent", ""),
        client_ip=user.ip or getattr(getattr(request, "client", None), "host", "") or "",
        user_agent=request.headers.get("user-agent", ""),
        original_headers=original_headers,
    )

    token = proxy.auth_token or os.environ.get("LANGGRAPH_AUTH_TOKEN", "")
    injector = ContextInjector(
        inject_user_info=True,
        inject_request_info=True,
        forward_auth=False,
        forward_all_headers=True,
    )
    headers = injector.build_headers(
        context,
        service_auth_token=token or None,
        forward_all_headers=True,
    )
    if token:
        headers["Authorization"] = token if token.startswith("Bearer ") else f"Bearer {token}"
    return headers


# ============ Pydantic 模型 ============


class AssistantCreate(BaseModel):
    graph_id: str
    config: dict[str, Any] | None = None
    metadata: dict[str, Any] | None = None
    name: str | None = None


class ThreadCreate(BaseModel):
    metadata: dict[str, Any] | None = None
    if_exists: str = "do_nothing"


class ThreadUpdate(BaseModel):
    metadata: dict[str, Any] | None = None


class ThreadStateUpdate(BaseModel):
    values: dict[str, Any]
    as_node: str | None = None


class RunCreate(BaseModel):
    assistant_id: str
    input: dict[str, Any] = Field(default_factory=dict)
    config: dict[str, Any] | None = None
    metadata: dict[str, Any] | None = None
    webhook: str | None = None
    interrupt_before: list[str] | None = None
    interrupt_after: list[str] | None = None
    stream_mode: list[str] | None = None


class StoreItem(BaseModel):
    namespace: list[str]
    key: str
    value: dict[str, Any]


def handle_proxy_error(e: Exception) -> None:
    handle_langgraph_proxy_error(e)


# ============ Assistants API ============


@router.get("/assistants")
async def list_assistants(
    limit: int = Query(10, ge=1, le=100),
    offset: int = Query(0, ge=0),
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """列出可用的 Assistants"""
    try:
        return await proxy.list_assistants(user, limit=limit, offset=offset)
    except Exception as e:
        handle_proxy_error(e)


@router.get("/assistants/{assistant_id}")
async def get_assistant(
    assistant_id: str,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """获取 Assistant 详情"""
    try:
        return await proxy.get_assistant(user, assistant_id)
    except Exception as e:
        handle_proxy_error(e)


@router.post("/assistants")
async def create_assistant(
    data: AssistantCreate,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """创建自定义 Assistant（需要企业版权限）"""
    try:
        return await proxy.create_assistant(
            user=user,
            graph_id=data.graph_id,
            config=data.config,
            metadata=data.metadata,
            name=data.name,
        )
    except Exception as e:
        handle_proxy_error(e)


# ============ Threads API ============


@router.post("/threads")
async def create_thread(
    data: ThreadCreate = None,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
    rate_limiter: MultiDimensionRateLimiter = Depends(get_rate_limiter),
):
    """创建 Thread"""
    await check_langgraph_rate_limit(user, rate_limiter, operation="thread_create")

    try:
        return await proxy.create_thread(
            user=user,
            metadata=data.metadata if data else None,
            if_exists=data.if_exists if data else "do_nothing",
        )
    except Exception as e:
        handle_proxy_error(e)


@router.get("/threads/{thread_id}")
async def get_thread(
    thread_id: str,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """获取 Thread"""
    try:
        return await proxy.get_thread(user, thread_id)
    except Exception as e:
        handle_proxy_error(e)


@router.patch("/threads/{thread_id}")
async def update_thread(
    thread_id: str,
    data: ThreadUpdate,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """更新 Thread"""
    try:
        return await proxy.update_thread(user, thread_id, metadata=data.metadata)
    except Exception as e:
        handle_proxy_error(e)


@router.delete("/threads/{thread_id}")
async def delete_thread(
    thread_id: str,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """删除 Thread"""
    try:
        await proxy.delete_thread(user, thread_id)
        return {"status": "ok"}
    except Exception as e:
        handle_proxy_error(e)


@router.post("/threads/search")
async def search_threads(
    metadata: dict[str, Any] | None = None,
    limit: int = Query(10, ge=1, le=100),
    offset: int = Query(0, ge=0),
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """搜索用户的 Threads"""
    try:
        return await proxy.search_threads(
            user=user,
            metadata=metadata,
            limit=limit,
            offset=offset,
        )
    except Exception as e:
        handle_proxy_error(e)


@router.get("/threads/{thread_id}/state")
async def get_thread_state(
    thread_id: str,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """获取 Thread 状态"""
    try:
        return await proxy.get_thread_state(user, thread_id)
    except Exception as e:
        handle_proxy_error(e)


@router.post("/threads/{thread_id}/state")
async def update_thread_state(
    thread_id: str,
    data: ThreadStateUpdate,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """更新 Thread 状态"""
    try:
        return await proxy.update_thread_state(
            user=user,
            thread_id=thread_id,
            values=data.values,
            as_node=data.as_node,
        )
    except Exception as e:
        handle_proxy_error(e)


@router.get("/threads/{thread_id}/history")
async def get_thread_history(
    thread_id: str,
    limit: int = Query(10, ge=1, le=100),
    before: str | None = None,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """获取 Thread 历史"""
    try:
        return await proxy.get_thread_history(
            user=user,
            thread_id=thread_id,
            limit=limit,
            before=before,
        )
    except Exception as e:
        handle_proxy_error(e)


# ============ Runs API ============


@router.post("/threads/{thread_id}/runs")
async def create_run(
    thread_id: str,
    data: RunCreate,
    request: Request,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
    _auth: AuthContext = Depends(get_auth_context),
    rate_limiter: MultiDimensionRateLimiter = Depends(get_rate_limiter),
):
    """创建 Run"""
    await check_langgraph_rate_limit(
        user,
        rate_limiter,
        assistant_id=data.assistant_id,
        operation="run_create",
    )

    try:
        return await proxy.create_run(
            user=user,
            thread_id=thread_id,
            assistant_id=data.assistant_id,
            input_data=data.input,
            config=data.config,
            metadata=data.metadata,
            webhook=data.webhook,
            interrupt_before=data.interrupt_before,
            interrupt_after=data.interrupt_after,
            http_request=request,
        )
    except Exception as e:
        handle_proxy_error(e)


@router.post("/threads/{thread_id}/runs/wait")
async def create_run_wait(
    thread_id: str,
    data: RunCreate,
    request: Request,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
    rate_limiter: MultiDimensionRateLimiter = Depends(get_rate_limiter),
):
    """创建 Run 并等待完成"""
    await check_langgraph_rate_limit(
        user,
        rate_limiter,
        assistant_id=data.assistant_id,
        operation="run_create",
    )

    try:
        return await proxy.create_run_wait(
            user=user,
            thread_id=thread_id,
            assistant_id=data.assistant_id,
            input_data=data.input,
            config=data.config,
            metadata=data.metadata,
            http_request=request,
        )
    except Exception as e:
        handle_proxy_error(e)


@router.post("/threads/{thread_id}/runs/stream")
async def stream_run(
    thread_id: str,
    data: RunCreate,
    request: Request,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
    rate_limiter: MultiDimensionRateLimiter = Depends(get_rate_limiter),
):
    """流式执行 Run"""
    rate_limit_headers = await check_langgraph_rate_limit(
        user,
        rate_limiter,
        assistant_id=data.assistant_id,
        operation="run_create",
    )

    async def event_generator():
        try:
            async for chunk in proxy.stream_run(
                user=user,
                thread_id=thread_id,
                assistant_id=data.assistant_id,
                input_data=data.input,
                config=data.config,
                metadata=data.metadata,
                stream_mode=data.stream_mode,
                http_request=request,
            ):
                yield {
                    "event": chunk.get("event", "message"),
                    "data": json.dumps(chunk.get("data", {})),
                }
        except HTTPException:
            raise
        except Exception as e:
            yield {
                "event": "error",
                "data": json.dumps({"error": str(e)}),
            }

    return EventSourceResponse(
        event_generator(),
        headers=rate_limit_headers,
        ping=int(DEFAULT_HEARTBEAT_INTERVAL_S),
    )


@router.get("/threads/{thread_id}/runs")
async def list_runs(
    thread_id: str,
    limit: int = Query(10, ge=1, le=100),
    offset: int = Query(0, ge=0),
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """列出 Thread 的 Runs"""
    try:
        return await proxy.list_runs(
            user=user,
            thread_id=thread_id,
            limit=limit,
            offset=offset,
        )
    except Exception as e:
        handle_proxy_error(e)


@router.get("/threads/{thread_id}/runs/{run_id}")
async def get_run(
    thread_id: str,
    run_id: str,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """获取 Run 详情"""
    try:
        return await proxy.get_run(user, thread_id, run_id)
    except Exception as e:
        handle_proxy_error(e)


@router.post("/threads/{thread_id}/runs/{run_id}/cancel")
async def cancel_run(
    thread_id: str,
    run_id: str,
    wait: bool = Query(False),
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """取消 Run"""
    try:
        await proxy.cancel_run(user, thread_id, run_id, wait=wait)
        return {"status": "ok"}
    except Exception as e:
        handle_proxy_error(e)


# ============ Threadless Runs ============


@router.post("/runs/stream")
async def stream_stateless_run(
    data: RunCreate,
    request: Request,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
    rate_limiter: MultiDimensionRateLimiter = Depends(get_rate_limiter),
):
    """无 Thread 的流式 Run（一次性对话）"""
    rate_limit_headers = await check_langgraph_rate_limit(
        user,
        rate_limiter,
        assistant_id=data.assistant_id,
        operation="run_create",
    )

    async def event_generator():
        try:
            async for chunk in proxy.stream_run(
                user=user,
                thread_id=None,
                assistant_id=data.assistant_id,
                input_data=data.input,
                config=data.config,
                metadata=data.metadata,
                stream_mode=data.stream_mode,
                http_request=request,
            ):
                yield {
                    "event": chunk.get("event", "message"),
                    "data": json.dumps(chunk.get("data", {})),
                }
        except HTTPException:
            raise
        except Exception as e:
            yield {
                "event": "error",
                "data": json.dumps({"error": str(e)}),
            }

    return EventSourceResponse(
        event_generator(),
        headers=rate_limit_headers,
        ping=int(DEFAULT_HEARTBEAT_INTERVAL_S),
    )


@router.post("/runs/wait")
async def create_stateless_run(
    data: RunCreate,
    request: Request,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
    rate_limiter: MultiDimensionRateLimiter = Depends(get_rate_limiter),
):
    """无状态 Run（一次性对话）"""
    await check_langgraph_rate_limit(
        user,
        rate_limiter,
        assistant_id=data.assistant_id,
        operation="run_create",
    )

    try:
        return await proxy.create_stateless_run(
            user=user,
            assistant_id=data.assistant_id,
            input_data=data.input,
            config=data.config,
            metadata=data.metadata,
            http_request=request,
        )
    except Exception as e:
        handle_proxy_error(e)


# ============ Store API ============


@router.post("/store/items")
async def store_put_item(
    data: StoreItem,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
    rate_limiter: MultiDimensionRateLimiter = Depends(get_rate_limiter),
):
    """写入记忆"""
    await check_langgraph_rate_limit(user, rate_limiter, operation="store_write")

    try:
        await proxy.store_put(user, data.namespace, data.key, data.value)
        return {"status": "ok"}
    except Exception as e:
        handle_proxy_error(e)


@router.get("/store/items")
async def store_get_item(
    namespace: str = Query(..., description="JSON encoded namespace list"),
    key: str = Query(...),
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """获取记忆"""
    try:
        ns = json.loads(namespace)
        result = await proxy.store_get(user, ns, key)
        if result is None:
            raise HTTPException(status_code=404, detail="Item not found")
        return result
    except json.JSONDecodeError:
        raise HTTPException(status_code=400, detail="Invalid namespace format")
    except Exception as e:
        handle_proxy_error(e)


@router.delete("/store/items")
async def store_delete_item(
    namespace: str = Query(..., description="JSON encoded namespace list"),
    key: str = Query(...),
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """删除记忆"""
    try:
        ns = json.loads(namespace)
        await proxy.store_delete(user, ns, key)
        return {"status": "ok"}
    except json.JSONDecodeError:
        raise HTTPException(status_code=400, detail="Invalid namespace format")
    except Exception as e:
        handle_proxy_error(e)


@router.get("/store/namespaces")
async def store_list_namespaces(
    prefix: str | None = Query(None, description="JSON encoded prefix list"),
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """列出命名空间"""
    try:
        prefix_list = json.loads(prefix) if prefix else None
        return await proxy.store_list_namespaces(user, prefix=prefix_list)
    except json.JSONDecodeError:
        raise HTTPException(status_code=400, detail="Invalid prefix format")
    except Exception as e:
        handle_proxy_error(e)


# ============ Passthrough (Full LangGraph API) ============


@router.api_route(
    "/{full_path:path}",
    methods=["GET", "POST", "PUT", "PATCH", "DELETE"],
)
async def passthrough(
    full_path: str,
    request: Request,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
    rate_limiter: MultiDimensionRateLimiter = Depends(get_rate_limiter),
):
    """
    LangGraph CLI / LangSmith Deployment API is evolving quickly.
    This catch-all route forwards any unimplemented endpoint to the selected
    upstream instance so the gateway stays compatible with upstream `/docs`.
    """
    upstream_path = "/" + full_path.lstrip("/")
    method = request.method.upper()
    body = (
        await request.body()
        if method in {"POST", "PUT", "PATCH"}
        else None
    )

    if is_langgraph_run_path(method, full_path):
        parsed = decode_json_body(body)
        assistant_id = (
            str(parsed.get("assistant_id") or "").strip() or None
            if isinstance(parsed, dict)
            else None
        )
        await check_langgraph_rate_limit(
            user,
            rate_limiter,
            assistant_id=assistant_id,
            operation="run_create",
        )

    # Ownership checks for thread/assistant scoped paths.
    # Authorization failures must abort; only non-authorization errors
    # (network, not-applicable checks) are tolerated.
    parts = [p for p in upstream_path.split("/") if p]
    try:
        if len(parts) >= 2 and parts[0] == "threads" and parts[1] not in ("search", "count"):
            await proxy.get_thread(user, parts[1])
        if len(parts) >= 2 and parts[0] == "assistants" and parts[1] not in ("search", "count"):
            await proxy.get_assistant(user, parts[1])
    except (ForbiddenError, AssistantAccessDeniedError) as exc:
        # Ownership denied — reject immediately, never forward.
        raise HTTPException(status_code=403, detail=str(exc))
    except (ThreadNotFoundError, AssistantNotFoundError):
        # Resource absent — return 404 rather than forwarding.
        raise HTTPException(status_code=404, detail="resource not found")
    except Exception as exc:
        # Ownership checks are part of the gateway trust boundary. If the
        # check cannot be completed, fail closed instead of forwarding blindly.
        raise HTTPException(status_code=502, detail="ownership check failed") from exc

    params = list(request.query_params.multi_items())
    headers = _build_langgraph_passthrough_headers(request, user, proxy)

    body = await prepare_langgraph_run_body_for_passthrough(
        body,
        method=method,
        path=full_path,
        request=request,
        user=user,
        auth=auth,
    )
    wants_stream = (
        "text/event-stream" in (headers.get("accept", "") or "")
        or upstream_path.endswith("/stream")
        or "/stream" in upstream_path
    )

    instance = await proxy.lb.select_instance()
    client = await proxy._get_client(instance)
    started_at = time.time()
    database = getattr(request.app.state, "database", None)
    request_id = str(getattr(request.state, "request_id", "") or "")
    traceparent = getattr(request.state, "traceparent", None) or request.headers.get(
        "traceparent"
    )

    def _capture_proxy_trace(
        *,
        status: str,
        upstream_status: int | None,
        error_summary: str | None = None,
    ) -> None:
        if database is None:
            return
        record_langgraph_proxy_trace(
            database,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            request_id=request_id,
            method=method,
            upstream_path=upstream_path,
            started_at=started_at,
            status=status,
            upstream_status=upstream_status,
            error_summary=error_summary,
            traceparent=traceparent,
            streaming=wants_stream,
        )

    if wants_stream:

        async def gen():
            upstream_status: int | None = None
            status = "succeeded"
            error_summary: str | None = None
            try:
                async with client.stream(
                    method,
                    upstream_path,
                    params=params,
                    headers=headers,
                    content=body if body else None,
                ) as resp:
                    upstream_status = resp.status_code
                    if resp.status_code >= 400:
                        status = "failed"
                        err = await resp.aread()
                        error_summary = err.decode("utf-8", errors="ignore")[:500]
                        raise HTTPException(
                            status_code=resp.status_code,
                            detail=error_summary,
                        )
                    async for chunk in resp.aiter_raw():
                        yield chunk
            except HTTPException:
                if upstream_status is None:
                    status = "failed"
                raise
            except Exception as exc:
                status = "failed"
                error_summary = str(exc)
                raise
            finally:
                _capture_proxy_trace(
                    status=status,
                    upstream_status=upstream_status,
                    error_summary=error_summary,
                )

        # Wrap the upstream byte stream with periodic ``: heartbeat``
        # SSE comments so an idle 30-60s stretch (long KB query, slow
        # generation) doesn't trip nginx / ALB / NAT idle timeouts and
        # sever the client-facing connection mid-flight. EventSource
        # silently drops ``:``-prefixed lines per the SSE spec, so the
        # frontend sees zero behavior change.
        return StreamingResponse(
            with_sse_heartbeat(gen()),
            media_type="text/event-stream",
        )

    resp = await client.request(
        method,
        upstream_path,
        params=params,
        headers=headers,
        content=body if body else None,
    )
    status = "succeeded" if resp.status_code < 400 else "failed"
    error_summary = None if status == "succeeded" else resp.text[:500]
    _capture_proxy_trace(
        status=status,
        upstream_status=resp.status_code,
        error_summary=error_summary,
    )
    content_type = resp.headers.get("content-type", "")
    if "application/json" in content_type:
        return resp.json()
    return Response(
        content=resp.content,
        status_code=resp.status_code,
        media_type=content_type or None,
    )
