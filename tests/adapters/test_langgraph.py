"""
LangGraph 适配器优化测试

测试内容：
- 操作类型检测
- 流式路径检测
- 上下文头注入
- Redis 缓存
- HTTP 连接池设置
"""


# ============ 操作类型检测测试 ============


class TestOperationTypeDetection:
    """LangGraph API 操作类型检测测试"""

    def test_detect_run_stream(self):
        """测试检测 run stream 操作"""
        from src.proxy.transparent_proxy import TransparentProxy

        assert TransparentProxy.detect_operation_type("POST", "/runs/stream") == "run_stream"
        assert (
            TransparentProxy.detect_operation_type("POST", "/threads/abc123/runs/stream")
            == "run_stream"
        )

    def test_detect_run_wait(self):
        """测试检测 run wait 操作"""
        from src.proxy.transparent_proxy import TransparentProxy

        assert TransparentProxy.detect_operation_type("POST", "/runs/wait") == "run_wait"
        assert (
            TransparentProxy.detect_operation_type("POST", "/threads/abc123/runs/wait")
            == "run_wait"
        )

    def test_detect_thread_operations(self):
        """测试检测 thread 操作"""
        from src.proxy.transparent_proxy import TransparentProxy

        assert TransparentProxy.detect_operation_type("POST", "/threads") == "thread_create"
        assert TransparentProxy.detect_operation_type("GET", "/threads/abc123") == "thread_read"
        assert (
            TransparentProxy.detect_operation_type("DELETE", "/threads/abc123") == "thread_delete"
        )

    def test_detect_assistant_operations(self):
        """测试检测 assistant 操作"""
        from src.proxy.transparent_proxy import TransparentProxy

        assert TransparentProxy.detect_operation_type("GET", "/assistants") == "assistant_list"
        assert (
            TransparentProxy.detect_operation_type("POST", "/assistants/search") == "assistant_list"
        )
        assert (
            TransparentProxy.detect_operation_type("GET", "/assistants/abc123") == "assistant_read"
        )

    def test_detect_store_operations(self):
        """测试检测 store 操作"""
        from src.proxy.transparent_proxy import TransparentProxy

        assert TransparentProxy.detect_operation_type("GET", "/store/items") == "store_read"
        assert TransparentProxy.detect_operation_type("POST", "/store/items") == "store_write"
        assert TransparentProxy.detect_operation_type("DELETE", "/store/items") == "store_delete"

    def test_detect_unknown_operation(self):
        """测试检测未知操作"""
        from src.proxy.transparent_proxy import TransparentProxy

        assert TransparentProxy.detect_operation_type("GET", "/unknown/path") == "proxy"


# ============ 流式路径检测测试 ============


class TestStreamingPathDetection:
    """流式路径检测测试"""

    def test_streaming_suffixes(self):
        """测试流式后缀"""
        from src.core.middleware.streaming import is_streaming_path

        assert is_streaming_path("/runs/stream") is True
        assert is_streaming_path("/threads/abc/runs/stream") is True
        assert is_streaming_path("/api/v1/stream") is True
        assert is_streaming_path("/sse") is True

    def test_streaming_prefixes(self):
        """测试流式前缀"""
        from src.core.middleware.streaming import is_streaming_path

        assert is_streaming_path("/proxy/myservice/runs/stream") is True
        assert is_streaming_path("/api/v1/proxy/myservice/stream") is True

    def test_non_streaming_paths(self):
        """测试非流式路径"""
        from src.core.middleware.streaming import is_streaming_path

        assert is_streaming_path("/assistants") is False
        assert is_streaming_path("/threads") is False
        assert is_streaming_path("/runs/wait") is False

    def test_excludes_upstream(self):
        """测试排除 upstream 路径"""
        from src.core.middleware.streaming import is_streaming_path

        assert is_streaming_path("/config/upstream") is False


# ============ 上下文头注入测试 ============


