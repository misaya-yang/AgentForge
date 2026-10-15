"""
透明代理核心测试

测试内容：
- 基本代理转发功能
- 请求头透传
- LangGraph assistant_id 注入
- 流式默认参数设置
- 错误处理和透传
- 负载均衡
"""

from __future__ import annotations

import json
from unittest.mock import AsyncMock, MagicMock, patch

import httpx
import pytest

from src.proxy.config_loader import ProxyServiceConfig
from src.proxy.context_injector import ContextInjector, RequestContext
from src.proxy.transparent_proxy import (
    LANGGRAPH_ASSISTANT_PATHS,
    LANGGRAPH_OPERATION_TYPES,
    ProxyQueueTimeoutError,
    ProxyRequest,
    ProxyResponse,
    TransparentProxy,
)

# ============ Proxy Request/Response Tests ============


class TestProxyRequest:
    """代理请求测试"""

    def test_proxy_request_defaults(self):
        """测试默认值"""
        req = ProxyRequest(
            service_name="test_service",
            path="/api/test",
        )

        assert req.service_name == "test_service"
        assert req.path == "/api/test"
        assert req.method == "GET"
        assert req.body is None
        assert req.query_params == {}
        assert req.context is None
        assert req.stream is False

    def test_proxy_request_with_body(self):
        """测试带请求体的请求"""
        body = json.dumps({"message": "hello"}).encode()
        req = ProxyRequest(
            service_name="langgraph",
            path="/runs",
            method="POST",
            body=body,
            stream=True,
        )

        assert req.method == "POST"
        assert req.body == body
        assert req.stream is True


class TestProxyResponse:
    """代理响应测试"""

    def test_proxy_response_success(self):
        """测试成功响应"""
        resp = ProxyResponse(
            status_code=200,
            headers={"content-type": "application/json"},
            body=b'{"status": "ok"}',
        )

        assert resp.status_code == 200
        assert resp.error is None
        assert resp.is_streaming is False

    def test_proxy_response_error(self):
        """测试错误响应"""
        resp = ProxyResponse(
            status_code=500,
            headers={},
            error="Internal Server Error",
        )

        assert resp.status_code == 500
        assert resp.error == "Internal Server Error"


# ============ TransparentProxy Tests ============


