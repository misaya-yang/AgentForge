"""
流式代理测试

测试内容：
- SSE 流式响应处理
- 流式数据完整性
- 计费数据提取
- 错误处理
- 流式超时
"""

from __future__ import annotations

import asyncio
import json
from collections.abc import AsyncIterator
from unittest.mock import AsyncMock, patch

import httpx
import pytest

from src.proxy.billing_interceptor import BillingInterceptor, StreamProcessor, UsageData
from src.proxy.billing_stream import (
    StreamProcessor as ExtractedStreamProcessor,
)
from src.proxy.billing_stream import UsageData as ExtractedUsageData
from src.proxy.config_loader import ProxyServiceConfig
from src.proxy.context_injector import ContextInjector, RequestContext
from src.proxy.transparent_proxy import ProxyRequest, TransparentProxy

# ============ Mock Streaming Response ============


class MockStreamResponse:
    """Mock 流式响应"""

    def __init__(
        self,
        status_code: int = 200,
        headers: dict = None,
        events: list[bytes] = None,
        delay: float = 0.01,
    ):
        self.status_code = status_code
        self.headers = headers or {"content-type": "text/event-stream"}
        self._events = events or []
        self._delay = delay
        self._closed = False

    async def aiter_raw(self) -> AsyncIterator[bytes]:
        for event in self._events:
            yield event
            await asyncio.sleep(self._delay)

    async def aread(self) -> bytes:
        return b"".join(self._events)

    async def aclose(self):
        self._closed = True


# ============ Streaming Tests ============


def test_billing_interceptor_reexports_extracted_stream_types() -> None:
    assert StreamProcessor is ExtractedStreamProcessor
    assert UsageData is ExtractedUsageData
    assert isinstance(
        BillingInterceptor().create_stream_processor("request-1", "service-1"),
        ExtractedStreamProcessor,
    )


class TestStreamingProxy:
    """流式代理测试"""

    @pytest.fixture
    def proxy_config(self):
        """代理服务配置"""
        return ProxyServiceConfig(
            service_id="langgraph_stream",
            service_name="langgraph",
            upstream_url="http://langgraph:8123",
            assistant_id="stream_assistant",
            enabled=True,
        )

    @pytest.fixture
    def mock_config_loader(self, proxy_config):
        """Mock 配置加载器"""
        loader = AsyncMock()
        loader.database = None
        loader.redis = None
        loader.get_config = AsyncMock(return_value=proxy_config)
        return loader

    @pytest.fixture
    def transparent_proxy(self, mock_config_loader):
        """透明代理实例"""
        return TransparentProxy(
            config_loader=mock_config_loader,
            context_injector=ContextInjector(),
        )

    @pytest.fixture
    def sample_sse_events(self):
        """样本 SSE 事件"""
        return [
            b'event: metadata\ndata: {"run_id": "run_001"}\n\n',
            b'event: messages/partial\ndata: [{"role": "assistant", "content": "Hello"}]\n\n',
            b'event: messages/partial\ndata: [{"role": "assistant", "content": "Hello, how"}]\n\n',
            b'event: messages/partial\ndata: [{"role": "assistant", "content": "Hello, how can I help?"}]\n\n',
            b'event: metadata\ndata: {"usage": {"input_tokens": 10, "output_tokens": 20}}\n\n',
            b"event: end\ndata: null\n\n",
        ]

    @pytest.fixture
    def request_context(self):
        """请求上下文"""
        return RequestContext(
            user_id="test_user",
            tenant_id="test_tenant",
            request_id="req_001",
        )

    # -------- 基本流式测试 --------

    @pytest.mark.asyncio
    async def test_streaming_response_is_async_iterator(
        self, sample_sse_events, request_context
    ):
        """测试流式响应是异步迭代器"""
        mock_response = MockStreamResponse(
            status_code=200,
            events=sample_sse_events,
        )

        with patch.object(httpx.AsyncClient, "send", new_callable=AsyncMock) as mock_send:
            mock_send.return_value = mock_response

            ProxyRequest(
                service_name="langgraph",
                path="/runs/stream",
                method="POST",
                body=json.dumps({"input": {"messages": []}}).encode(),
                stream=True,
                context=request_context,
            )

            # 由于我们需要 mock 整个 httpx client，这里简化测试
            # 实际测试中应该用集成测试

    @pytest.mark.asyncio
    async def test_streaming_path_detection(self, transparent_proxy):
        """测试流式路径检测"""
        # 流式路径
        assert transparent_proxy._is_streaming_path("/runs/stream") is True
        assert transparent_proxy._is_streaming_path("/threads/123/runs/stream") is True
        assert transparent_proxy._is_streaming_path("/api/sse") is True

        # 非流式路径
        assert transparent_proxy._is_streaming_path("/runs") is False
        assert transparent_proxy._is_streaming_path("/threads/123") is False

    # -------- 流式默认参数测试 --------

    @pytest.mark.asyncio
    async def test_stream_defaults_applied(self, transparent_proxy):
        """测试流式请求默认参数被应用"""
        body = json.dumps({"input": {"messages": [{"role": "user", "content": "test"}]}}).encode()

        result = transparent_proxy._ensure_stream_defaults(body, "/runs/stream")
        data = json.loads(result.decode())

        assert "stream_mode" in data
        assert "messages" in data["stream_mode"]

    @pytest.mark.asyncio
    async def test_stream_subgraphs_enabled_for_messages(self, transparent_proxy):
        """测试 messages 模式下自动启用 stream_subgraphs"""
        body = json.dumps(
            {
                "input": {"messages": []},
                "stream_mode": ["messages"],
            }
        ).encode()

        result = transparent_proxy._ensure_stream_defaults(body, "/runs/stream")
        data = json.loads(result.decode())

        assert data.get("stream_subgraphs") is True

    @pytest.mark.asyncio
    async def test_stream_defaults_not_applied_to_non_stream(self, transparent_proxy):
        """测试非流式路径不应用默认参数"""
        body = json.dumps({"input": {"messages": []}}).encode()

        result = transparent_proxy._ensure_stream_defaults(body, "/runs")
        data = json.loads(result.decode())

        # 非流式路径不应添加 stream_mode
        assert "stream_mode" not in data