class TestContextHeaderInjection:
    """上下文头注入测试"""

    def test_langgraph_headers_injected(self):
        """测试 LangGraph 兼容头注入"""
        from src.proxy.context_injector import ContextInjector, RequestContext

        injector = ContextInjector(inject_user_info=True)
        context = RequestContext(
            user_id="user123",
            tenant_id="tenant456",
            user_tier="premium",
            is_authenticated=True,
            roles=["user", "developer"],
        )

        headers = injector.build_headers(context)

        assert headers.get("X-User-Id") == "user123"
        assert headers.get("X-Tenant-Id") == "tenant456"
        assert headers.get("X-User-Tier") == "premium"
        assert headers.get("X-User-Type") == "user"
        assert "read" in headers.get("X-User-Permissions", "")
        assert "write" in headers.get("X-User-Permissions", "")

    def test_gateway_headers_also_injected(self):
        """测试网关标准头也注入"""
        from src.proxy.context_injector import ContextInjector, RequestContext

        injector = ContextInjector(inject_user_info=True)
        context = RequestContext(
            user_id="user123",
            tenant_id="tenant456",
            user_tier="premium",
            is_authenticated=True,
        )

        headers = injector.build_headers(context)

        assert headers.get("X-GW-User-ID") == "user123"
        assert headers.get("X-GW-Tenant-ID") == "tenant456"
        assert headers.get("X-GW-User-Tier") == "premium"

    def test_anonymous_user_type(self):
        """测试匿名用户类型"""
        from src.proxy.context_injector import ContextInjector, RequestContext

        injector = ContextInjector(inject_user_info=True)
        context = RequestContext(
            user_id="",
            is_authenticated=False,
        )

        headers = injector.build_headers(context)

        assert headers.get("X-User-Type") == "anonymous"

    def test_guest_user_type(self):
        """测试游客用户类型"""
        from src.proxy.context_injector import ContextInjector, RequestContext

        injector = ContextInjector(inject_user_info=True)
        context = RequestContext(
            user_id="guest123",
            is_authenticated=False,
        )

        headers = injector.build_headers(context)

        assert headers.get("X-User-Type") == "guest"


# ============ Redis 缓存测试 ============


class TestRedisCaching:
    """Redis 缓存测试"""

    def test_thread_mapping_key_format(self):
        """测试 thread 映射键格式"""
        session_id = "session-123"
        expected_key = f"lg:thread_map:{session_id}"
        assert f"lg:thread_map:{session_id}" == expected_key

    def test_thread_cache_key_format(self):
        """测试 thread 缓存键格式"""
        thread_id = "abc-123"
        expected_key = f"lg:thread:{thread_id}"
        assert expected_key == f"lg:thread:{thread_id}"

    def test_assistant_cache_key_format(self):
        """测试 assistant 缓存键格式"""
        assistant_id = "assistant-456"
        expected_key = f"lg:assistant:{assistant_id}"
        assert expected_key == f"lg:assistant:{assistant_id}"

    def test_assistants_list_cache_key_format(self):
        """测试 assistants 列表缓存键格式"""
        user_id = "user-789"
        expected_key = f"lg:assistants_list:{user_id}"
        assert expected_key == f"lg:assistants_list:{user_id}"

    def test_quota_key_format(self):
        """测试用户配额键格式"""
        user_id = "user-789"
        expected_key = f"lg:quota:{user_id}:threads"
        assert expected_key == f"lg:quota:{user_id}:threads"


# ============ HTTP 连接池测试 ============


class TestHTTPConnectionPool:
    """HTTP 连接池优化设置测试"""

    def test_default_pool_settings(self):
        """测试默认连接池设置"""
        from src.transports.http import HTTPConnector

        assert HTTPConnector.DEFAULT_MAX_CONNECTIONS == 200
        assert HTTPConnector.DEFAULT_KEEPALIVE_CONNECTIONS == 50
        assert HTTPConnector.DEFAULT_KEEPALIVE_EXPIRY == 120.0

    def test_default_timeout_settings(self):
        """测试默认超时设置"""
        from src.transports.http import HTTPConnector

        assert HTTPConnector.DEFAULT_CONNECT_TIMEOUT == 3.0
        assert HTTPConnector.DEFAULT_READ_TIMEOUT == 300.0
        assert HTTPConnector.DEFAULT_WRITE_TIMEOUT == 60.0
        assert HTTPConnector.DEFAULT_POOL_TIMEOUT == 10.0


