from __future__ import annotations

import codecs
import importlib
import json
import re
from collections.abc import AsyncIterator, Callable

# 定义流事件的结构
from dataclasses import dataclass
from types import SimpleNamespace
from typing import Any

from ai_gateway_core.enums import ContentType, StreamEventType, TransportType
from ai_gateway_core.exceptions import ValidationFailedError

from ..models.request import ContentItem, UnifiedRequest
from ..models.response import StreamChunk, ToolCall, UnifiedResponse
from ..proxy.langgraph_run_body import (
    build_control_plane_request,
    prepare_and_finalize_langgraph_run_payload,
    proxy_service_config_from_connector,
)
from ..services.metrics.redaction import redact_sensitive_text
from .base import ProtocolAdapter, _owned_stream


@dataclass
class StreamEvent:
    """流事件封装"""

    event_type: StreamEventType
    text: str | None = None
    tool_call: ToolCall | None = None


_SECRET_JSON_VALUE_RE = re.compile(
    r'("?_?api_key"?\s*[:=]\s*)("[^"]*"|[^,\s}]+)',
    re.IGNORECASE,
)


def _scrub_sensitive_text(value: str) -> str:
    """Scrub provider credentials from upstream errors before logging/returning."""
    return redact_sensitive_text(value)


def _extract_langgraph_error(result: Any) -> str | None:
    """Return a scrubbed LangGraph error payload when a 200 response embeds one."""
    if not isinstance(result, dict) or "__error__" not in result:
        return None

    error = result.get("__error__")
    if isinstance(error, dict):
        error_type = error.get("error") or error.get("type") or "LangGraphError"
        message = error.get("message") or error.get("detail") or ""
        return _scrub_sensitive_text(f"{error_type}: {message}")
    return _scrub_sensitive_text(str(error))


def _tool_result_status(message: Any) -> str:
    status = message.get("status") if isinstance(message, dict) else getattr(message, "status", None)
    return "error" if str(status or "").lower() in ("error", "failed") else "completed"