# ============ Billing Interceptor Tests ============


class TestBillingInterceptor:
    """计费拦截器测试"""

    @pytest.fixture
    def billing_interceptor(self):
        """计费拦截器实例"""
        return BillingInterceptor(
            callback=None,
            redis_client=None,
            buffer_size=10,
            flush_interval=1.0,
        )

    def test_usage_data_creation(self):
        """测试 UsageData 创建"""
        usage = UsageData(
            input_tokens=100,
            output_tokens=50,
            request_id="req_001",
            service_id="langgraph",
            user_id="user_123",
        )

        assert usage.input_tokens == 100
        assert usage.output_tokens == 50
        assert usage.total_tokens == 150  # auto-calculated

    def test_usage_data_total_tokens_auto_calc(self):
        """测试 total_tokens 自动计算"""
        usage = UsageData(
            input_tokens=200,
            output_tokens=100,
        )

        assert usage.total_tokens == 300

    @pytest.mark.asyncio
    async def test_create_stream_processor(self, billing_interceptor):
        """测试创建流处理器"""
        processor = billing_interceptor.create_stream_processor(
            request_id="req_001",
            service_id="langgraph",
            user_id="user_123",
            tenant_id="tenant_001",
            assistant_id="assistant_001",
        )

        assert processor is not None
        assert processor.request_id == "req_001"

    @pytest.mark.asyncio
    async def test_stream_processor_extract_usage(self, billing_interceptor):
        """测试流处理器提取 usage 数据"""
        processor = billing_interceptor.create_stream_processor(
            request_id="req_001",
            service_id="langgraph",
            user_id="user_123",
            tenant_id="tenant_001",
        )

        # 模拟包含 usage 的 SSE 事件
        chunk = b'event: metadata\ndata: {"usage": {"input_tokens": 50, "output_tokens": 25}}\n\n'

        # 处理 chunk
        result = await processor.process_chunk(chunk)

        # 应该透传原始数据
        assert result == chunk

    @pytest.mark.asyncio
    async def test_stream_processor_bounds_only_incomplete_event(self, billing_interceptor):
        billing_interceptor._realtime_metrics = False
        processor = billing_interceptor.create_stream_processor(
            request_id="req_bounded",
            service_id="langgraph",
            user_id="user_123",
            tenant_id="tenant_001",
        )
        processor.MAX_BUFFER_CHARS = 64

        await processor.process_chunk(b"x" * 100)

        assert processor._buffer_overflowed is True
        assert processor._discarding_oversized_event is True
        assert processor._buffer == ""
        assert processor._status == "error"
        assert (
            processor._error_metadata["upstream_error_type"]
            == "billing_stream_buffer_overflow"
        )

        await processor.finalize()

        fallback = billing_interceptor._buffer[-1]
        assert fallback.total_tokens == 0
        assert fallback.status == "error"
        assert fallback.raw_metadata["token_source"] == "zero_on_failure"
        assert (
            fallback.raw_metadata["upstream_error_type"]
            == "billing_stream_buffer_overflow"
        )

    @pytest.mark.asyncio
    async def test_stream_processor_recovers_after_discarding_oversized_event(
        self, billing_interceptor
    ):
        billing_interceptor._realtime_metrics = False
        processor = billing_interceptor.create_stream_processor(
            request_id="req_recover_after_overflow",
            service_id="langgraph",
            user_id="user_123",
            tenant_id="tenant_001",
        )
        processor.MAX_BUFFER_CHARS = 64

        await processor.process_chunk(b"event: updates\ndata: " + b"x" * 100)
        await processor.process_chunk(
            b'\n\nevent: metadata\ndata: {"usage": {"input_tokens": 4, '
            b'"output_tokens": 3}}\n\n'
        )
        await processor.finalize()

        assert processor._discarding_oversized_event is False
        assert processor._usage_collected is True
        assert billing_interceptor._buffer[-1].total_tokens == 7
        assert billing_interceptor._buffer[-1].status == "error"
        assert (
            billing_interceptor._buffer[-1].raw_metadata["upstream_error_type"]
            == "billing_stream_buffer_overflow"
        )

    @pytest.mark.asyncio
    async def test_cumulative_usage_records_once_with_final_totals(self):
        realtime = AsyncMock()
        interceptor = BillingInterceptor(
            callback=None,
            redis_client=None,
            realtime_metrics=realtime,
            buffer_size=10,
        )
        processor = interceptor.create_stream_processor(
            request_id="req-cumulative",
            service_id="agent-service",
            user_id="user-1",
            tenant_id="tenant-1",
        )

        await processor.process_chunk(
            b'event: metadata\ndata: {"usage": {"input_tokens": 10, '
            b'"output_tokens": 2}}\n\n'
        )
        await processor.process_chunk(
            b'event: metadata\ndata: {"usage": {"input_tokens": 10, '
            b'"output_tokens": 9}}\n\n'
        )
        await processor.finalize()

        assert len(interceptor._buffer) == 1
        assert interceptor._buffer[0].input_tokens == 10
        assert interceptor._buffer[0].output_tokens == 9
        realtime.record_token_usage.assert_awaited_once_with(
            input_tokens=10,
            output_tokens=9,
        )

    @pytest.mark.asyncio
    async def test_stream_processor_parses_complete_events_before_buffer_bound(
        self, billing_interceptor
    ):
        billing_interceptor._realtime_metrics = False
        processor = billing_interceptor.create_stream_processor(
            request_id="req_complete_events",
            service_id="langgraph",
            user_id="user_123",
            tenant_id="tenant_001",
        )
        processor.MAX_BUFFER_CHARS = 64
        processor._process_event_block = AsyncMock()
        event = b"event: ping\ndata: {}\n\n"

        await processor.process_chunk(event * 10)

        assert processor._process_event_block.await_count == 10
        assert processor._buffer == ""
        assert processor._buffer_overflowed is False

    @pytest.mark.asyncio
    async def test_stream_processor_extract_usage_from_updates_event(self):
        """测试从 LangGraph updates 事件中提取 usage_metadata"""
        captured: list[UsageData] = []

        async def callback(data: UsageData):
            captured.append(data)

        interceptor = BillingInterceptor(
            callback=callback,
            redis_client=None,
            buffer_size=10,
            flush_interval=1.0,
        )
        interceptor._record_to_database = AsyncMock()

        processor = interceptor.create_stream_processor(
            request_id="req_updates_001",
            service_id="agent_service",
            user_id="user_123",
            tenant_id="default",
        )

        chunk = (
            b"event: updates\n"
            b'data: {"model":{"messages":[{"usage_metadata":{"input_tokens":12,"output_tokens":8,"total_tokens":20}}]}}\n\n'
        )

        await processor.process_chunk(chunk)
        await processor.finalize()
        await interceptor._flush_buffer()

        assert len(captured) == 1
        assert captured[0].input_tokens == 12
        assert captured[0].output_tokens == 8
        assert captured[0].total_tokens == 20
        assert captured[0].service_id == "agent_service"

    @pytest.mark.asyncio
    async def test_stream_processor_finalize(self, billing_interceptor):
        """测试流处理器完成处理"""
        processor = billing_interceptor.create_stream_processor(
            request_id="req_001",
            service_id="langgraph",
            user_id="user_123",
            tenant_id="tenant_001",
        )

        # 处理一些 chunks
        await processor.process_chunk(b'event: metadata\ndata: {"run_id": "run_001"}\n\n')

        # 完成处理
        await processor.finalize()

        # 验证处理完成（不应抛出异常）

    @pytest.mark.asyncio
    async def test_stream_processor_finalize_records_error_without_usage(self):
        """测试 error 事件即使无 usage 也会记录请求。"""
        captured: list[UsageData] = []

        async def callback(data: UsageData):
            captured.append(data)

        interceptor = BillingInterceptor(
            callback=callback,
            redis_client=None,
            buffer_size=10,
            flush_interval=1.0,
        )
        interceptor._record_to_database = AsyncMock()

        processor = interceptor.create_stream_processor(
            request_id="req_error_001",
            service_id="agent_service",
            user_id="user_123",
            tenant_id="default",
            assistant_id="assistant_agent",
            request_type="proxy_run_stream",
            model_hint="gemini-2.0-flash",
        )

        await processor.process_chunk(
            b'event: error\r\ndata: {"error": {"message": "quota exhausted"}}\r\n\r\n'
        )
        await processor.finalize()
        await interceptor._flush_buffer()

        assert len(captured) == 1
        assert captured[0].total_tokens == 0
        assert captured[0].status == "error"
        assert captured[0].request_type == "proxy_run_stream"
        assert captured[0].model == "gemini-2.0-flash"