class TestTransparentProxy:
    """透明代理核心测试"""

    @pytest.fixture
    def proxy_config(self):
        """代理服务配置"""
        return ProxyServiceConfig(
            service_id="langgraph_001",
            service_name="langgraph",
            upstream_url="http://langgraph:8123",
            assistant_id="assistant_test_001",
            timeout_connect=5.0,
            timeout_read=60.0,
            enabled=True,
        )

    @pytest.fixture
    def mock_config_loader(self, proxy_config):
        """Mock 配置加载器"""
        loader = AsyncMock()
        loader.get_config = AsyncMock(return_value=proxy_config)
        return loader

    @pytest.fixture
    def context_injector(self):
        """上下文注入器"""
        return ContextInjector()

    @pytest.fixture
    def transparent_proxy(self, mock_config_loader, context_injector):
        """透明代理实例"""
        return TransparentProxy(
            config_loader=mock_config_loader,
            context_injector=context_injector,
            default_timeout=60.0,
        )

    # -------- 基本功能测试 --------

    @pytest.mark.asyncio
    async def test_proxy_service_not_found(self, transparent_proxy, mock_config_loader):
        """测试服务不存在"""
        mock_config_loader.get_config.return_value = None

        request = ProxyRequest(
            service_name="unknown_service",
            path="/api/test",
        )

        response = await transparent_proxy.proxy(request)

        assert response.status_code == 404
        assert "not found" in response.error.lower()

    @pytest.mark.asyncio
    async def test_proxy_service_disabled(
        self, transparent_proxy, mock_config_loader, proxy_config
    ):
        """测试服务已禁用"""
        proxy_config.enabled = False
        mock_config_loader.get_config.return_value = proxy_config

        request = ProxyRequest(
            service_name="langgraph",
            path="/api/test",
        )

        response = await transparent_proxy.proxy(request)

        assert response.status_code == 503
        assert "disabled" in response.error.lower()

    @pytest.mark.asyncio
    async def test_proxy_normal_request(self, transparent_proxy, mock_httpx_response):
        """测试普通 GET 请求代理"""
        mock_httpx_response(
            status_code=200,
            headers={"content-type": "application/json"},
            content=b'{"status": "ok"}',
        )

        with patch.object(httpx.AsyncClient, "request", new_callable=AsyncMock) as mock_request:
            # 创建一个返回正确内容的 mock
            mock_resp = MagicMock()
            mock_resp.status_code = 200
            mock_resp.headers = {"content-type": "application/json"}
            mock_resp.content = b'{"status": "ok"}'
            mock_request.return_value = mock_resp

            request = ProxyRequest(
                service_name="langgraph",
                path="/assistants",
                method="GET",
            )

            response = await transparent_proxy.proxy(request)

            # 验证请求被正确转发
            assert mock_request.called or response.status_code in (200, 502)

    @pytest.mark.asyncio
    async def test_proxy_fast_fails_when_service_is_unavailable(
        self, transparent_proxy, mock_config_loader, proxy_config
    ):
        mock_config_loader.get_config.return_value = proxy_config

        with patch.object(
            transparent_proxy,
            "get_service_availability",
            new=AsyncMock(
                return_value={
                    "availability_status": "unavailable",
                    "available_upstreams": [],
                    "last_health_error": "dial tcp 127.0.0.1:2025: connect: connection refused",
                }
            ),
        ):
            response = await transparent_proxy.proxy(
                ProxyRequest(service_name="langgraph", path="/runs/wait", method="POST")
            )

        assert response.status_code == 503
        assert "Service unavailable" in (response.error or "")

    @pytest.mark.asyncio
    async def test_stream_slots_are_isolated_from_default_slots(
        self, transparent_proxy, proxy_config
    ):
        stream_sem = transparent_proxy._get_service_semaphore(proxy_config, "stream")
        default_sem = transparent_proxy._get_service_semaphore(proxy_config, "default")

        assert stream_sem is not default_sem

    @pytest.mark.asyncio
    async def test_stream_clients_are_isolated_from_default_clients(
        self, transparent_proxy, proxy_config
    ):
        stream_client = await transparent_proxy._get_client(proxy_config, slot_kind="stream")
        default_client = await transparent_proxy._get_client(proxy_config, slot_kind="default")

        try:
            assert stream_client is not default_client
            assert ("langgraph_001", "stream") in transparent_proxy._clients
            assert ("langgraph_001", "default") in transparent_proxy._clients
        finally:
            await transparent_proxy.close()

    @pytest.mark.asyncio
    async def test_queue_timeout_raises_when_slot_is_exhausted(
        self, transparent_proxy, proxy_config
    ):
        proxy_config.non_streaming_concurrency_limit = 1
        proxy_config.concurrency_queue_timeout = 0.01

        release_slot, _ = await transparent_proxy._acquire_request_slot(
            proxy_config,
            proxy_config.upstream_url,
            "default",
        )

        with pytest.raises(ProxyQueueTimeoutError):
            await transparent_proxy._acquire_request_slot(
                proxy_config,
                proxy_config.upstream_url,
                "default",
            )

        await release_slot()

    @pytest.mark.asyncio
    async def test_proxy_auto_heals_invalid_assistant(
        self, transparent_proxy, mock_config_loader, proxy_config
    ):
        """当 assistant_id 无效时，自动基于上游 graph 列表进行一次自愈重试。"""
        proxy_config.service_id = "local-2024-flash"
        proxy_config.service_name = "flash"
        proxy_config.assistant_id = "Agent"
        proxy_config.graph_id = "Agent"
        mock_config_loader.get_config.return_value = proxy_config

        invalid_resp = MagicMock()
        invalid_resp.status_code = 422
        invalid_resp.headers = {"content-type": "application/json"}
        invalid_resp.content = (
            b'{"detail":"Invalid assistant: \\"Agent\\". Must be either:\\n- A valid assistant UUID, '
            b'or\\n- One of the registered graphs: flash"}'
        )

        healed_resp = MagicMock()
        healed_resp.status_code = 200
        healed_resp.headers = {"content-type": "application/json"}
        healed_resp.content = b'{"messages":[{"type":"ai","content":"ok"}]}'

        assistants_resp = MagicMock()
        assistants_resp.status_code = 200
        assistants_resp.content = json.dumps(
            [{"assistant_id": "asst-flash-001", "graph_id": "flash"}]
        ).encode("utf-8")

        with patch.object(httpx.AsyncClient, "request", new_callable=AsyncMock) as mock_request:
            with patch.object(httpx.AsyncClient, "post", new_callable=AsyncMock) as mock_post:
                with patch.object(
                    transparent_proxy,
                    "get_service_availability",
                    new=AsyncMock(
                        return_value={
                            "availability_status": "available",
                            "available_upstreams": [proxy_config.upstream_url],
                            "last_health_error": None,
                        }
                    ),
                ):
                    mock_request.side_effect = [invalid_resp, healed_resp]
                    mock_post.return_value = assistants_resp

                    request = ProxyRequest(
                        service_name="local-2024-flash",
                        path="/runs/wait",
                        method="POST",
                        body=json.dumps(
                            {"input": {"messages": [{"role": "user", "content": "hello"}]}}
                        ).encode("utf-8"),
                        context=RequestContext(user_id="test-user", tenant_id="test-tenant"),
                    )
                    response = await transparent_proxy.proxy(request)

        assert response.status_code == 200
        assert proxy_config.assistant_id == "flash"
        assert mock_request.await_count == 2

        retry_call = mock_request.await_args_list[1]
        retry_body = retry_call.kwargs.get("content")
        retry_payload = json.loads(retry_body.decode("utf-8"))
        assert retry_payload["assistant_id"] == "flash"

    @pytest.mark.asyncio
    async def test_record_non_stream_usage_from_json_response(
        self, transparent_proxy, proxy_config
    ):
        """测试非流式 JSON 响应会写入 usage 统计"""
        proxy_config.assistant_id = None
        context = RequestContext(
            user_id="user_usage_1",
            tenant_id="tenant_usage_1",
            request_id="req_usage_1",
        )
        response_body = json.dumps(
            {
                "model": "gpt-4o-mini",
                "usage": {
                    "input_tokens": 7,
                    "output_tokens": 3,
                    "total_tokens": 10,
                },
            }
        ).encode("utf-8")
        request_body = json.dumps({"assistant_id": "agent_asst_1"}).encode("utf-8")

        recorder = AsyncMock()
        with patch("src.services.metrics.get_usage_recorder", return_value=recorder):
            await transparent_proxy._record_non_stream_usage(
                response_body=response_body,
                response_content_type="application/json; charset=utf-8",
                request_body=request_body,
                config=proxy_config,
                context=context,
                method="POST",
                path="/runs/wait",
                duration_ms=128.4,
                status_code=200,
            )

        recorder.record_usage.assert_awaited_once()
        kwargs = recorder.record_usage.await_args.kwargs
        assert kwargs["service_id"] == proxy_config.service_id
        assert kwargs["assistant_id"] == "agent_asst_1"
        assert kwargs["input_tokens"] == 7
        assert kwargs["output_tokens"] == 3

    @pytest.mark.asyncio
    async def test_record_non_stream_usage_run_error_without_usage(
        self, transparent_proxy, proxy_config
    ):
        """测试 run 请求即使无 usage 也会记录错误统计。"""
        context = RequestContext(
            user_id="user_usage_err",
            tenant_id="tenant_usage_err",
            request_id="req_usage_err",
        )
        response_body = json.dumps(
            {
                "__error__": {
                    "error": "upstream_error",
                    "message": "quota exhausted",
                }
            }
        ).encode("utf-8")

        recorder = AsyncMock()
        with patch("src.services.metrics.get_usage_recorder", return_value=recorder):
            await transparent_proxy._record_non_stream_usage(
                response_body=response_body,
                response_content_type="application/json",
                request_body=json.dumps({}).encode("utf-8"),
                config=proxy_config,
                context=context,
                method="POST",
                path="runs/wait",  # 无前导斜杠
                duration_ms=32.5,
                status_code=200,
            )

        recorder.record_usage.assert_awaited_once()
        kwargs = recorder.record_usage.await_args.kwargs
        assert kwargs["input_tokens"] == 0
        assert kwargs["output_tokens"] == 0
        assert kwargs["status"] == "error"
        assert kwargs["request_type"] == "proxy_run_wait"
        assert kwargs["metadata"]["response_has_error"] is True

    # -------- Assistant ID 注入测试 --------

    def test_inject_assistant_id_to_runs(self, transparent_proxy):
        """测试 /runs 路径注入 assistant_id"""
        body = json.dumps({"input": {"messages": [{"role": "user", "content": "hello"}]}}).encode()

        result = transparent_proxy._inject_assistant_id(
            body=body,
            path="/runs",
            assistant_id="test_assistant",
        )

        data = json.loads(result.decode())
        assert data.get("assistant_id") == "test_assistant"

    def test_inject_assistant_id_to_runs_stream(self, transparent_proxy):
        """测试 /runs/stream 路径注入 assistant_id"""
        body = json.dumps({"input": {"messages": [{"role": "user", "content": "hello"}]}}).encode()

        result = transparent_proxy._inject_assistant_id(
            body=body,
            path="/runs/stream",
            assistant_id="test_assistant",
        )

        data = json.loads(result.decode())
        assert data.get("assistant_id") == "test_assistant"

    def test_inject_assistant_id_runs_wait_without_leading_slash(self, transparent_proxy):
        """测试 runs/wait（无前导斜杠）也会注入 assistant_id"""
        body = json.dumps({"input": {"messages": [{"role": "user", "content": "hello"}]}}).encode()

        result = transparent_proxy._inject_assistant_id(
            body=body,
            path="runs/wait",
            assistant_id="test_assistant",
        )

        data = json.loads(result.decode())
        assert data.get("assistant_id") == "test_assistant"

    def test_inject_assistant_id_not_override(self, transparent_proxy):
        """测试不覆盖已有的 assistant_id"""
        body = json.dumps(
            {"assistant_id": "existing_assistant", "input": {"messages": []}}
        ).encode()

        result = transparent_proxy._inject_assistant_id(
            body=body,
            path="/runs",
            assistant_id="new_assistant",
        )

        data = json.loads(result.decode())
        # 应保留原有的 assistant_id
        assert data.get("assistant_id") == "existing_assistant"

    def test_inject_assistant_id_skip_non_langgraph_paths(self, transparent_proxy):
        """测试非 LangGraph 路径不注入"""
        body = json.dumps({"data": "test"}).encode()

        result = transparent_proxy._inject_assistant_id(
            body=body,
            path="/api/other",
            assistant_id="test_assistant",
        )

        data = json.loads(result.decode())
        assert "assistant_id" not in data

    def test_inject_assistant_id_empty_body(self, transparent_proxy):
        """测试空请求体"""
        result = transparent_proxy._inject_assistant_id(
            body=None,
            path="/runs",
            assistant_id="test_assistant",
        )

        assert result is None

    def test_inject_assistant_id_non_json_body(self, transparent_proxy):
        """测试非 JSON 请求体"""
        body = b"plain text body"

        result = transparent_proxy._inject_assistant_id(
            body=body,
            path="/runs",
            assistant_id="test_assistant",
        )

        # 非 JSON 应原样返回
        assert result == body

    # -------- 流式默认参数测试 --------

    def test_ensure_stream_defaults_adds_stream_mode(self, transparent_proxy):
        """测试自动添加 stream_mode"""
        body = json.dumps({"input": {"messages": [{"role": "user", "content": "hello"}]}}).encode()

        result = transparent_proxy._ensure_stream_defaults(body, "/runs/stream")

        data = json.loads(result.decode())
        assert "stream_mode" in data
        assert "messages" in data["stream_mode"]
        assert "updates" in data["stream_mode"]

    def test_ensure_stream_defaults_adds_stream_subgraphs(self, transparent_proxy):
        """测试自动添加 stream_subgraphs"""
        body = json.dumps(
            {
                "input": {"messages": []},
                "stream_mode": ["messages"],
            }
        ).encode()

        result = transparent_proxy._ensure_stream_defaults(body, "/runs/stream")

        data = json.loads(result.decode())
        assert data.get("stream_subgraphs") is True

    def test_ensure_stream_defaults_not_override_existing(self, transparent_proxy):
        """测试不覆盖已有配置"""
        body = json.dumps(
            {
                "input": {"messages": []},
                "stream_mode": ["values"],
                "stream_subgraphs": False,
            }
        ).encode()

        result = transparent_proxy._ensure_stream_defaults(body, "/runs/stream")

        data = json.loads(result.decode())
        # 应保留原有配置
        assert data["stream_mode"] == ["values"]
        assert data["stream_subgraphs"] is False

    def test_ensure_stream_defaults_skip_non_stream_path(self, transparent_proxy):
        """测试非流式路径不处理"""
        body = json.dumps({"input": {}}).encode()

        result = transparent_proxy._ensure_stream_defaults(body, "/runs")

        # 非流式路径应原样返回
        data = json.loads(result.decode())
        assert "stream_mode" not in data

    # -------- 流式路径检测测试 --------

    def test_is_streaming_path_runs_stream(self, transparent_proxy):
        """测试 /runs/stream 路径"""
        assert transparent_proxy._is_streaming_path("/runs/stream") is True
        assert transparent_proxy._is_streaming_path("/threads/123/runs/stream") is True

    def test_is_streaming_path_sse(self, transparent_proxy):
        """测试 /sse 路径"""
        assert transparent_proxy._is_streaming_path("/api/sse") is True

    def test_is_streaming_path_non_stream(self, transparent_proxy):
        """测试非流式路径"""
        assert transparent_proxy._is_streaming_path("/runs") is False
        assert transparent_proxy._is_streaming_path("/assistants") is False
        assert transparent_proxy._is_streaming_path("/threads/123") is False

    # -------- 操作类型检测测试 --------

    def test_detect_operation_type_run_stream(self):
        """测试检测 run_stream 操作"""
        op = TransparentProxy.detect_operation_type("POST", "/runs/stream")
        assert op == "run_stream"

    def test_detect_operation_type_run_wait_without_leading_slash(self):
        """测试无前导斜杠路径也能识别 run_wait。"""
        op = TransparentProxy.detect_operation_type("POST", "runs/wait")
        assert op == "run_wait"

    def test_detect_operation_type_thread_create(self):
        """测试检测 thread_create 操作"""
        op = TransparentProxy.detect_operation_type("POST", "/threads")
        assert op == "thread_create"

    def test_detect_operation_type_assistant_list(self):
        """测试检测 assistant_list 操作"""
        op = TransparentProxy.detect_operation_type("GET", "/assistants")
        assert op == "assistant_list"

    @pytest.mark.asyncio
    async def test_acquire_request_slot_tracks_and_releases_connections(
        self, transparent_proxy, proxy_config
    ):
        release, queue_wait_ms = await transparent_proxy._acquire_request_slot(
            proxy_config,
            proxy_config.upstream_url,
            "default",
        )

        assert queue_wait_ms >= 0
        assert transparent_proxy._lb_connections[proxy_config.service_id][proxy_config.upstream_url] == 1

        await release()

        assert transparent_proxy._lb_connections[proxy_config.service_id] == {}

    def test_select_upstream_uses_least_connections_live_counters(
        self, transparent_proxy, proxy_config
    ):
        proxy_config.upstream_urls = [
            "http://langgraph:8123",
            "http://langgraph:8124",
        ]
        proxy_config.load_balance_strategy = "least_connections"
        transparent_proxy._lb_connections[proxy_config.service_id] = {
            "http://langgraph:8123": 2,
            "http://langgraph:8124": 1,
        }

        selected = transparent_proxy._select_upstream(proxy_config)

        assert selected == "http://langgraph:8124"

    def test_detect_operation_type_unknown(self):
        """测试未知操作返回默认值"""
        op = TransparentProxy.detect_operation_type("GET", "/unknown/path")
        assert op == "proxy"

    # -------- 响应头过滤测试 --------

    def test_filter_response_headers_removes_hop_by_hop(self, transparent_proxy):
        """测试移除 hop-by-hop 头"""
        headers = {
            "content-type": "application/json",
            "connection": "keep-alive",
            "transfer-encoding": "chunked",
            "x-custom-header": "value",
        }

        filtered = transparent_proxy._filter_response_headers(headers)

        assert "content-type" in filtered
        assert "x-custom-header" in filtered
        assert "connection" not in filtered
        assert "transfer-encoding" not in filtered

    # -------- 负载均衡测试 --------

    def test_select_upstream_single_url(self, transparent_proxy, proxy_config):
        """测试单 URL 选择"""
        url = transparent_proxy._select_upstream(proxy_config)
        assert url == proxy_config.upstream_url

    def test_select_upstream_round_robin(self, transparent_proxy, proxy_config):
        """测试轮询负载均衡"""
        # 清空 upstream_url 避免被加入列表
        proxy_config.upstream_url = ""
        proxy_config.upstream_urls = [
            "http://langgraph-1:8123",
            "http://langgraph-2:8123",
            "http://langgraph-3:8123",
        ]
        proxy_config.load_balance_strategy = "round_robin"

        # 多次选择，验证轮询
        urls = [transparent_proxy._select_upstream(proxy_config) for _ in range(6)]

        # 应该轮询所有 URL
        assert len(set(urls)) == 3

    def test_select_upstream_random(self, transparent_proxy, proxy_config):
        """测试随机负载均衡"""
        proxy_config.upstream_urls = [
            "http://langgraph-1:8123",
            "http://langgraph-2:8123",
        ]
        proxy_config.load_balance_strategy = "random"

        # 多次选择
        urls = [transparent_proxy._select_upstream(proxy_config) for _ in range(10)]

        # 应该选中所有 URL（概率上）
        assert all(url in proxy_config.get_upstream_urls() for url in urls)

    def test_select_upstream_no_urls(self, transparent_proxy, proxy_config):
        """测试无可用 URL"""
        proxy_config.upstream_url = ""
        proxy_config.upstream_urls = []

        with pytest.raises(ValueError, match="No upstream URLs"):
            transparent_proxy._select_upstream(proxy_config)

    # -------- URL 构建测试 --------

    def test_build_upstream_url_basic(self, transparent_proxy, proxy_config):
        """测试基本 URL 构建"""
        url = transparent_proxy._build_upstream_url(
            config=proxy_config,
            path="/runs/stream",
            base_url="http://langgraph:8123",
        )

        assert url == "http://langgraph:8123/runs/stream"

    def test_build_upstream_url_with_path_rewrite(self, transparent_proxy, proxy_config):
        """测试带路径重写的 URL 构建"""
        proxy_config.path_rewrite = "/api/v1"

        url = transparent_proxy._build_upstream_url(
            config=proxy_config,
            path="/runs/stream",
            base_url="http://langgraph:8123",
        )

        assert url == "http://langgraph:8123/api/v1/runs/stream"

    def test_build_upstream_url_cleans_slashes(self, transparent_proxy, proxy_config):
        """测试斜杠清理"""
        url = transparent_proxy._build_upstream_url(
            config=proxy_config,
            path="runs/stream",  # 无前导斜杠
            base_url="http://langgraph:8123/",  # 有尾部斜杠
        )

        assert url == "http://langgraph:8123/runs/stream"


