"""
简化的对话 API

对外暴露业务友好的接口，隐藏 LangGraph 实现细节：

外部 API（应用端调用）           内部转换为 LangGraph API
─────────────────────────────────────────────────────────
POST   /conversations           →  POST /threads
GET    /conversations           →  POST /threads/search (带 filter)
GET    /conversations/{id}      →  GET  /threads/{id}
DELETE /conversations/{id}      →  DELETE /threads/{id}

POST   /conversations/{id}/messages  →  POST /threads/{id}/runs/wait
GET    /conversations/{id}/messages  →  GET  /threads/{id}/history
POST   /conversations/{id}/stream    →  SSE /threads/{id}/runs/stream
"""

from __future__ import annotations

import json
from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel, Field
from sse_starlette.sse import EventSourceResponse

from ...adapters.langgraph_proxy import LangGraphProxy
from ...core.auth.user_resolver import UserContext
from ...core.gateway.multi_dimension_rate_limiter import MultiDimensionRateLimiter
from ..deps import AuthContext, get_auth_context, get_rate_limiter, get_user_context, require_langgraph_proxy
from ._langgraph_route_utils import check_langgraph_rate_limit, handle_langgraph_proxy_error

router = APIRouter(prefix="/conversations", tags=["Conversations"])


async def _record_security_event(
    event_type: str,
    tenant_id: str,
    user_id: str,
    service_id: str | None,
) -> None:
    try:
        from ...services.metrics import get_security_event_recorder

        recorder = get_security_event_recorder()
        await recorder.record_event(
            tenant_id=tenant_id or "public",
            user_id=user_id or None,
            service_id=service_id,
            event_type=event_type,
        )
    except Exception:
        pass


# ============ Pydantic 模型 ============


class ConversationCreate(BaseModel):
    """创建对话请求"""

    title: str | None = None
    metadata: dict[str, Any] | None = None
    assistant_id: str = Field(default="agent", description="使用的 Assistant ID")


class ConversationResponse(BaseModel):
    """对话响应"""

    conversation_id: str
    title: str | None = None
    created_at: str
    updated_at: str | None = None
    status: str = "active"
    metadata: dict[str, Any] | None = None


class MessageCreate(BaseModel):
    """发送消息请求"""

    content: str
    role: str = "user"
    metadata: dict[str, Any] | None = None


class MessageResponse(BaseModel):
    """消息响应"""

    message_id: str
    role: str
    content: str
    created_at: str
    metadata: dict[str, Any] | None = None


class StreamMessageCreate(BaseModel):
    """流式消息请求"""

    content: str
    role: str = "user"
    stream_mode: list[str] | None = Field(
        default=["messages", "updates"], description="流式输出模式"
    )


def handle_proxy_error(e: Exception) -> None:
    handle_langgraph_proxy_error(e, not_found_detail="Conversation not found")


async def _on_rate_limit_denied(
    user: UserContext,
    service_id: str | None,
) -> None:
    await _record_security_event(
        event_type="rate_limited",
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        service_id=service_id or "assistant",
    )


# ============ 辅助函数 ============


def _thread_to_conversation(thread: dict[str, Any]) -> dict[str, Any]:
    """将 Thread 转换为 Conversation 格式"""
    metadata = thread.get("metadata", {})
    return {
        "conversation_id": thread.get("thread_id"),
        "title": metadata.get("title"),
        "created_at": metadata.get("created_at", thread.get("created_at")),
        "updated_at": thread.get("updated_at"),
        "status": thread.get("status", "active"),
        "metadata": {
            k: v
            for k, v in metadata.items()
            if k not in ("owner_id", "tenant_id", "user_tier", "created_at", "title")
        },
    }