class LangGraphAdapter(ProtocolAdapter):
    # 类级别的 session_id -> thread_id (UUID) 映射缓存
    _session_to_thread_map: dict[str, str] = {}
    _provider_service: Any | None = None
    _model_service: Any | None = None

    @classmethod
    def configure_model_control_plane(
        cls,
        provider_service: Any | None,
        model_service: Any | None,
    ) -> None:
        cls._provider_service = provider_service
        cls._model_service = model_service

    def __init__(self, service):
        super().__init__(service)
        config = service.connector_config or {}
        self.config = config
        self.remote = service.connector_type != TransportType.IN_PROCESS
        self.assistant_id = (
            config.get("assistant_id")
            or config.get("graph_id")
            or config.get("graph_name")
            or service.service_id
        )
        if not self.remote:
            module_path = config.get("graph_module")
            graph_name = config.get("graph_name")
            if not module_path or not graph_name:
                raise ValidationFailedError("graph_module and graph_name are required")
            module = importlib.import_module(module_path)
            graph_obj = getattr(module, graph_name)
            if callable(graph_obj) and not hasattr(graph_obj, "ainvoke"):
                graph_obj = graph_obj()
            self.graph = graph_obj
        else:
            self.graph = None
            self.invoke_endpoint = config.get("invoke_endpoint", "/runs/wait")
            self.stream_endpoint = config.get("stream_endpoint", "/runs/stream")
            self.thread_invoke_endpoint = config.get(
                "thread_invoke_endpoint", "/threads/{thread_id}/runs/wait"
            )
            self.thread_stream_endpoint = config.get(
                "thread_stream_endpoint", "/threads/{thread_id}/runs/stream"
            )
            # 认证配置：如果 Agent 启用了 auth，网关需要传递用户信息
            # 支持配置静态用户 ID 或动态传递请求中的用户信息
            self.auth_user_id = config.get("auth_user_id")  # 静态用户 ID（可选）
            self.auth_user_type = config.get("auth_user_type", "user")  # 用户类型
            self.forward_auth = config.get("forward_auth", True)  # 是否转发认证信息

    def _build_auth_headers(self, request: UnifiedRequest) -> dict[str, str]:
        """
        构建认证头部，用于转发给 LangGraph Agent。

        LangGraph Agent 的 auth 模块支持两种认证方式：
        1. X-User-Id 头部（网关转发）
        2. Authorization Bearer token（直接访问）

        为确保兼容性，同时发送两种认证方式。
        """
        if not self.forward_auth:
            return {}

        # 使用配置的静态用户 ID 或请求中的用户 ID
        user_id = self.auth_user_id or request.user_id or "gateway-user"
        user_type = self.auth_user_type
        user_name = f"User-{request.user_id}" if request.user_id else "Gateway User"

        headers: dict[str, str] = {
            # 方式1: X-User-Id 头部（LangGraph SDK 会自动映射到 x_user_id 参数）
            "x-user-id": str(user_id),
            "x-user-type": user_type,
            "x-user-name": user_name,
            "x-user-permissions": "read,write",
        }

        # 方式2: 同时发送 Bearer token 作为备用认证
        # 使用 user_id 作为简单 token，LangGraph auth 会在 DEV_MODE 下接受任何 token
        headers["Authorization"] = f"Bearer gateway-{user_id}"

        # 如果有租户 ID，也传递
        if request.tenant_id:
            headers["x-tenant-id"] = str(request.tenant_id)

        return headers

    def _build_base_run_config(
        self, request: UnifiedRequest, thread_id: str | None = None
    ) -> dict[str, Any]:
        """Build LangGraph run config with gateway-injected `configurable` values."""
        params = request.parameters or {}
        run_config: dict[str, Any] = {}
        if isinstance(params.get("config"), dict):
            run_config = dict(params["config"])

        configurable = run_config.get("configurable") or {}
        if not isinstance(configurable, dict):
            configurable = {}
        configurable = dict(configurable)
        # Browser/caller supplied model config is never authoritative.
        configurable.pop("gateway_model", None)

        # Merge in caller-provided configurable from request.context / request.parameters.
        for source in (request.context, request.parameters):
            if not isinstance(source, dict):
                continue
            src_cfg = source.get("configurable")
            if isinstance(src_cfg, dict):
                for k, v in src_cfg.items():
                    if k == "gateway_model":
                        continue
                    if k not in configurable and v is not None:
                        configurable[k] = v
            for k in ("dataset_id", "gateway_token"):
                v = source.get(k)
                if k not in configurable and v is not None:
                    configurable[k] = v

        # Enforce gateway scoping fields.
        configurable["user_id"] = request.user_id
        configurable["tenant_id"] = request.tenant_id
        configurable["checkpoint_ns"] = request.tenant_id or ""
        configurable["thread_id"] = thread_id or (request.session_id or request.request_id)

        run_config["configurable"] = configurable
        return run_config

    @staticmethod
    def _adapter_user_namespace(request: UnifiedRequest) -> SimpleNamespace:
        return SimpleNamespace(
            user_id=request.user_id,
            tenant_id=request.tenant_id,
            roles=["user"] if request.user_id else ["guest"],
            is_authenticated=bool(request.user_id),
            tier="anonymous",
        )

    def _proxy_service_config(self) -> Any:
        return proxy_service_config_from_connector(
            self.config,
            service_id=self.service.service_id,
            service_name=self.service.name,
            assistant_id=self.assistant_id,
            metadata=self.service.metadata,
        )

    async def _prepare_remote_run_payload(
        self,
        request: UnifiedRequest,
        *,
        payload: dict[str, Any],
        path: str,
    ) -> dict[str, Any]:
        provider_service = self._provider_service
        model_service = self._model_service
        if provider_service is None or model_service is None:
            model_override = self.config.get("model_override")
            if isinstance(model_override, dict) and model_override.get("enabled"):
                raise ValidationFailedError("LangGraph model control plane is not initialized")
            payload["config"] = self._build_base_run_config(
                request,
                thread_id=path.split("/")[1] if path.startswith("threads/") else None,
            )
            return payload

        cp_request = build_control_plane_request(
            provider_service=provider_service,
            model_service=model_service,
            request_id=request.request_id,
            trace_id=request.request_id,
        )
        try:
            return await prepare_and_finalize_langgraph_run_payload(
                payload,
                method="POST",
                path=path,
                request=cp_request,
                user=self._adapter_user_namespace(request),
                service_config=self._proxy_service_config(),
                assistant_id=self.assistant_id,
                tenant_id=request.tenant_id,
            )
        except Exception as exc:
            from fastapi import HTTPException

            if isinstance(exc, HTTPException) and exc.status_code == 422:
                raise ValidationFailedError(
                    f"LangGraph model_override invalid: {exc.detail}"
                ) from exc
            if isinstance(exc, HTTPException) and exc.status_code == 503:
                raise ValidationFailedError(
                    "LangGraph model control plane is not initialized"
                ) from exc
            raise

    async def _build_run_config(
        self, request: UnifiedRequest, thread_id: str | None = None
    ) -> dict[str, Any]:
        path = f"threads/{thread_id}/runs/wait" if thread_id else "runs/wait"
        payload = {
            "assistant_id": self.assistant_id,
            "config": self._build_base_run_config(request, thread_id=thread_id),
        }
        prepared = await self._prepare_remote_run_payload(
            request,
            payload=payload,
            path=path,
        )
        run_config = prepared.get("config")
        return dict(run_config) if isinstance(run_config, dict) else {}

    async def invoke(self, request: UnifiedRequest) -> UnifiedResponse:
        messages = self._build_messages(request.inputs)
        metadata = None
        if not self.remote:
            from .langgraph_local_usage import prepare_local_invoke_usage

            run_config, usage_observer = prepare_local_invoke_usage(
                await self._build_run_config(request)
            )
            result = await self.graph.ainvoke({"messages": messages}, run_config)
            from .langgraph_local import extract_local_result_text

            if usage_observer is not None:
                coverage = usage_observer.result()
                metadata = {"local_model_usage": coverage}
            content = extract_local_result_text(result)
            if content is None:
                content = self._extract_result_text(result)
        else:
            result = await self._remote_wait(request, messages)
            content = self._extract_result_text(result)
        return UnifiedResponse(
            request_id=request.request_id,
            status="success",
            outputs=[ContentItem(type=ContentType.TEXT, data=content)],
            session_id=request.session_id,
            metadata=metadata,
        )

    async def stream(self, request: UnifiedRequest) -> AsyncIterator[StreamChunk]:
        messages = self._build_messages(request.inputs)
        idx = 0
        usage_metadata: dict[str, Any] | None = None
        if not self.remote:
            if not hasattr(self.graph, "astream_events"):
                raise ValidationFailedError("graph does not support streaming")
            from .langgraph_local import prepare_local_tool_stream

            run_config, tool_stream = prepare_local_tool_stream(
                await self._build_run_config(request)
            )
            # 追踪已处理的内容，用于去重
            seen_content_ids: set = set()

            async with _owned_stream(self.graph.astream_events(
                {"messages": messages}, run_config, version="v2"
            )) as producer:
                async for event in producer:
                    if isinstance(event, dict) and event.get("event") == "metadata":
                        data = event.get("data") or {}
                        if isinstance(data, dict) and isinstance(data.get("usage"), dict):
                            usage_metadata = data.get("usage")
                        continue
                    projections = tool_stream.project(event) if tool_stream else [event]
                    for projected in projections:
                        stream_event = self._extract_stream_event(projected, seen_content_ids)
                        if stream_event:
                            if tool_stream and stream_event.tool_call:
                                call_id = tool_stream.tool_identity(projected)
                                if call_id:
                                    stream_event.tool_call.tool_call_id = call_id
                            yield StreamChunk(
                                request_id=request.request_id,
                                chunk_index=idx,
                                content=ContentItem(
                                    type=ContentType.TEXT if stream_event.text else ContentType.TOOL_CALL,
                                    data=stream_event.text or "",
                                ),
                                event_type=stream_event.event_type,
                                tool_call=stream_event.tool_call,
                            )
                            idx += 1
            if tool_stream:
                tool_stream.finish()
        else:

            def _capture_usage(usage: dict[str, Any]) -> None:
                nonlocal usage_metadata
                usage_metadata = usage

            async with _owned_stream(self._remote_stream(
                request, messages, usage_handler=_capture_usage
            )) as producer:
                async for stream_event in producer:
                    if stream_event:
                        yield StreamChunk(
                            request_id=request.request_id,
                            chunk_index=idx,
                            content=ContentItem(
                                type=ContentType.TEXT if stream_event.text else ContentType.TOOL_CALL,
                                data=stream_event.text or "",
                            ),
                            event_type=stream_event.event_type,
                            tool_call=stream_event.tool_call,
                        )
                        idx += 1

        yield StreamChunk(
            request_id=request.request_id,
            chunk_index=idx,
            content=ContentItem(type=ContentType.TEXT, data=""),
            is_final=True,
            event_type=StreamEventType.FINAL,
            metadata={"usage": usage_metadata} if usage_metadata else None,
        )

    async def health_check(self) -> bool:
        if not self.remote:
            return self.graph is not None
        # 为远程健康检查构建认证头部
        # 使用虚拟请求构建头部（因为健康检查没有实际请求上下文）
        from ..models.request import UnifiedRequest

        dummy_request = UnifiedRequest(
            request_id="health-check",
            service_id=self.service.service_id,
            inputs=[],
            user_id="gateway-health-check",
            tenant_id="gateway",
        )
        auth_headers = self._build_auth_headers(dummy_request)
        return await self.connector.health_check(headers=auth_headers)

    def _build_messages(self, inputs: list[ContentItem]) -> list[dict[str, Any]]:
        texts = [str(i.data) for i in inputs if i.type == ContentType.TEXT and i.data]
        if not texts:
            raise ValidationFailedError("text input required")
        return [{"role": "user", "content": "\n".join(texts)}]

    def _extract_result_text(self, result: Any) -> str:
        if isinstance(result, dict):
            messages = result.get("messages")
            if not messages and isinstance(result.get("values"), dict):
                messages = result["values"].get("messages")
            if not messages and isinstance(result.get("output"), dict):
                messages = result["output"].get("messages")
            if isinstance(messages, list) and messages:
                # 从后向前遍历，找到最后一条 AI 消息
                for msg in reversed(messages):
                    if isinstance(msg, dict):
                        role = msg.get("role", "") or msg.get("type", "")
                        # 只返回 AI 消息内容
                        if role in ("assistant", "ai", "AIMessage"):
                            return str(msg.get("content") or "")
                    elif hasattr(msg, "type"):
                        msg_type = getattr(msg, "type", "")
                        if msg_type in ("ai", "AIMessage"):
                            return str(getattr(msg, "content", msg))
                # 如果没找到明确的 AI 消息，尝试返回最后一条非用户消息
                last = messages[-1]
                if isinstance(last, dict):
                    role = last.get("role", "") or last.get("type", "")
                    if role not in ("user", "human", "HumanMessage"):
                        return str(last.get("content") or "")
                elif hasattr(last, "type"):
                    msg_type = getattr(last, "type", "")
                    if msg_type not in ("human", "HumanMessage"):
                        return str(getattr(last, "content", last))
        return str(result)

    def _extract_stream_event(self, event: Any, seen_ids: set) -> StreamEvent | None:
        """从本地 LangGraph 的 astream_events 中提取流事件"""
        if not isinstance(event, dict):
            return None

        event_type = event.get("event", "")
        run_id = event.get("run_id", "")
        data = event.get("data") or {}

        # 处理 AI 模型的流式文本输出
        if event_type == "on_chat_model_stream":
            chunk = data.get("chunk")
            if chunk is not None:
                # astream_events emits deltas. Their id belongs to the parent
                # message and is shared across chunks, including empty/repeated
                # chunks; it cannot identify duplicate delivery.
                # 检查是否有工具调用
                tool_calls = None
                if hasattr(chunk, "tool_call_chunks"):
                    tool_calls = chunk.tool_call_chunks
                elif hasattr(chunk, "tool_calls"):
                    tool_calls = chunk.tool_calls
                elif isinstance(chunk, dict):
                    tool_calls = chunk.get("tool_call_chunks") or chunk.get("tool_calls")

                # 处理工具调用增量
                if tool_calls and len(tool_calls) > 0:
                    tc = tool_calls[0]
                    if isinstance(tc, dict):
                        tc_id = tc.get("id") or tc.get("tool_call_id") or ""
                        tc_name = tc.get("name") or ""
                        tc_args = tc.get("args") or tc.get("arguments") or ""
                    else:
                        tc_id = getattr(tc, "id", "") or getattr(tc, "tool_call_id", "") or ""
                        tc_name = getattr(tc, "name", "") or ""
                        tc_args = getattr(tc, "args", "") or getattr(tc, "arguments", "") or ""

                    if isinstance(tc_args, dict):
                        tc_args = json.dumps(tc_args)

                    # 只返回有实际内容的工具调用
                    if tc_name or tc_args:
                        return StreamEvent(
                            event_type=StreamEventType.TOOL_CALL_DELTA,
                            tool_call=ToolCall(
                                tool_call_id=tc_id,
                                name=tc_name,
                                arguments=tc_args,
                                status="running",
                            ),
                        )

                # 提取文本内容
                if hasattr(chunk, "content"):
                    content = chunk.content
                    if isinstance(content, str) and content:
                        return StreamEvent(
                            event_type=StreamEventType.TEXT_DELTA,
                            text=content,
                        )
                elif isinstance(chunk, dict):
                    content = chunk.get("content")
                    if isinstance(content, str) and content:
                        return StreamEvent(
                            event_type=StreamEventType.TEXT_DELTA,
                            text=content,
                        )
            return None

        # 处理工具调用开始事件
        if event_type == "on_tool_start":
            # 使用 run_id 去重
            event_key = f"tool_start:{run_id}"
            if event_key in seen_ids:
                return None
            seen_ids.add(event_key)

            tool_name = event.get("name", "")
            tool_input = data.get("input", {})
            if isinstance(tool_input, dict):
                tool_input_str = json.dumps(tool_input, ensure_ascii=False)
            else:
                tool_input_str = str(tool_input)

            return StreamEvent(
                event_type=StreamEventType.TOOL_CALL_START,
                tool_call=ToolCall(
                    tool_call_id=run_id,
                    name=tool_name,
                    arguments=tool_input_str,
                    status="running",
                ),
            )

        if event_type == "on_tool_error":
            event_key = f"tool_end:{run_id}"
            if event_key in seen_ids:
                return None
            seen_ids.add(event_key)
            # Preserve failure status without exposing exception/provider details.
            return StreamEvent(
                event_type=StreamEventType.TOOL_RESULT,
                text="",
                tool_call=ToolCall(
                    tool_call_id=run_id, name=event.get("name", ""), status="error"
                ),
            )

        # 处理工具调用结束事件
        if event_type == "on_tool_end":
            # 使用 run_id 去重
            event_key = f"tool_end:{run_id}"
            if event_key in seen_ids:
                return None
            seen_ids.add(event_key)

            tool_name = event.get("name", "")
            output = data.get("output", "")
            if isinstance(output, dict):
                output_str = json.dumps(output, ensure_ascii=False)
            elif hasattr(output, "content"):
                output_str = str(getattr(output, "content", output))
            else:
                output_str = str(output)

            return StreamEvent(
                event_type=StreamEventType.TOOL_RESULT,
                text=output_str,
                tool_call=ToolCall(
                    tool_call_id=run_id,
                    name=tool_name,
                    arguments="",
                    status=_tool_result_status(output),
                ),
            )

        return None

    async def _remote_wait(self, request: UnifiedRequest, messages: list[dict[str, Any]]) -> Any:
        input_payload: dict[str, Any] = {"messages": messages}
        params = request.parameters or {}
        extra_input = params.get("input")
        if isinstance(extra_input, dict):
            input_payload.update(extra_input)

        payload: dict[str, Any] = {
            "assistant_id": self.assistant_id,
            "input": input_payload,
            "metadata": {
                "request_id": request.request_id,
                "user_id": request.user_id,
                "tenant_id": request.tenant_id,
            },
            "config": self._build_base_run_config(request),
        }
        if request.callback_url:
            from ai_gateway_core.security import SafeFetchError, validate_callback_url

            try:
                payload["webhook"] = validate_callback_url(request.callback_url)
            except SafeFetchError as exc:
                raise ValidationFailedError("callback_url is not allowed") from exc
        if self.service.session_enabled and request.session_id:
            valid_thread_id = await self._ensure_thread(request.session_id, request)
            endpoint = self.thread_invoke_endpoint.format(thread_id=valid_thread_id)
            payload["config"] = self._build_base_run_config(
                request, thread_id=valid_thread_id
            )
            prep_path = f"threads/{valid_thread_id}/runs/wait"
        else:
            endpoint = self.invoke_endpoint
            prep_path = "runs/wait"
        payload = await self._prepare_remote_run_payload(
            request,
            payload=payload,
            path=prep_path,
        )
        try:
            # 构建认证头部
            auth_headers = self._build_auth_headers(request)
            result = await self.connector.post(endpoint, json=payload, headers=auth_headers)
            embedded_error = _extract_langgraph_error(result)
            if embedded_error:
                raise ValidationFailedError(
                    f"LangGraph invoke failed at {endpoint}: {embedded_error}"
                )
            return result
        except Exception as exc:
            # HTTPConnector uses httpx under the hood; surface readable upstream errors
            # instead of returning a generic 500.
            import httpx

            if isinstance(exc, httpx.HTTPStatusError):
                status = exc.response.status_code
                body = _scrub_sensitive_text(exc.response.text)
                raise ValidationFailedError(
                    f"LangGraph invoke failed ({status}) at {endpoint}: {body}"
                ) from exc
            if isinstance(exc, httpx.RequestError):
                raise ValidationFailedError(
                    f"LangGraph invoke request error at {endpoint}: {exc}"
                ) from exc
            raise

    async def _remote_stream(
        self,
        request: UnifiedRequest,
        messages: list[dict[str, Any]],
        usage_handler: Callable[[dict[str, Any]], None] | None = None,
    ) -> AsyncIterator[StreamEvent]:
        input_payload: dict[str, Any] = {"messages": messages}
        params = request.parameters or {}
        extra_input = params.get("input")
        if isinstance(extra_input, dict):
            input_payload.update(extra_input)

        # Allow caller/config to override stream_mode (LangGraph OSS vs Cloud differ).
        stream_mode = None
        if isinstance(params, dict):
            stream_mode = params.get("stream_mode")
        if stream_mode is None:
            stream_mode = self.config.get("stream_mode")
        if stream_mode is None:
            stream_mode = ["messages", "updates"]
        elif isinstance(stream_mode, str) and "," in stream_mode:
            # Support comma-separated string for convenience.
            stream_mode = [s.strip() for s in stream_mode.split(",") if s.strip()]

        payload: dict[str, Any] = {
            "assistant_id": self.assistant_id,
            "input": input_payload,
            "stream_mode": stream_mode,
            "metadata": {
                "request_id": request.request_id,
                "user_id": request.user_id,
                "tenant_id": request.tenant_id,
            },
            "config": self._build_base_run_config(request),
        }
        stream_subgraphs = None
        if isinstance(params, dict):
            stream_subgraphs = params.get("stream_subgraphs")
        if stream_subgraphs is None:
            stream_subgraphs = self.config.get("stream_subgraphs")
        if stream_subgraphs is not None:
            payload["stream_subgraphs"] = stream_subgraphs

        import logging
        import time

        import httpx

        logger = logging.getLogger(__name__)

        t_method_start = time.perf_counter()

        if self.service.session_enabled and request.session_id:
            t_ensure_start = time.perf_counter()
            valid_thread_id = await self._ensure_thread(request.session_id, request)
            t_ensure_end = time.perf_counter()
            logger.info(f"[TIMING] _ensure_thread: {(t_ensure_end - t_ensure_start) * 1000:.2f}ms")
            endpoint = self.thread_stream_endpoint.format(thread_id=valid_thread_id)
            payload["config"] = self._build_base_run_config(
                request, thread_id=valid_thread_id
            )
            prep_path = f"threads/{valid_thread_id}/runs/stream"
        else:
            endpoint = self.stream_endpoint
            prep_path = "runs/stream"
        payload = await self._prepare_remote_run_payload(
            request,
            payload=payload,
            path=prep_path,
        )

        client = getattr(self.connector, "_client", None)
        if client is None:
            raise ValidationFailedError("remote streaming requires HTTPConnector")

        # Use shorter connect timeout, longer read timeout for streaming
        stream_timeout = httpx.Timeout(connect=5.0, read=300.0, write=30.0, pool=10.0)
        auth_headers = self._build_auth_headers(request)

        t_pre_stream = time.perf_counter()
        logger.info(
            f"[TIMING] Pre-stream setup: {(t_pre_stream - t_method_start) * 1000:.2f}ms (includes _ensure_thread if applicable)"
        )

        t_start = time.perf_counter()
        first_data_time = None
        first_yield_time = None

        try:
            async with client.stream(
                "POST", endpoint, json=payload, timeout=stream_timeout, headers=auth_headers
            ) as resp:
                t_response = time.perf_counter()
                logger.info(
                    f"[TIMING] LangGraph HTTP response: {(t_response - t_start) * 1000:.2f}ms (connect+server processing), status={resp.status_code}"
                )
                logger.info(
                    f"[TIMING] Total to HTTP response: {(t_response - t_method_start) * 1000:.2f}ms from method start"
                )

                if resp.status_code != 200:
                    error_text = await resp.aread()
                    scrubbed_error = _scrub_sensitive_text(error_text.decode("utf-8", errors="ignore"))
                    logger.error(
                        f"LangGraph stream error: {resp.status_code} - {scrubbed_error}"
                    )
                    raise ValidationFailedError(
                        f"LangGraph stream failed ({resp.status_code}) at {endpoint}: {scrubbed_error}"
                    )

                current_event_type = ""
                last_content_length = 0
                last_tool_args: dict[str, str] = {}  # Store actual args text, not just length
                current_tool_call_id = ""
                current_tool_name = ""
                sent_tool_call_starts: set = (
                    set()
                )  # Track tool calls that have sent TOOL_CALL_START

                # Use aiter_bytes for real-time streaming (aiter_lines can buffer)
                line_count = 0
                buffer = ""
                decoder = codecs.getincrementaldecoder("utf-8")(errors="ignore")
                stream_done = False  # Flag to break out of outer loop
                async for chunk in resp.aiter_bytes():
                    if stream_done:
                        break
                    if first_data_time is None:
                        first_data_time = time.perf_counter()
                        logger.info(
                            f"[TIMING] LangGraph first chunk: {(first_data_time - t_start) * 1000:.2f}ms"
                        )

                    # Decode and buffer
                    # HTTP chunks can split a UTF-8 code point; keep decoder state
                    # across reads while retaining the existing malformed-byte policy.
                    buffer += decoder.decode(chunk)

                    # Process complete lines (SSE events end with \n\n or \n)
                    while "\n" in buffer:
                        line, buffer = buffer.split("\n", 1)
                        line = line.strip()
                        line_count += 1
                        if not line:
                            current_event_type = ""
                            continue
                        # 解析 SSE event 类型
                        if line.startswith("event:"):
                            current_event_type = line[6:].strip()
                            # A 200 stream can still fail, including a subgraph.
                            # Never expose or log the provider's error payload.
                            if current_event_type.split("|", 1)[0] == "error":
                                raise ValidationFailedError("langgraph_provider_error")
                            logger.debug(f"SSE event type: {current_event_type}")
                            # Check for end event type - signal stream completion
                            if current_event_type == "end":
                                logger.info("[STREAM] Received 'end' event, stream complete")
                                stream_done = True
                                break
                            continue
                        if not line.startswith("data:"):
                            continue
                        data_str = line[5:].strip()
                        if not data_str or data_str == "[DONE]":
                            logger.info("[STREAM] Received [DONE] signal, stream complete")
                            stream_done = True
                            break
                        try:
                            data = json.loads(data_str)
                        except Exception as e:
                            logger.warning(f"Failed to parse SSE data: {e}")
                            continue

                        if (
                            not current_event_type
                            and isinstance(data, dict)
                            and isinstance(data.get("event"), str)
                            and data["event"].split("|", 1)[0] == "error"
                        ):
                            raise ValidationFailedError("langgraph_provider_error")

                        if current_event_type == "metadata":
                            if isinstance(data, dict):
                                usage = data.get("usage")
                                if isinstance(usage, dict) and usage_handler:
                                    usage_handler(usage)
                            continue

                        # Debug: log received event type and content preview
                        if current_event_type:
                            content_preview = ""
                            msg_type_info = ""
                            if (
                                isinstance(data, list)
                                and len(data) > 0
                                and isinstance(data[0], dict)
                            ):
                                msg = data[0]
                                c = msg.get("content", "")
                                msg_type_info = f", msg_type={msg.get('type', 'unknown')}"
                                if isinstance(c, str):
                                    content_preview = f", content_len={len(c)}"
                            logger.debug(
                                f"[SSE] event={current_event_type}{msg_type_info}{content_preview}"
                            )

                        # messages/complete indicates a message boundary, not a stream end.

                        # 构建完整事件对象以便统一处理
                        event = (
                            {"event": current_event_type, "data": data}
                            if current_event_type
                            else data
                        )
                        # 立即发送 thinking 事件让前端知道我们在处理
                        if first_yield_time is None and current_event_type:
                            first_yield_time = time.perf_counter()
                            logger.info(
                                f"[TIMING] LangGraph sending thinking signal: {(first_yield_time - t_start) * 1000:.2f}ms, event_type={current_event_type}"
                            )
                            yield StreamEvent(event_type=StreamEventType.THINKING)

                        result = self._extract_remote_stream_events(
                            event,
                            last_content_length,
                            last_tool_args,
                            current_tool_call_id,
                            current_tool_name,
                            sent_tool_call_starts,
                        )
                        (
                            stream_events,
                            last_content_length,
                            last_tool_args,
                            current_tool_call_id,
                            current_tool_name,
                        ) = result
                        for stream_event in stream_events:
                            logger.debug(
                                f"[TIMING] LangGraph yield: event_type={stream_event.event_type}, text_len={len(stream_event.text) if stream_event.text else 0}"
                            )
                            yield stream_event
        except httpx.RequestError as exc:
            raise ValidationFailedError(
                f"LangGraph stream request error at {endpoint}: {exc}"
            ) from exc

        logger.debug("LangGraph stream completed")

    @staticmethod
    def _extract_ai_message_text_delta(
        msg: Any,
        last_content_length: int,
    ) -> tuple[StreamEvent | None, int]:
        if not isinstance(msg, dict) or msg.get("type", "") not in (
            "ai",
            "AIMessage",
            "AIMessageChunk",
        ):
            return None, last_content_length

        content = msg.get("content", "")
        if isinstance(content, list):
            text_parts: list[str] = []
            for item in content:
                if isinstance(item, str):
                    text_parts.append(item)
                elif isinstance(item, dict) and item.get("type") == "text":
                    text_parts.append(str(item.get("text", "")))
            content = "".join(text_parts)

        if not isinstance(content, str) or len(content) <= last_content_length:
            return None, last_content_length

        return (
            StreamEvent(
                event_type=StreamEventType.TEXT_DELTA,
                text=content[last_content_length:],
            ),
            len(content),
        )

    def _extract_remote_stream_events(
        self,
        event: Any,
        last_content_length: int,
        last_tool_args: dict[str, str],
        current_tool_call_id: str,
        current_tool_name: str,
        sent_tool_call_starts: set,
    ) -> tuple[list[StreamEvent], int, dict[str, str], str, str]:
        """Project one upstream event into every observable downstream event."""

        (
            primary_event,
            last_content_length,
            last_tool_args,
            current_tool_call_id,
            current_tool_name,
        ) = self._extract_remote_stream_event(
            event,
            last_content_length,
            last_tool_args,
            current_tool_call_id,
            current_tool_name,
            sent_tool_call_starts,
        )
        events = [primary_event] if primary_event is not None else []

        if not any(item.event_type == StreamEventType.TEXT_DELTA for item in events):
            event_type = event.get("event", "") if isinstance(event, dict) else ""
            data = event.get("data") if isinstance(event, dict) and "event" in event else event
            if event_type in ("messages", "messages/partial"):
                msg = data[0] if isinstance(data, list) and data else data
                text_event, last_content_length = self._extract_ai_message_text_delta(
                    msg,
                    last_content_length,
                )
                if text_event is not None:
                    events.append(text_event)

        return (
            events,
            last_content_length,
            last_tool_args,
            current_tool_call_id,
            current_tool_name,
        )

    def _extract_remote_stream_event(
        self,
        event: Any,
        last_content_length: int,
        last_tool_args: dict[str, str],  # Changed: stores actual args text, not just length
        current_tool_call_id: str,
        current_tool_name: str,
        sent_tool_call_starts: set,  # Track tool calls that have sent TOOL_CALL_START
    ) -> tuple[StreamEvent | None, int, dict[str, str], str, str]:
        """从远程 LangGraph API 的流式事件中提取流事件（返回增量）"""
        import logging

        logger = logging.getLogger(__name__)

        if not isinstance(event, dict):
            return (
                None,
                last_content_length,
                last_tool_args,
                current_tool_call_id,
                current_tool_name,
            )

        event_type = event.get("event", "")
        data = event.get("data") if "event" in event else event

        # 详细调试日志 - 显示每个收到的事件
        if event_type:
            # 提取消息类型和内容长度用于调试
            debug_info = f"event_type={event_type}"
            if isinstance(data, list) and len(data) > 0 and isinstance(data[0], dict):
                msg = data[0]
                msg_type = msg.get("type", "unknown")
                content = msg.get("content", "")
                content_len = len(content) if isinstance(content, str) else 0
                debug_info += f", msg_type={msg_type}, content_len={content_len}, last_len={last_content_length}"
            logger.debug(f"[STREAM] Processing: {debug_info}")

        # 处理 messages/complete 事件 - 重置状态
        if event_type == "messages/complete":
            # 消息完成，重置累积长度以准备下一条消息
            return None, 0, {}, "", ""

        # 处理 end 事件
        if event_type == "end":
            return (
                None,
                last_content_length,
                last_tool_args,
                current_tool_call_id,
                current_tool_name,
            )

        # 处理 error 事件
        if event_type == "error":
            import logging

            logging.getLogger(__name__).error(f"LangGraph stream error event: {data}")
            return (
                None,
                last_content_length,
                last_tool_args,
                current_tool_call_id,
                current_tool_name,
            )

        # 处理 metadata 事件（忽略）
        if event_type == "metadata":
            return (
                None,
                last_content_length,
                last_tool_args,
                current_tool_call_id,
                current_tool_name,
            )

        # LangGraph Cloud API 的 messages/partial 事件类型
        if event_type in ("messages", "messages/partial"):
            # 格式: data: [message, metadata] 或 data: message
            msg = data[0] if isinstance(data, list) and len(data) >= 1 else data

            if isinstance(msg, dict):
                msg_type = msg.get("type", "")

                # 优先处理 tool_call_chunks
                tool_call_chunks = msg.get("tool_call_chunks")
                if (
                    tool_call_chunks
                    and isinstance(tool_call_chunks, list)
                    and len(tool_call_chunks) > 0
                ):
                    tc = tool_call_chunks[0]
                    # id 和 name 可能只在第一个 chunk 中有值
                    tc_id = tc.get("id") or tc.get("tool_call_id") or ""
                    tc_name = tc.get("name") or ""

                    # 如果有新的 id，更新当前追踪的工具调用
                    if tc_id:
                        current_tool_call_id = tc_id
                    if tc_name:
                        current_tool_name = tc_name

                    # 获取 args - LangGraph 可能发送累积值或增量值
                    tc_args = tc.get("args") or ""
                    if isinstance(tc_args, dict):
                        tc_args = json.dumps(tc_args, ensure_ascii=False)

                    logger.debug(
                        f"[tool_call_chunks] id={tc_id}, name={tc_name}, args={tc_args!r}, tracked_id={current_tool_call_id}"
                    )

                    key = current_tool_call_id or "default"

                    # 检测是否是完整 JSON 对象（累积格式）
                    args_stripped = tc_args.strip()
                    is_complete_json = args_stripped.startswith("{") and args_stripped.endswith("}")

                    if is_complete_json:
                        # 累积格式：发送完整值，前端会处理
                        # 使用实际 args 长度来判断是否有更新
                        prev_args = last_tool_args.get(key, "")
                        prev_len = len(prev_args)
                        logger.debug(
                            f"[tool_call_chunks] Complete JSON detected, len={len(tc_args)}, prev_len={prev_len}"
                        )

                        # 检查是否是空占位符（如 {"query": ""}）
                        is_empty_placeholder = False
                        try:
                            parsed = json.loads(tc_args)
                            if isinstance(parsed, dict):
                                # 检查所有值是否为空
                                is_empty_placeholder = all(
                                    v == "" or v is None or v == [] for v in parsed.values()
                                )
                        except json.JSONDecodeError:
                            pass

                        if len(tc_args) <= prev_len:
                            # 没有新内容，跳过
                            return (
                                None,
                                last_content_length,
                                last_tool_args,
                                current_tool_call_id,
                                current_tool_name,
                            )

                        # 如果是空占位符，不存储（让后续真实值可以覆盖）
                        if is_empty_placeholder:
                            logger.debug(
                                f"[tool_call_chunks] Skipping empty placeholder: {tc_args!r}"
                            )
                            return (
                                None,
                                last_content_length,
                                last_tool_args,
                                current_tool_call_id,
                                current_tool_name,
                            )

                        # 存储实际 args 文本
                        last_tool_args[key] = tc_args
                        logger.debug(f"[tool_call_chunks] Stored args for {key}: {tc_args!r}")
                    # else: 增量格式，直接使用

                    # 只有当有实际内容时才返回
                    if tc_name or tc_args:
                        # 判断是否首次发送此工具调用（发送 TOOL_CALL_START）
                        tool_key = current_tool_call_id or "default"
                        if tool_key not in sent_tool_call_starts:
                            sent_tool_call_starts.add(tool_key)
                            event_type = StreamEventType.TOOL_CALL_START
                            logger.debug(
                                f"[tool_call_chunks] Sending TOOL_CALL_START: name={current_tool_name}, args={tc_args!r}"
                            )
                        else:
                            event_type = StreamEventType.TOOL_CALL_DELTA
                            logger.debug(
                                f"[tool_call_chunks] Sending TOOL_CALL_DELTA: name={current_tool_name}, args={tc_args!r}"
                            )
                        return (
                            StreamEvent(
                                event_type=event_type,
                                tool_call=ToolCall(
                                    tool_call_id=current_tool_call_id,
                                    name=current_tool_name,
                                    arguments=tc_args,
                                    status="running",
                                ),
                            ),
                            last_content_length,
                            last_tool_args,
                            current_tool_call_id,
                            current_tool_name,
                        )

                    return (
                        None,
                        last_content_length,
                        last_tool_args,
                        current_tool_call_id,
                        current_tool_name,
                    )

                # 处理完整的 tool_calls（累积格式，需要计算增量）
                tool_calls = msg.get("tool_calls")
                if tool_calls and isinstance(tool_calls, list) and len(tool_calls) > 0:
                    tc = tool_calls[0]
                    tc_id = tc.get("id") or tc.get("tool_call_id") or ""
                    tc_name = tc.get("name") or ""
                    tc_args = tc.get("args") or tc.get("arguments") or ""
                    if isinstance(tc_args, dict):
                        tc_args = json.dumps(tc_args, ensure_ascii=False)

                    # 更新当前追踪
                    if tc_id:
                        current_tool_call_id = tc_id
                    if tc_name:
                        current_tool_name = tc_name

                    # 检查是否有新内容（累积格式，发送完整值让前端处理）
                    key = current_tool_call_id or "default"
                    prev_args = last_tool_args.get(key, "")

                    # 检查是否是空占位符
                    is_empty_prev = False
                    if prev_args:
                        try:
                            parsed_prev = json.loads(prev_args)
                            if isinstance(parsed_prev, dict):
                                is_empty_prev = all(
                                    v == "" or v is None or v == [] for v in parsed_prev.values()
                                )
                        except json.JSONDecodeError:
                            pass

                    # 如果之前是空占位符，或者有新内容
                    if is_empty_prev or len(tc_args) > len(prev_args):
                        # 存储实际 args 文本
                        last_tool_args[key] = tc_args
                        logger.debug(f"[tool_calls] Stored args for {key}: {tc_args!r}")

                        # 判断是否首次发送此工具调用（发送 TOOL_CALL_START）
                        tool_key = current_tool_call_id or "default"
                        if tool_key not in sent_tool_call_starts:
                            sent_tool_call_starts.add(tool_key)
                            event_type = StreamEventType.TOOL_CALL_START
                            logger.debug(
                                f"[tool_calls] Sending TOOL_CALL_START: name={current_tool_name}"
                            )
                        else:
                            event_type = StreamEventType.TOOL_CALL_DELTA
                            logger.debug(
                                f"[tool_calls] Sending TOOL_CALL_DELTA: name={current_tool_name}"
                            )

                        # 发送完整累积值（不是增量），前端会处理
                        return (
                            StreamEvent(
                                event_type=event_type,
                                tool_call=ToolCall(
                                    tool_call_id=current_tool_call_id,
                                    name=current_tool_name,
                                    arguments=tc_args,  # 发送完整值
                                    status="running",
                                ),
                            ),
                            last_content_length,
                            last_tool_args,
                            current_tool_call_id,
                            current_tool_name,
                        )

                    return (
                        None,
                        last_content_length,
                        last_tool_args,
                        current_tool_call_id,
                        current_tool_name,
                    )

                # 处理工具结果消息（完整内容，不需要增量）
                if msg_type in ("tool", "ToolMessage"):
                    tool_call_id = msg.get("tool_call_id", "")
                    tool_name = msg.get("name", "")
                    content = msg.get("content", "")
                    if isinstance(content, dict):
                        content = json.dumps(content, ensure_ascii=False)

                    logger.debug(f"[ToolMessage] keys: {list(msg.keys())}")
                    logger.debug(
                        f"[ToolMessage] tool_name={tool_name}, tool_call_id={tool_call_id}"
                    )

                    # 尝试从多个位置提取实际使用的查询参数
                    final_args = ""

                    # 1. 尝试从 artifact 中提取
                    artifact = msg.get("artifact")
                    if artifact:
                        logger.debug(
                            f"[ToolMessage] artifact type={type(artifact).__name__}, value={artifact}"
                        )
                    if isinstance(artifact, dict) and "query" in artifact:
                        # RAG 工具的 artifact 可能包含 {"query": "...", ...}
                        final_args = json.dumps(
                            {"query": artifact["query"]}, ensure_ascii=False
                        )
                        logger.debug(
                            f"[ToolMessage] Extracted query from artifact: {final_args}"
                        )

                    # 2. 尝试从 additional_kwargs 获取
                    if not final_args:
                        additional_kwargs = msg.get("additional_kwargs", {})
                        logger.debug(f"[ToolMessage] additional_kwargs: {additional_kwargs}")
                        if isinstance(additional_kwargs, dict) and "query" in additional_kwargs:
                            final_args = json.dumps(
                                {"query": additional_kwargs["query"]}, ensure_ascii=False
                            )
                            logger.debug(
                                f"[ToolMessage] Extracted query from additional_kwargs: {final_args}"
                            )

                    # 3. 尝试从 tool_input 获取（某些 LangGraph 版本使用此字段）
                    if not final_args:
                        tool_input = msg.get("tool_input")
                        if tool_input:
                            logger.debug(f"[ToolMessage] tool_input: {tool_input}")
                        if isinstance(tool_input, dict):
                            if "query" in tool_input:
                                final_args = json.dumps(
                                    {"query": tool_input["query"]}, ensure_ascii=False
                                )
                            else:
                                # 使用整个 tool_input 作为 args
                                final_args = json.dumps(tool_input, ensure_ascii=False)
                            logger.debug(f"[ToolMessage] Extracted from tool_input: {final_args}")

                    # 4. 如果还是没有，使用之前追踪的 args（存储的完整 args 文本）
                    if not final_args:
                        # 尝试用 tool_call_id 查找
                        stored_args = last_tool_args.get(tool_call_id, "")
                        # 如果没找到，尝试用 current_tool_call_id
                        if not stored_args and current_tool_call_id:
                            stored_args = last_tool_args.get(current_tool_call_id, "")
                        if stored_args:
                            # 检查存储的 args 是否有实际内容（不是空占位符）
                            try:
                                parsed = json.loads(stored_args)
                                if isinstance(parsed, dict):
                                    has_content = any(
                                        v and v != "" and v != [] and v is not None
                                        for v in parsed.values()
                                    )
                                    if has_content:
                                        final_args = stored_args
                                        logger.debug(
                                            f"[ToolMessage] Using stored args: {final_args}"
                                        )
                                    else:
                                        logger.debug(
                                            f"[ToolMessage] Stored args is empty placeholder: {stored_args}"
                                        )
                                else:
                                    final_args = stored_args
                                    logger.debug(
                                        f"[ToolMessage] Using stored args (non-dict): {final_args}"
                                    )
                            except json.JSONDecodeError:
                                # 如果不是有效 JSON，仍然使用它
                                final_args = stored_args
                                logger.debug(
                                    f"[ToolMessage] Using stored args (non-JSON): {final_args}"
                                )

                    logger.debug(f"[ToolMessage] Final args to send: {final_args or '(empty)'}")

                    # 重置工具调用追踪
                    current_tool_call_id = ""
                    current_tool_name = ""

                    return (
                        StreamEvent(
                            event_type=StreamEventType.TOOL_RESULT,
                            text=str(content),
                            tool_call=ToolCall(
                                tool_call_id=tool_call_id,
                                name=tool_name,
                                arguments=final_args,
                                status=_tool_result_status(msg),
                            ),
                        ),
                        last_content_length,
                        last_tool_args,
                        current_tool_call_id,
                        current_tool_name,
                    )

                # 处理 AI 消息类型的文本（累积内容，需要计算增量）
                if msg_type in ("ai", "AIMessage", "AIMessageChunk"):
                    text_event, new_length = self._extract_ai_message_text_delta(
                        msg,
                        last_content_length,
                    )
                    if text_event is not None:
                        logger.debug(
                            f"[STREAM] TEXT_DELTA: delta_len={len(text_event.text)}, total_len={new_length}"
                        )
                        return (
                            text_event,
                            new_length,
                            last_tool_args,
                            current_tool_call_id,
                            current_tool_name,
                        )

            return (
                None,
                last_content_length,
                last_tool_args,
                current_tool_call_id,
                current_tool_name,
            )

        # 处理 updates 事件（节点更新）- 这些是完整内容，不需要增量计算
        if event_type == "updates":
            if isinstance(data, dict):
                # 检查是否有工具节点的更新
                for node_name, node_data in data.items():
                    if node_name == "tools" and isinstance(node_data, dict):
                        messages = node_data.get("messages", [])
                        if isinstance(messages, list):
                            for msg in messages:
                                if isinstance(msg, dict) and msg.get("type") in (
                                    "tool",
                                    "ToolMessage",
                                ):
                                    tool_call_id = msg.get("tool_call_id", "")
                                    tool_name = msg.get("name", "")
                                    content = msg.get("content", "")
                                    if isinstance(content, dict):
                                        content = json.dumps(content, ensure_ascii=False)

                                    # 尝试使用存储的 args
                                    stored_args = last_tool_args.get(
                                        tool_call_id, ""
                                    ) or last_tool_args.get(current_tool_call_id, "")
                                    logger.debug(
                                        f"[updates] TOOL_RESULT: tool_call_id={tool_call_id}, stored_args={stored_args!r}"
                                    )

                                    return (
                                        StreamEvent(
                                            event_type=StreamEventType.TOOL_RESULT,
                                            text=str(content),
                                            tool_call=ToolCall(
                                                tool_call_id=tool_call_id,
                                                name=tool_name,
                                                arguments=stored_args,
                                                status=_tool_result_status(msg),
                                            ),
                                        ),
                                        last_content_length,
                                        last_tool_args,
                                        current_tool_call_id,
                                        current_tool_name,
                                    )
            return (
                None,
                last_content_length,
                last_tool_args,
                current_tool_call_id,
                current_tool_name,
            )

        return None, last_content_length, last_tool_args, current_tool_call_id, current_tool_name

    async def _ensure_thread(self, session_id: str, request: UnifiedRequest) -> str:
        """
        确保 Thread 存在。如果 session_id 不是有效的 UUID，会生成一个新的 UUID。
        使用二级缓存确保同一 session_id 始终映射到同一个 thread_id：
        - L1: 本地类级别缓存（最快，进程内）
        - L2: Redis 分布式缓存（跨进程/节点）

        返回实际使用的 thread_id（UUID 格式）。

        重要：此方法会同步等待 thread 创建完成，以避免后续请求因 thread 不存在而失败。
        使用缓存来减少后续请求的开销。
        """
        import logging
        import time
        import uuid

        import httpx

        logger = logging.getLogger(__name__)

        t_start = time.perf_counter()

        # L1: 检查本地缓存 - 最快路径
        if session_id in self._session_to_thread_map:
            cached_id = self._session_to_thread_map[session_id]
            logger.debug(f"[TIMING] _ensure_thread: L1 cache hit for {session_id}")
            return cached_id

        # L2: 检查 Redis 缓存 - 分布式缓存
        redis_storage = await self._get_redis_storage()
        if redis_storage:
            try:
                cached_thread_id = await redis_storage.get_thread_mapping(session_id)
                if cached_thread_id:
                    # 回填 L1 缓存
                    self._session_to_thread_map[session_id] = cached_thread_id
                    t_end = time.perf_counter()
                    logger.debug(
                        f"[TIMING] _ensure_thread: L2 cache hit, {(t_end - t_start) * 1000:.2f}ms"
                    )
                    return cached_thread_id
            except Exception as e:
                logger.warning(f"Redis cache lookup failed: {e}")

        # 缓存未命中 - 需要创建或验证 thread

        # 验证是否为有效 UUID
        try:
            uuid.UUID(session_id)
            valid_thread_id = session_id
        except ValueError:
            # 不是有效 UUID，生成一个新的
            valid_thread_id = str(uuid.uuid4())

        # 同步创建 thread - 确保 thread 存在再继续
        # 这会增加首次请求的延迟，但确保后续流式请求不会失败
        try:
            auth_headers = self._build_auth_headers(request)
            created = await self.connector.post(
                "/threads",
                json={
                    "thread_id": valid_thread_id,
                    "if_exists": "do_nothing",
                    "metadata": {
                        "user_id": request.user_id,
                        "tenant_id": request.tenant_id,
                        "original_session_id": session_id,
                    },
                },
                headers=auth_headers,
            )
        except httpx.HTTPStatusError as exc:
            if exc.response.status_code != 409:
                raise ValidationFailedError(
                    f"LangGraph thread creation failed ({exc.response.status_code})"
                ) from None
            # Some servers report an existing explicit ID as conflict even with
            # if_exists=do_nothing. Observe that exact ID; never send a run or
            # retry creation merely because the conflicting thread might exist.
            try:
                created = await self.connector.get(
                    f"/threads/{valid_thread_id}", headers=auth_headers
                )
            except Exception:
                raise ValidationFailedError("langgraph_thread_readback_failed") from None
        except Exception:
            # Do not leak upstream response bodies, URLs or exception messages.
            # CancelledError is not caught: cancellation cannot populate caches.
            raise ValidationFailedError("langgraph_thread_creation_failed") from None

        # The Thread API returns JSON with required UUID identity, including an
        # existing thread under if_exists=do_nothing. An empty ack is not proof.
        if not isinstance(created, dict) or "__error__" in created:
            raise ValidationFailedError("langgraph_thread_identity_unconfirmed")
        returned_id = created.get("thread_id")
        try:
            matches = isinstance(returned_id, str) and uuid.UUID(returned_id) == uuid.UUID(valid_thread_id)
        except ValueError:
            matches = False
        if not matches:
            raise ValidationFailedError("langgraph_thread_identity_unconfirmed")
        logger.debug("LangGraph thread creation acknowledged")

        # 更新二级缓存
        # L1: 本地缓存
        self._session_to_thread_map[session_id] = valid_thread_id

        # L2: Redis 缓存（异步，不阻塞主流程）
        if redis_storage:
            try:
                await redis_storage.set_thread_mapping(session_id, valid_thread_id)
                logger.debug(f"Thread mapping cached to Redis: {session_id} -> {valid_thread_id}")
            except Exception as e:
                logger.warning(f"Redis cache write failed: {e}")

        t_end = time.perf_counter()
        logger.info(f"[TIMING] _ensure_thread: {(t_end - t_start) * 1000:.2f}ms (new thread)")

        return valid_thread_id

    async def _get_redis_storage(self):
        """
        获取 Redis 存储实例（懒加载）。

        Returns:
            RedisStorage 实例，如果未启用或获取失败则返回 None
        """
        try:
            from ..container import get_container

            container = get_container()
            redis = container.redis
            if redis and redis.enabled:
                return redis
        except Exception:
            pass
        return None
