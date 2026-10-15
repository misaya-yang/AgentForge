"""
网关集成测试

端到端测试整个链路，验证：
- 鉴权功能
- 限流功能
- 用户隔离
- 流式输出
- 游客会话
"""

import asyncio
from datetime import datetime, timedelta, timezone
from unittest.mock import MagicMock

import jwt
import pytest
from ai_gateway_core.exceptions import AuthError

# 测试配置
TEST_JWT_SECRET = "test-secret-key-for-testing"
TEST_JWT_ALGORITHM = "HS256"


def create_test_token(
    user_id: str,
    tier: str = "normal",
    tenant_id: str = "",
    roles: list = None,
    expires_delta: timedelta = None,
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

    return jwt.encode(payload, TEST_JWT_SECRET, algorithm=TEST_JWT_ALGORITHM)


def create_expired_token(user_id: str) -> str:
    """创建过期的 Token"""
    return create_test_token(
        user_id=user_id,
        expires_delta=timedelta(hours=-1),  # 已过期
    )


# ============ 鉴权测试 ============


class TestAuthentication:
    """鉴权测试"""

    @pytest.fixture
    def mock_settings(self):
        """模拟配置"""
        from src.config.settings import AuthenticationSettings, AuthJWTSettings, Settings

        settings = Settings()
        settings.authentication = AuthenticationSettings(
            jwt=AuthJWTSettings(
                enabled=True,
                secret=TEST_JWT_SECRET,
                algorithms=[TEST_JWT_ALGORITHM],
            )
        )
        return settings

    @pytest.mark.asyncio
    async def test_valid_token_accepted(self, mock_settings):
        """有效 token 可以访问"""
        token = create_test_token(user_id="user_123")

        # 验证 token 可以被解析
        from src.core.auth.jwt import decode_jwt_token

        payload = decode_jwt_token(
            token,
            secret=TEST_JWT_SECRET,
            algorithms=[TEST_JWT_ALGORITHM],
        )

        assert payload["sub"] == "user_123"
        assert payload["tier"] == "normal"

    @pytest.mark.asyncio
    async def test_invalid_token_rejected(self):
        """无效 token 被拒绝"""
        from src.core.auth.jwt import decode_jwt_token

        with pytest.raises(AuthError):
            decode_jwt_token(
                "invalid_token",
                secret=TEST_JWT_SECRET,
                algorithms=[TEST_JWT_ALGORITHM],
            )

    @pytest.mark.asyncio
    async def test_expired_token_rejected(self):
        """过期 token 被拒绝"""
        from src.core.auth.jwt import decode_jwt_token

        expired_token = create_expired_token(user_id="user_123")

        with pytest.raises(AuthError):
            decode_jwt_token(
                expired_token,
                secret=TEST_JWT_SECRET,
                algorithms=[TEST_JWT_ALGORITHM],
            )


# ============ 游客会话测试 ============


class TestGuestSession:
    """游客会话测试"""

    @pytest.mark.asyncio
    async def test_create_guest_session(self):
        """创建游客会话"""
        from src.services.session.guest_session_manager import (
            GuestSessionConfig,
            GuestSessionManager,
        )

        manager = GuestSessionManager(
            config=GuestSessionConfig(),
            redis_client=None,  # 使用内存存储
        )

        session = await manager.create_session()

        assert session.session_id.startswith("guest_")
        assert session.created_at is not None

    @pytest.mark.asyncio
    async def test_validate_guest_session(self):
        """验证游客会话"""
        from src.services.session.guest_session_manager import (
            GuestSessionConfig,
            GuestSessionManager,
        )

        manager = GuestSessionManager(
            config=GuestSessionConfig(),
            redis_client=None,
        )

        session = await manager.create_session()

        # 验证有效会话
        is_valid = await manager.validate_session(session.session_id)
        assert is_valid is True

        # 验证无效会话
        is_valid = await manager.validate_session("invalid_session_id")
        assert is_valid is False

    @pytest.mark.asyncio
    async def test_guest_session_thread_quota(self):
        """游客会话对话配额"""
        from src.services.session.guest_session_manager import (
            GuestSessionConfig,
            GuestSessionManager,
        )

        config = GuestSessionConfig(max_threads_per_session=2)
        manager = GuestSessionManager(config=config, redis_client=None)

        session = await manager.create_session()

        # 添加第 1 个 thread
        result = await manager.add_thread_to_session(session.session_id, "thread_1")
        assert result is True

        # 添加第 2 个 thread
        result = await manager.add_thread_to_session(session.session_id, "thread_2")
        assert result is True

        # 添加第 3 个 thread（超出配额）
        result = await manager.add_thread_to_session(session.session_id, "thread_3")
        assert result is False

    @pytest.mark.asyncio
    async def test_guest_to_user_conversion(self):
        """游客转正式用户"""
        from src.services.session.guest_session_manager import (
            GuestSessionConfig,
            GuestSessionManager,
        )

        manager = GuestSessionManager(
            config=GuestSessionConfig(),
            redis_client=None,
        )

        session = await manager.create_session()
        await manager.add_thread_to_session(session.session_id, "thread_1")
        await manager.add_thread_to_session(session.session_id, "thread_2")

        # 转换为正式用户
        result = await manager.convert_to_user(
            session_id=session.session_id,
            user_id="new_user_123",
        )

        assert result["success"] is True
        assert result["user_id"] == "new_user_123"
        assert len(result["migrated_threads"]) == 2

        # 会话应该已被删除
        is_valid = await manager.validate_session(session.session_id)
        assert is_valid is False


# ============ 限流测试 ============


class TestRateLimit:
    """限流测试"""

    @pytest.mark.asyncio
    async def test_sliding_window_rate_limit(self):
        """滑动窗口限流"""
        from src.core.middleware.rate_limit_http import SlidingWindowRateLimiter

        limiter = SlidingWindowRateLimiter(redis_client=None)

        # 配置：2 个请求/秒
        limit = 2
        window = 1

        # 第 1 个请求应该通过
        result = await limiter.check("test_key", limit, window)
        assert result.allowed is True

        # 第 2 个请求应该通过
        result = await limiter.check("test_key", limit, window)
        assert result.allowed is True

        # 第 3 个请求应该被限流
        result = await limiter.check("test_key", limit, window)
        assert result.allowed is False

        # 等待窗口过期后应该可以继续
        await asyncio.sleep(1.1)
        result = await limiter.check("test_key", limit, window)
        assert result.allowed is True

    @pytest.mark.asyncio
    async def test_multi_dimension_rate_limit(self):
        """多维度限流"""
        from src.core.gateway.multi_dimension_rate_limiter import (
            MultiDimensionRateLimitConfig,
            MultiDimensionRateLimiter,
            RateLimitContext,
            TierLimit,
        )

        config = MultiDimensionRateLimitConfig(
            global_enabled=False,  # 禁用全局限流以便测试
            ip_enabled=True,
            ip_limit=2,
            ip_window=1,
            user_tier_limits={
                "anonymous": TierLimit(requests=1, window=1),
                "normal": TierLimit(requests=5, window=1),
            },
        )

        limiter = MultiDimensionRateLimiter(config=config, redis_client=None)

        # 匿名用户限流测试
        context = RateLimitContext(
            ip="192.168.1.1",
            user_id="anon:test",
            user_tier="anonymous",
        )

        # 第 1 个请求通过
        result = await limiter.check(context)
        assert result.allowed is True

        # 第 2 个请求被限流（匿名用户只能 1 req/s）
        result = await limiter.check(context)
        assert result.allowed is False


# ============ 用户隔离测试 ============


class TestUserIsolation:
    """用户隔离测试"""

    @pytest.mark.asyncio
    async def test_thread_ownership_verification(self):
        """Thread 所有权验证"""
        from src.adapters.langgraph_proxy import ForbiddenError, LangGraphProxy
        from src.core.auth.user_resolver import UserContext

        # 模拟 Thread 数据
        thread = {
            "thread_id": "test_thread_123",
            "metadata": {
                "owner_id": "user_a",
                "tenant_id": "tenant_1",
            },
        }

        proxy = LangGraphProxy.__new__(LangGraphProxy)

        # 所有者可以访问
        user_a = UserContext(
            user_id="user_a",
            tenant_id="tenant_1",
            tier="normal",
            is_authenticated=True,
        )
        proxy._verify_ownership(thread, user_a)  # 不应该抛出异常

        # 其他用户不能访问
        user_b = UserContext(
            user_id="user_b",
            tenant_id="tenant_1",
            tier="normal",
            is_authenticated=True,
        )

        with pytest.raises(ForbiddenError):
            proxy._verify_ownership(thread, user_b)

    @pytest.mark.asyncio
    async def test_admin_can_access_all(self):
        """管理员可以访问所有 Thread"""
        from src.adapters.langgraph_proxy import LangGraphProxy
        from src.core.auth.user_resolver import UserContext

        thread = {
            "thread_id": "test_thread_123",
            "metadata": {
                "owner_id": "user_a",
                "tenant_id": "tenant_1",
            },
        }

        proxy = LangGraphProxy.__new__(LangGraphProxy)

        # 管理员可以访问任何 Thread
        admin = UserContext(
            user_id="admin_user",
            tenant_id="",
            tier="admin",
            is_authenticated=True,
        )
        proxy._verify_ownership(thread, admin)  # 不应该抛出异常

    @pytest.mark.asyncio
    async def test_enterprise_tenant_access(self):
        """企业版同租户访问"""
        from src.adapters.langgraph_proxy import LangGraphProxy
        from src.core.auth.user_resolver import UserContext

        thread = {
            "thread_id": "test_thread_123",
            "metadata": {
                "owner_id": "user_a",
                "tenant_id": "tenant_1",
            },
        }

        proxy = LangGraphProxy.__new__(LangGraphProxy)

        # 同租户企业用户可以访问
        enterprise_user = UserContext(
            user_id="user_b",
            tenant_id="tenant_1",
            tier="enterprise",
            is_authenticated=True,
        )
        proxy._verify_ownership(thread, enterprise_user)  # 不应该抛出异常


# ============ Header 注入测试 ============


class TestHeaderInjection:
    """Header 注入测试"""

    def test_build_langgraph_headers(self):
        """构建 LangGraph 请求头"""
        from src.adapters.langgraph_proxy import LangGraphProxy
        from src.core.auth.user_resolver import UserContext

        proxy = LangGraphProxy.__new__(LangGraphProxy)
        proxy.auth_token = None  # Required for _build_langgraph_headers

        user = UserContext(
            user_id="user_123",
            tenant_id="tenant_1",
            tier="premium",
            is_authenticated=True,
        )

        headers = proxy._build_langgraph_headers(user)

        assert headers["X-User-Id"] == "user_123"
        assert headers["X-User-Type"] == "user"
        assert headers["X-Tenant-Id"] == "tenant_1"
        assert headers["X-User-Tier"] == "premium"
        assert headers["Content-Type"] == "application/json"

    def test_build_headers_for_guest(self):
        """为游客构建请求头"""
        from src.adapters.langgraph_proxy import LangGraphProxy
        from src.core.auth.user_resolver import UserContext

        proxy = LangGraphProxy.__new__(LangGraphProxy)
        proxy.auth_token = None  # Required for _build_langgraph_headers

        guest = UserContext(
            user_id="guest_abc123",
            tenant_id="public",
            tier="anonymous",
            is_authenticated=False,
        )

        headers = proxy._build_langgraph_headers(guest)

        assert headers["X-User-Id"] == "guest_abc123"
        assert headers["X-User-Type"] == "guest"
        assert headers["X-User-Tier"] == "anonymous"


# ============ Run 配置注入测试 ============


class TestRunConfigInjection:
    """Run 配置注入测试"""

    def test_build_run_config(self):
        """构建 Run 配置"""
        from src.adapters.langgraph_proxy import LangGraphProxy
        from src.core.auth.user_resolver import UserContext

        proxy = LangGraphProxy.__new__(LangGraphProxy)

        user = UserContext(
            user_id="user_123",
            tenant_id="tenant_1",
            tier="premium",
            is_authenticated=True,
        )

        config = proxy._build_run_config(user)

        assert config["configurable"]["user_id"] == "user_123"
        assert config["configurable"]["user_tier"] == "premium"
        assert config["configurable"]["tenant_id"] == "tenant_1"
        assert config["configurable"]["checkpoint_ns"] == "tenant_1"

    def test_build_run_config_with_existing_config(self):
        """使用现有配置构建 Run 配置"""
        from src.adapters.langgraph_proxy import LangGraphProxy
        from src.core.auth.user_resolver import UserContext

        proxy = LangGraphProxy.__new__(LangGraphProxy)

        user = UserContext(
            user_id="user_123",
            tenant_id="tenant_1",
            tier="normal",
            is_authenticated=True,
        )

        existing_config = {
            "configurable": {
                "custom_key": "custom_value",
            },
        }

        config = proxy._build_run_config(user, existing_config)

        # 保留原有配置
        assert config["configurable"]["custom_key"] == "custom_value"
        # 注入用户信息
        assert config["configurable"]["user_id"] == "user_123"


# ============ 运行测试 ============

if __name__ == "__main__":
    pytest.main([__file__, "-v"])
