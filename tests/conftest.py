"""
测试公共配置和 Fixtures

提供所有测试所需的共享配置：
- Mock 服务（Redis、LangGraph API）
- JWT Token 生成
- 测试客户端
- 异步事件循环配置
"""

from __future__ import annotations

import asyncio
import json
import sys
from collections.abc import AsyncGenerator
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock, patch

import dotenv
import jwt
import pytest
import pytest_asyncio
from fastapi.testclient import TestClient
from httpx import ASGITransport, AsyncClient


# ---------------------------------------------------------------------------
# Keep the suite hermetic against the developer's local .env
#
# `src/main.py` calls `load_dotenv(_env_file, override=False)` at import time.
# The first test module that transitively imports it therefore injects the real
# .env into os.environ for the rest of the session, which made outcomes depend
# on test order *and* on whichever .env the machine happens to have. Observed
# before this guard, all passing when run alone and failing in a full run:
#
#   - test_agent_plugin_catalog / test_agent_runtime_resolver inherited a real
#     GATEWAY_ASSISTANT_SHARED_SECRET and broke their anonymous-mode setup
#   - test_open_source_container_distribution read KB_EMBEDDING_MODEL as
#     'gemini-embedding-2-preview' instead of the declared default
#   - test_validate_env_quickstart saw POSTGRES_PASSWORD already set and
#     asserted on the wrong missing-variable message
#
# Neutralizing the loader is the narrowest fix that covers the whole class:
# `src/main.py` binds the name at import (`from dotenv import load_dotenv`), so
# replacing the attribute here — before any test module is imported — makes
# that call a no-op while leaving `dotenv_values` (used by a few tests to read
# .env deliberately) untouched. Tests needing a value still set it explicitly
# via monkeypatch.
#
# This must stay at conftest module level: test modules import application code
# during collection, before any fixture — even a session-scoped autouse one —
# gets a chance to run.
# ---------------------------------------------------------------------------
def _no_dotenv_during_tests(*_args: Any, **_kwargs: Any) -> bool:
    """Stand in for dotenv.load_dotenv so the suite ignores a local .env."""
    return False


dotenv.load_dotenv = _no_dotenv_during_tests

REPO_ROOT = Path(__file__).resolve().parents[1]
KNOWLEDGE_SERVICE_SRC = REPO_ROOT / "apps" / "knowledge-service" / "src"
if KNOWLEDGE_SERVICE_SRC.exists() and str(KNOWLEDGE_SERVICE_SRC) not in sys.path:
    sys.path.insert(0, str(KNOWLEDGE_SERVICE_SRC))

# ============ 测试常量 ============

TEST_JWT_SECRET = "test-secret-key-for-testing-only"
TEST_JWT_ALGORITHM = "HS256"
TEST_LANGGRAPH_URL = "http://mock-langgraph:8123"
TEST_REDIS_URL = "redis://localhost:6379/15"  # 使用 DB 15 作为测试数据库


# ============ JWT Token Fixtures ============


def create_test_token(
    user_id: str,
    tier: str = "normal",
    tenant_id: str = "test_tenant",
    roles: list[str] = None,
    expires_delta: timedelta | None = None,
    extra_claims: dict[str, Any] = None,
) -> str:
    """创建测试用 JWT Token"""
    if expires_delta is None:
        expires_delta = timedelta(hours=1)

    payload = {
        "sub": user_id,
        "user_id": user_id,
        "tier": tier,
        "tenant_id": tenant_id,
        "roles": roles or ["user"],
        "exp": datetime.now(timezone.utc) + expires_delta,
        "iat": datetime.now(timezone.utc),
    }

    if extra_claims:
        payload.update(extra_claims)

    return jwt.encode(payload, TEST_JWT_SECRET, algorithm=TEST_JWT_ALGORITHM)


@pytest.fixture
def valid_jwt_user_a() -> str:
    """User A 的有效 JWT Token"""
    return create_test_token(
        user_id="user_a_123",
        tier="normal",
        tenant_id="tenant_001",
        roles=["user", "developer"],
    )