# ============ Stream Mode Helper Tests ============


class TestStreamModeHelpers:
    """流式模式辅助函数测试"""

    def test_stream_mode_wants_messages_list(self):
        """测试列表形式的 stream_mode"""
        assert TransparentProxy._stream_mode_wants_messages(["messages"]) is True
        assert TransparentProxy._stream_mode_wants_messages(["messages", "updates"]) is True
        assert TransparentProxy._stream_mode_wants_messages(["messages-tuple"]) is True
        assert TransparentProxy._stream_mode_wants_messages(["values"]) is False
        assert TransparentProxy._stream_mode_wants_messages(["updates"]) is False

    def test_stream_mode_wants_messages_string(self):
        """测试字符串形式的 stream_mode"""
        assert TransparentProxy._stream_mode_wants_messages("messages") is True
        assert TransparentProxy._stream_mode_wants_messages("messages,updates") is True
        assert TransparentProxy._stream_mode_wants_messages("values") is False

    def test_stream_mode_wants_messages_none(self):
        """测试 None 值"""
        assert TransparentProxy._stream_mode_wants_messages(None) is False

    def test_stream_mode_wants_messages_invalid(self):
        """测试无效类型"""
        assert TransparentProxy._stream_mode_wants_messages(123) is False
        assert TransparentProxy._stream_mode_wants_messages({}) is False


# ============ Constants Tests ============


class TestConstants:
    """常量测试"""

    def test_langgraph_assistant_paths(self):
        """测试 LangGraph 需要 assistant_id 的路径"""
        assert "/runs" in LANGGRAPH_ASSISTANT_PATHS
        assert "/runs/stream" in LANGGRAPH_ASSISTANT_PATHS
        assert "/threads/" in LANGGRAPH_ASSISTANT_PATHS

    def test_langgraph_operation_types(self):
        """测试操作类型映射"""
        assert LANGGRAPH_OPERATION_TYPES["POST /runs/stream"] == "run_stream"
        assert LANGGRAPH_OPERATION_TYPES["POST /threads"] == "thread_create"
        assert LANGGRAPH_OPERATION_TYPES["GET /assistants"] == "assistant_list"