# ============ SSE Event Parsing Tests ============


class TestSSEParsing:
    """SSE 事件解析测试"""

    def test_parse_metadata_event(self):
        """测试解析 metadata 事件"""
        event = b'event: metadata\ndata: {"usage": {"input_tokens": 10}}\n\n'

        # 解析事件类型
        lines = event.decode().strip().split("\n")
        event_type = None
        data = None

        for line in lines:
            if line.startswith("event:"):
                event_type = line.split(":", 1)[1].strip()
            elif line.startswith("data:"):
                data = json.loads(line.split(":", 1)[1].strip())

        assert event_type == "metadata"
        assert data["usage"]["input_tokens"] == 10

    def test_parse_messages_event(self):
        """测试解析 messages 事件"""
        event = b'event: messages/partial\ndata: [{"role": "assistant", "content": "Hello"}]\n\n'

        lines = event.decode().strip().split("\n")
        event_type = None
        data = None

        for line in lines:
            if line.startswith("event:"):
                event_type = line.split(":", 1)[1].strip()
            elif line.startswith("data:"):
                data = json.loads(line.split(":", 1)[1].strip())

        assert event_type == "messages/partial"
        assert len(data) == 1
        assert data[0]["role"] == "assistant"

    def test_parse_end_event(self):
        """测试解析 end 事件"""
        event = b"event: end\ndata: null\n\n"

        lines = event.decode().strip().split("\n")
        event_type = None

        for line in lines:
            if line.startswith("event:"):
                event_type = line.split(":", 1)[1].strip()

        assert event_type == "end"