def _history_to_messages(history: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """将 Thread History 转换为消息格式"""
    messages = []
    for item in history:
        values = item.get("values", {})
        if "messages" in values:
            for msg in values["messages"]:
                messages.append(
                    {
                        "message_id": msg.get("id", ""),
                        "role": msg.get("type", "unknown"),
                        "content": msg.get("content", ""),
                        "created_at": item.get("created_at", ""),
                        "metadata": msg.get("metadata", {}),
                    }
                )
    return messages


# ============ API 端点 ============


@router.post("", response_model=ConversationResponse)
async def create_conversation(
    data: ConversationCreate,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
    rate_limiter: MultiDimensionRateLimiter = Depends(get_rate_limiter),
):
    """
    创建新对话

    创建一个新的对话会话，返回对话 ID 用于后续消息交互。
    """
    await check_langgraph_rate_limit(
        user,
        rate_limiter,
        operation="thread_create",
        service_id=data.assistant_id,
        on_denied=lambda: _on_rate_limit_denied(user, data.assistant_id),
    )

    try:
        # 构建 Thread 元数据
        metadata = data.metadata or {}
        if data.title:
            metadata["title"] = data.title
        metadata["assistant_id"] = data.assistant_id

        thread = await proxy.create_thread(user=user, metadata=metadata)

        return _thread_to_conversation(thread)
    except Exception as e:
        handle_proxy_error(e)


@router.get("")
async def list_conversations(
    limit: int = Query(10, ge=1, le=100),
    offset: int = Query(0, ge=0),
    status: str | None = Query(None, description="Filter by status"),
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """
    获取对话列表

    获取当前用户的所有对话，支持分页。
    """
    try:
        metadata_filter = {}
        if status:
            metadata_filter["status"] = status

        threads = await proxy.search_threads(
            user=user,
            metadata=metadata_filter if metadata_filter else None,
            limit=limit,
            offset=offset,
        )

        return {
            "conversations": [_thread_to_conversation(t) for t in threads],
            "total": len(threads),
            "limit": limit,
            "offset": offset,
        }
    except Exception as e:
        handle_proxy_error(e)


@router.get("/{conversation_id}", response_model=ConversationResponse)
async def get_conversation(
    conversation_id: str,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """
    获取对话详情

    获取指定对话的详细信息。
    """
    try:
        thread = await proxy.get_thread(user, conversation_id)
        return _thread_to_conversation(thread)
    except Exception as e:
        handle_proxy_error(e)


@router.delete("/{conversation_id}")
async def delete_conversation(
    conversation_id: str,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """
    删除对话

    删除指定的对话及其所有消息历史。
    """
    try:
        await proxy.delete_thread(user, conversation_id)
        return {"status": "ok", "message": "Conversation deleted"}
    except Exception as e:
        handle_proxy_error(e)


@router.post("/{conversation_id}/messages")
async def send_message(
    conversation_id: str,
    data: MessageCreate,
    request: Request,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
    _auth: AuthContext = Depends(get_auth_context),
    rate_limiter: MultiDimensionRateLimiter = Depends(get_rate_limiter),
):
    """
    发送消息（同步）

    向对话发送消息并等待 AI 回复。适用于简单交互场景。
    """
    await check_langgraph_rate_limit(
        user,
        rate_limiter,
        operation="run_create",
        service_id="assistant",
        on_denied=lambda: _on_rate_limit_denied(user, "assistant"),
    )

    try:
        # 获取对话关联的 assistant_id
        thread = await proxy.get_thread(user, conversation_id)
        metadata = thread.get("metadata", {})
        assistant_id = metadata.get("assistant_id", "agent")

        # 构建消息输入
        input_data = {"messages": [{"role": data.role, "content": data.content}]}

        # 执行 Run 并等待完成
        result = await proxy.create_run_wait(
            user=user,
            thread_id=conversation_id,
            assistant_id=assistant_id,
            input_data=input_data,
            metadata=data.metadata,
            http_request=request,
        )

        # 提取回复消息
        output_messages = []
        if "messages" in result:
            for msg in result["messages"]:
                output_messages.append(
                    {
                        "message_id": msg.get("id", ""),
                        "role": msg.get("type", "assistant"),
                        "content": msg.get("content", ""),
                    }
                )

        return {
            "conversation_id": conversation_id,
            "messages": output_messages,
            "status": result.get("status", "completed"),
        }
    except Exception as e:
        handle_proxy_error(e)


@router.get("/{conversation_id}/messages")
async def get_messages(
    conversation_id: str,
    limit: int = Query(50, ge=1, le=200),
    before: str | None = Query(None, description="Cursor for pagination"),
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
):
    """
    获取消息历史

    获取对话的所有消息历史，支持分页。
    """
    try:
        history = await proxy.get_thread_history(
            user=user,
            thread_id=conversation_id,
            limit=limit,
            before=before,
        )

        messages = _history_to_messages(history)

        return {
            "conversation_id": conversation_id,
            "messages": messages,
            "total": len(messages),
        }
    except Exception as e:
        handle_proxy_error(e)


@router.post("/{conversation_id}/stream")
async def stream_message(
    conversation_id: str,
    data: StreamMessageCreate,
    request: Request,
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
    user: UserContext = Depends(get_user_context),
    rate_limiter: MultiDimensionRateLimiter = Depends(get_rate_limiter),
):
    """
    发送消息（流式）

    向对话发送消息并获取流式回复。适用于需要实时显示 AI 回复的场景。
    """
    await check_langgraph_rate_limit(
        user,
        rate_limiter,
        operation="run_create",
        service_id="assistant",
        on_denied=lambda: _on_rate_limit_denied(user, "assistant"),
    )

    # 获取对话关联的 assistant_id
    try:
        thread = await proxy.get_thread(user, conversation_id)
        metadata = thread.get("metadata", {})
        assistant_id = metadata.get("assistant_id", "agent")
    except Exception as e:
        handle_proxy_error(e)

    # 构建消息输入
    input_data = {"messages": [{"role": data.role, "content": data.content}]}

    async def event_generator():
        try:
            async for chunk in proxy.stream_run(
                user=user,
                thread_id=conversation_id,
                assistant_id=assistant_id,
                input_data=input_data,
                stream_mode=data.stream_mode,
                skip_thread_validation=True,  # 已在上面验证过
                http_request=request,
            ):
                event_type = chunk.get("event", "message")
                event_data = chunk.get("data", {})

                # 简化事件格式
                if event_type in ("messages", "updates"):
                    yield {
                        "event": "message",
                        "data": json.dumps(event_data),
                    }
                elif event_type == "error":
                    yield {
                        "event": "error",
                        "data": json.dumps(event_data),
                    }
                else:
                    yield {
                        "event": event_type,
                        "data": json.dumps(event_data),
                    }

            # 发送完成事件
            yield {
                "event": "done",
                "data": json.dumps({"status": "completed"}),
            }

        except HTTPException:
            raise
        except Exception as e:
            yield {
                "event": "error",
                "data": json.dumps({"error": str(e)}),
            }

    return EventSourceResponse(event_generator())


# ============ 游客会话 API ============


@router.post("/guest/session")
async def create_guest_session(
    proxy: LangGraphProxy = Depends(require_langgraph_proxy),
):
    """
    创建游客会话

    为未登录用户创建临时会话，返回 session_id 用于后续请求。
    """
    from ...services.session.guest_session_manager import (
        GuestSessionConfig,
        GuestSessionManager,
    )

    # 获取或创建游客会话管理器
    manager = GuestSessionManager(
        config=GuestSessionConfig(),
        redis_client=proxy.redis,
    )

    session = await manager.create_session()

    return {
        "session_id": session.session_id,
        "expires_in": manager.config.session_ttl,
        "message": "Use this session_id in X-Guest-Session header for subsequent requests",
    }