# ============ LangGraph Proxy 缓存测试 ============


class TestLangGraphProxyCaching:
    """LangGraph Proxy 两级缓存测试"""

    def test_cache_ttl_settings(self):
        """测试缓存 TTL 设置"""
        from src.adapters.langgraph_proxy import LangGraphProxy

        assert LangGraphProxy.THREAD_CACHE_TTL == 60
        assert LangGraphProxy.ASSISTANT_CACHE_TTL == 300
        assert LangGraphProxy.ASSISTANTS_LIST_CACHE_TTL == 60


class TestLangGraphRunMetadataInjection:
    """LangGraph Run metadata 注入测试"""

    def test_injects_gateway_domain_policy_from_assistant_metadata(self):
        """assistant metadata 显式声明 agent 时注入 gateway.domain_policy。"""
        from src.adapters.langgraph_proxy import LangGraphProxy

        merged = LangGraphProxy._inject_gateway_domain_policy_metadata(
            metadata=None,
            assistant_payload={"metadata": {"domain_policy": "agent"}},
        )

        assert isinstance(merged, dict)
        assert merged["gateway"]["domain_policy"] == "agent"

    def test_keeps_existing_gateway_domain_policy(self):
        """调用方显式传入 gateway.domain_policy 时不覆盖。"""
        from src.adapters.langgraph_proxy import LangGraphProxy

        merged = LangGraphProxy._inject_gateway_domain_policy_metadata(
            metadata={"gateway": {"domain_policy": "custom"}},
            assistant_payload={"metadata": {"domain_policy": "agent"}},
        )

        assert isinstance(merged, dict)
        assert merged["gateway"]["domain_policy"] == "custom"


class TestRemoteStreamProjection:
    """One upstream chunk may carry both a tool call and assistant text."""

    def test_tool_chunk_with_text_projects_both_events(self):
        from ai_gateway_core.enums import StreamEventType

        from src.adapters.langgraph import LangGraphAdapter

        adapter = object.__new__(LangGraphAdapter)
        event = {
            "event": "messages/partial",
            "data": [
                {
                    "type": "AIMessageChunk",
                    "content": "I am checking that now.",
                    "tool_call_chunks": [
                        {
                            "id": "call-1",
                            "name": "search_knowledge_base",
                            "args": '{"query":"contract"}',
                        }
                    ],
                }
            ],
        }

        events, content_length, args, call_id, tool_name = (
            adapter._extract_remote_stream_events(
                event,
                0,
                {},
                "",
                "",
                set(),
            )
        )

        assert [item.event_type for item in events] == [
            StreamEventType.TOOL_CALL_START,
            StreamEventType.TEXT_DELTA,
        ]
        assert events[0].tool_call.tool_call_id == "call-1"
        assert events[1].text == "I am checking that now."
        assert content_length == len("I am checking that now.")
        assert args["call-1"] == '{"query":"contract"}'
        assert call_id == "call-1"
        assert tool_name == "search_knowledge_base"

    def test_plain_text_chunk_is_not_duplicated(self):
        from ai_gateway_core.enums import StreamEventType

        from src.adapters.langgraph import LangGraphAdapter

        adapter = object.__new__(LangGraphAdapter)
        event = {
            "event": "messages/partial",
            "data": [{"type": "AIMessageChunk", "content": "Hello"}],
        }

        events, content_length, *_ = adapter._extract_remote_stream_events(
            event,
            0,
            {},
            "",
            "",
            set(),
        )

        assert len(events) == 1
        assert events[0].event_type == StreamEventType.TEXT_DELTA
        assert events[0].text == "Hello"
        assert content_length == 5