@pytest.fixture
def valid_jwt_user_b() -> str:
    """User B 的有效 JWT Token"""
    return create_test_token(
        user_id="user_b_456",
        tier="normal",
        tenant_id="tenant_001",
        roles=["user"],
    )


@pytest.fixture
def valid_jwt_admin() -> str:
    """Admin 用户的有效 JWT Token"""
    return create_test_token(
        user_id="admin_001",
        tier="enterprise",
        tenant_id="tenant_001",
        roles=["user", "admin"],
    )


@pytest.fixture
def expired_jwt_token() -> str:
    """过期的 JWT Token"""
    return create_test_token(
        user_id="expired_user",
        expires_delta=timedelta(hours=-1),
    )


@pytest.fixture
def invalid_jwt_token() -> str:
    """无效签名的 JWT Token"""
    return jwt.encode(
        {"sub": "fake_user", "user_id": "fake_user"},
        "wrong-secret-key",
        algorithm=TEST_JWT_ALGORITHM,
    )


# ============ Mock Services ============


@pytest.fixture
def mock_redis():
    """Mock Redis 客户端"""
    redis_mock = AsyncMock()
    redis_mock.enabled = True
    redis_mock.get = AsyncMock(return_value=None)
    redis_mock.set = AsyncMock(return_value=True)
    redis_mock.incr = AsyncMock(return_value=1)
    redis_mock.expire = AsyncMock(return_value=True)
    redis_mock.delete = AsyncMock(return_value=1)
    redis_mock.ping = AsyncMock(return_value=True)
    redis_mock.close = AsyncMock()
    return redis_mock


@pytest.fixture
def mock_database():
    """Mock 数据库客户端"""
    db_mock = AsyncMock()
    db_mock.enabled = True
    db_mock.execute = AsyncMock(return_value=[])
    db_mock.fetch_one = AsyncMock(return_value=None)
    db_mock.fetch_all = AsyncMock(return_value=[])
    db_mock.close = AsyncMock()
    return db_mock


@pytest.fixture
def mock_langgraph_response():
    """Mock LangGraph API 响应"""
    return {
        "status_code": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "thread_id": "test_thread_001",
                "status": "completed",
                "output": {"messages": [{"role": "assistant", "content": "Hello!"}]},
            }
        ).encode(),
    }


@pytest.fixture
def mock_langgraph_stream_response():
    """Mock LangGraph 流式响应"""
    events = [
        b'event: metadata\ndata: {"run_id": "run_001"}\n\n',
        b'event: messages/partial\ndata: [{"role": "assistant", "content": "Hello"}]\n\n',
        b'event: messages/partial\ndata: [{"role": "assistant", "content": "Hello, how"}]\n\n',
        b'event: messages/partial\ndata: [{"role": "assistant", "content": "Hello, how can I help?"}]\n\n',
        b'event: metadata\ndata: {"usage": {"input_tokens": 10, "output_tokens": 20}}\n\n',
        b"event: end\ndata: null\n\n",
    ]

    async def stream_generator():
        for event in events:
            yield event
            await asyncio.sleep(0.01)

    return stream_generator


# ============ Proxy Config Fixtures ============


@pytest.fixture
def mock_proxy_config():
    """Mock 代理服务配置"""
    from src.proxy.config_loader import ProxyServiceConfig

    return ProxyServiceConfig(
        service_id="langgraph_test",
        service_name="langgraph",
        upstream_url=TEST_LANGGRAPH_URL,
        assistant_id="test_assistant_001",
        timeout_connect=5.0,
        timeout_read=60.0,
        enabled=True,
        forward_auth=True,
    )


@pytest.fixture
def mock_config_loader(mock_proxy_config):
    """Mock 配置加载器"""
    from src.proxy.config_loader import ProxyConfigLoader

    loader = AsyncMock(spec=ProxyConfigLoader)
    loader.get_config = AsyncMock(return_value=mock_proxy_config)
    loader.list_services = AsyncMock(return_value=[mock_proxy_config])
    return loader