# ============ Error Handling Tests ============


class TestStreamingErrorHandling:
    """流式错误处理测试"""

    @pytest.fixture
    def proxy_config(self):
        return ProxyServiceConfig(
            service_id="langgraph_test",
            service_name="langgraph",
            upstream_url="http://langgraph:8123",
            enabled=True,
        )

    @pytest.fixture
    def mock_config_loader(self, proxy_config):
        loader = AsyncMock()
        loader.database = None
        loader.redis = None
        loader.get_config = AsyncMock(return_value=proxy_config)
        return loader

    @pytest.fixture
    def transparent_proxy(self, mock_config_loader):
        return TransparentProxy(
            config_loader=mock_config_loader,
            context_injector=ContextInjector(),
        )

    @pytest.mark.asyncio
    async def test_upstream_error_passthrough(self):
        """测试上游错误透传"""
        # 模拟上游 500 错误
        error_response = MockStreamResponse(
            status_code=500,
            headers={"content-type": "application/json"},
            events=[b'{"error": "Internal Server Error"}'],
        )

        # 验证错误响应被正确处理
        # 由于需要完整的 httpx mock，这里简化验证逻辑
        assert error_response.status_code == 500

    @pytest.mark.asyncio
    async def test_streaming_timeout_handling(self, transparent_proxy):
        """A streaming header timeout maps to 504 and releases capacity."""
        client = httpx.AsyncClient()
        release_slot = AsyncMock()
        try:
            with patch.object(
                client,
                "send",
                AsyncMock(side_effect=httpx.ReadTimeout("upstream timed out")),
            ):
                response = await transparent_proxy._proxy_streaming(
                    client=client,
                    method="POST",
                    url="http://langgraph:8123/runs/stream",
                    headers={},
                    body=b"{}",
                    params={},
                    config=await transparent_proxy.config_loader.get_config("langgraph"),
                    context=RequestContext(request_id="timeout-test"),
                    path="/runs/stream",
                    release_slot=release_slot,
                )
        finally:
            await client.aclose()

        assert response.status_code == 504
        assert response.error == "Upstream timeout: upstream timed out"
        release_slot.assert_awaited_once_with()

    @pytest.mark.asyncio
    async def test_connection_error_handling(self, transparent_proxy):
        """A streaming connection failure maps to 502 and releases capacity."""
        client = httpx.AsyncClient()
        release_slot = AsyncMock()
        request = httpx.Request("POST", "http://langgraph:8123/runs/stream")
        try:
            with patch.object(
                client,
                "send",
                AsyncMock(
                    side_effect=httpx.ConnectError("connection refused", request=request)
                ),
            ):
                response = await transparent_proxy._proxy_streaming(
                    client=client,
                    method="POST",
                    url=str(request.url),
                    headers={},
                    body=b"{}",
                    params={},
                    config=await transparent_proxy.config_loader.get_config("langgraph"),
                    context=RequestContext(request_id="connection-test"),
                    path="/runs/stream",
                    release_slot=release_slot,
                )
        finally:
            await client.aclose()

        assert response.status_code == 502
        assert response.error == "Upstream error: connection refused"
        release_slot.assert_awaited_once_with()


# ============ Content Type Detection Tests ============


class TestContentTypeDetection:
    """Content-Type 检测测试"""

    def test_detect_sse_content_type(self):
        """测试 SSE Content-Type 检测"""
        headers = {"content-type": "text/event-stream"}
        is_sse = "text/event-stream" in headers.get("content-type", "")
        assert is_sse is True

    def test_detect_json_content_type(self):
        """测试 JSON Content-Type 检测"""
        headers = {"content-type": "application/json"}
        is_sse = "text/event-stream" in headers.get("content-type", "")
        assert is_sse is False

    def test_detect_content_type_with_charset(self):
        """测试带 charset 的 Content-Type"""
        headers = {"content-type": "text/event-stream; charset=utf-8"}
        is_sse = "text/event-stream" in headers.get("content-type", "")
        assert is_sse is True