# ============ Settings Fixtures ============


@pytest.fixture
def test_settings():
    """测试用配置"""
    from src.config.settings import (
        AuthenticationSettings,
        AuthJWTSettings,
        DatabaseSettings,
        ProxySettings,
        RedisSettings,
        Settings,
    )

    return Settings(
        host="127.0.0.1",
        port=8080,
        database=DatabaseSettings(enabled=False),
        redis=RedisSettings(enabled=False),
        authentication=AuthenticationSettings(
            jwt=AuthJWTSettings(
                enabled=True,
                secret=TEST_JWT_SECRET,
                algorithms=[TEST_JWT_ALGORITHM],
            )
        ),
        proxy=ProxySettings(enabled=True),
    )


# ============ Application Fixtures ============


@pytest_asyncio.fixture
async def test_app(test_settings, mock_config_loader, mock_redis):
    """创建测试应用"""
    from src.main import create_app

    # 使用 mock 配置创建应用
    with patch("src.main.Settings", return_value=test_settings):
        app = create_app()

        # 注入 mock 依赖
        app.state.proxy_config_loader = mock_config_loader
        app.state.redis = mock_redis

        yield app


@pytest_asyncio.fixture
async def async_client(test_app) -> AsyncGenerator[AsyncClient, None]:
    """异步测试客户端"""
    transport = ASGITransport(app=test_app)
    async with AsyncClient(transport=transport, base_url="http://test") as client:
        yield client


@pytest.fixture
def sync_client(test_app) -> TestClient:
    """同步测试客户端"""
    return TestClient(test_app)


# ============ Async Event Loop ============


@pytest.fixture(scope="session")
def event_loop():
    """创建事件循环（用于 session 范围的异步测试）"""
    loop = asyncio.get_event_loop_policy().new_event_loop()
    yield loop
    loop.close()


# ============ Utility Fixtures ============


@pytest.fixture
def auth_headers(valid_jwt_user_a):
    """带 JWT 的认证头"""
    return {"Authorization": f"Bearer {valid_jwt_user_a}"}


@pytest.fixture
def user_a_headers(valid_jwt_user_a):
    """User A 的认证头"""
    return {"Authorization": f"Bearer {valid_jwt_user_a}"}


@pytest.fixture
def user_b_headers(valid_jwt_user_b):
    """User B 的认证头"""
    return {"Authorization": f"Bearer {valid_jwt_user_b}"}


# ============ HTTP Response Helpers ============


class MockHttpxResponse:
    """Mock httpx Response"""

    def __init__(
        self,
        status_code: int = 200,
        headers: dict[str, str] = None,
        content: bytes = b"",
        is_stream: bool = False,
        stream_data: list[bytes] = None,
    ):
        self.status_code = status_code
        self.headers = headers or {"content-type": "application/json"}
        self.content = content
        self._is_stream = is_stream
        self._stream_data = stream_data or []
        self._closed = False

    async def aread(self) -> bytes:
        return self.content

    async def aclose(self):
        self._closed = True

    async def aiter_raw(self):
        for chunk in self._stream_data:
            yield chunk


@pytest.fixture
def mock_httpx_response():
    """创建 Mock HTTP 响应的工厂"""

    def _create(
        status_code: int = 200,
        headers: dict[str, str] = None,
        content: bytes = b"",
        is_stream: bool = False,
        stream_data: list[bytes] = None,
    ):
        return MockHttpxResponse(
            status_code=status_code,
            headers=headers,
            content=content,
            is_stream=is_stream,
            stream_data=stream_data,
        )

    return _create


# ============ Test Data Generators ============


@pytest.fixture
def langgraph_run_payload():
    """LangGraph /runs 请求 payload"""
    return {
        "input": {"messages": [{"role": "user", "content": "Hello, how are you?"}]},
    }


@pytest.fixture
def langgraph_stream_payload():
    """LangGraph /runs/stream 请求 payload"""
    return {
        "input": {"messages": [{"role": "user", "content": "Tell me a story"}]},
        "stream_mode": ["messages", "updates"],
    }
