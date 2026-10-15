"""
限流测试

测试内容：
- 全局限流
- 租户限流
- 用户限流
- 服务限流
- IP 限流
- 滑动窗口算法
- 令牌桶 burst
- 限流恢复
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from unittest.mock import MagicMock

import pytest

from ai_gateway_core.exceptions import RateLimitExceededError
from src.core.gateway.rate_limiter import (
    RateLimit,
    RateLimitConfig,
    RateLimiter,
)

# ============ RateLimit Config Tests ============


class TestRateLimitConfig:
    """RateLimit 配置测试"""

    def test_default_config(self):
        """测试默认配置"""
        config = RateLimitConfig()

        assert config.global_limit is None
        assert config.tenant_limits is None
        assert config.user_limits is None
        assert config.service_limits is None
        assert config.ip_limits is None

    def test_rate_limit_creation(self):
        """测试 RateLimit 创建"""
        limit = RateLimit(
            requests=100,
            window=60,
            burst=10,
            strategy="sliding_window",
        )

        assert limit.requests == 100
        assert limit.window == 60
        assert limit.burst == 10
        assert limit.strategy == "sliding_window"

    def test_rate_limit_default_values(self):
        """测试 RateLimit 默认值"""
        limit = RateLimit(requests=100, window=60)

        assert limit.burst == 0
        assert limit.strategy == "sliding_window"

    def test_config_with_all_limits(self):
        """测试包含所有限制的配置"""
        config = RateLimitConfig(
            global_limit=RateLimit(requests=1000, window=60),
            tenant_limits={
                "tenant_001": RateLimit(requests=500, window=60),
                "tenant_002": RateLimit(requests=200, window=60),
            },
            user_limits={
                "user_001": RateLimit(requests=100, window=60),
            },
            service_limits={
                "langgraph": RateLimit(requests=50, window=60),
            },
            ip_limits=RateLimit(requests=10, window=60),
        )

        assert config.global_limit.requests == 1000
        assert config.tenant_limits["tenant_001"].requests == 500
        assert config.user_limits["user_001"].requests == 100
        assert config.service_limits["langgraph"].requests == 50
        assert config.ip_limits.requests == 10


# ============ Mock Request and Service ============


@dataclass
class MockRequest:
    """Mock 请求"""

    user_id: str = ""
    tenant_id: str = ""


@dataclass
class MockServiceConfig:
    """Mock 服务配置"""

    rate_limit: MagicMock = None

    def __post_init__(self):
        if self.rate_limit is None:
            self.rate_limit = MagicMock()
            self.rate_limit.enabled = False


@dataclass
class MockService:
    """Mock 服务定义"""

    service_id: str = "test_service"
    rate_limit: dict | None = None
    _config: MockServiceConfig = None

    def __post_init__(self):
        if self._config is None:
            self._config = MockServiceConfig()

    def get_service_config(self):
        return self._config


# ============ RateLimiter Core Tests ============


class TestRateLimiter:
    """RateLimiter 核心测试"""

    @pytest.fixture
    def basic_config(self):
        """基础配置"""
        return RateLimitConfig(
            global_limit=RateLimit(requests=10, window=60),
        )

    @pytest.fixture
    def rate_limiter(self, basic_config):
        """RateLimiter 实例"""
        return RateLimiter(basic_config)

    @pytest.mark.asyncio
    async def test_first_request_allowed(self, rate_limiter):
        """测试第一个请求被允许"""
        request = MockRequest(user_id="user_001")
        service = MockService()

        # 第一个请求应该通过
        await rate_limiter.enforce(request, service)

    @pytest.mark.asyncio
    async def test_requests_within_limit_allowed(self, rate_limiter):
        """测试限制内的请求都被允许"""
        request = MockRequest(user_id="user_001")
        service = MockService()

        # 发送 10 个请求（等于限制）
        for _ in range(10):
            await rate_limiter.enforce(request, service)

    @pytest.mark.asyncio
    async def test_request_exceeding_limit_rejected(self, rate_limiter):
        """测试超过限制的请求被拒绝"""
        request = MockRequest(user_id="user_001")
        service = MockService()

        # 发送 10 个请求
        for _ in range(10):
            await rate_limiter.enforce(request, service)

        # 第 11 个请求应该被拒绝
        with pytest.raises(RateLimitExceededError):
            await rate_limiter.enforce(request, service)

    @pytest.mark.asyncio
    async def test_limit_resets_after_window(self, rate_limiter):
        """测试窗口过后限制重置"""
        # 使用短窗口的配置
        config = RateLimitConfig(
            global_limit=RateLimit(requests=2, window=1),  # 1 秒窗口
        )
        limiter = RateLimiter(config)

        request = MockRequest(user_id="user_001")
        service = MockService()

        # 发送 2 个请求
        await limiter.enforce(request, service)
        await limiter.enforce(request, service)

        # 等待窗口过期
        await asyncio.sleep(1.1)

        # 应该可以再次发送请求
        await limiter.enforce(request, service)


# ============ Tenant Rate Limiting Tests ============


class TestTenantRateLimiting:
    """租户限流测试"""

    @pytest.fixture
    def tenant_config(self):
        """租户配置"""
        return RateLimitConfig(
            tenant_limits={
                "tenant_001": RateLimit(requests=5, window=60),
                "tenant_002": RateLimit(requests=10, window=60),
            },
        )

    @pytest.fixture
    def tenant_limiter(self, tenant_config):
        """租户 RateLimiter"""
        return RateLimiter(tenant_config)

    @pytest.mark.asyncio
    async def test_different_tenants_have_separate_limits(self, tenant_limiter):
        """测试不同租户有独立限制"""
        request_1 = MockRequest(user_id="user_a", tenant_id="tenant_001")
        request_2 = MockRequest(user_id="user_b", tenant_id="tenant_002")
        service = MockService()

        # tenant_001 发送 5 个请求
        for _ in range(5):
            await tenant_limiter.enforce(request_1, service)

        # tenant_001 超限
        with pytest.raises(RateLimitExceededError):
            await tenant_limiter.enforce(request_1, service)

        # tenant_002 仍可发送
        await tenant_limiter.enforce(request_2, service)

    @pytest.mark.asyncio
    async def test_tenant_limit_shared_among_users(self, tenant_limiter):
        """测试同租户用户共享限制"""
        request_a = MockRequest(user_id="user_a", tenant_id="tenant_001")
        request_b = MockRequest(user_id="user_b", tenant_id="tenant_001")
        service = MockService()

        # user_a 发送 3 个请求
        for _ in range(3):
            await tenant_limiter.enforce(request_a, service)

        # user_b 发送 2 个请求（共 5 个，达到限制）
        for _ in range(2):
            await tenant_limiter.enforce(request_b, service)

        # 两个用户都超限
        with pytest.raises(RateLimitExceededError):
            await tenant_limiter.enforce(request_a, service)

        with pytest.raises(RateLimitExceededError):
            await tenant_limiter.enforce(request_b, service)

    @pytest.mark.asyncio
    async def test_unknown_tenant_no_limit(self, tenant_limiter):
        """测试未配置租户无限制"""
        request = MockRequest(user_id="user_c", tenant_id="unknown_tenant")
        service = MockService()

        # 未配置的租户没有限制，可以发送多个请求
        for _ in range(20):
            await tenant_limiter.enforce(request, service)


# ============ User Rate Limiting Tests ============


class TestUserRateLimiting:
    """用户限流测试"""

    @pytest.fixture
    def user_config(self):
        """用户配置"""
        return RateLimitConfig(
            user_limits={
                "user_001": RateLimit(requests=3, window=60),
                "user_002": RateLimit(requests=5, window=60),
            },
        )

    @pytest.fixture
    def user_limiter(self, user_config):
        """用户 RateLimiter"""
        return RateLimiter(user_config)

    @pytest.mark.asyncio
    async def test_user_specific_limit(self, user_limiter):
        """测试用户特定限制"""
        request = MockRequest(user_id="user_001")
        service = MockService()

        # 发送 3 个请求
        for _ in range(3):
            await user_limiter.enforce(request, service)

        # 第 4 个请求被拒绝
        with pytest.raises(RateLimitExceededError):
            await user_limiter.enforce(request, service)

    @pytest.mark.asyncio
    async def test_different_users_different_limits(self, user_limiter):
        """测试不同用户有不同限制"""
        request_1 = MockRequest(user_id="user_001")
        request_2 = MockRequest(user_id="user_002")
        service = MockService()

        # user_001 限制 3 次
        for _ in range(3):
            await user_limiter.enforce(request_1, service)

        with pytest.raises(RateLimitExceededError):
            await user_limiter.enforce(request_1, service)

        # user_002 限制 5 次
        for _ in range(5):
            await user_limiter.enforce(request_2, service)

        with pytest.raises(RateLimitExceededError):
            await user_limiter.enforce(request_2, service)


# ============ Service Rate Limiting Tests ============


class TestServiceRateLimiting:
    """服务限流测试"""

    @pytest.fixture
    def service_config(self):
        """服务配置"""
        return RateLimitConfig(
            service_limits={
                "langgraph": RateLimit(requests=5, window=60),
                "openai": RateLimit(requests=10, window=60),
            },
        )

    @pytest.fixture
    def service_limiter(self, service_config):
        """服务 RateLimiter"""
        return RateLimiter(service_config)

    @pytest.mark.asyncio
    async def test_service_limit_applies(self, service_limiter):
        """测试服务限制生效"""
        request = MockRequest(user_id="user_001")
        service = MockService(service_id="langgraph")

        # 发送 5 个请求
        for _ in range(5):
            await service_limiter.enforce(request, service)

        # 第 6 个请求被拒绝
        with pytest.raises(RateLimitExceededError):
            await service_limiter.enforce(request, service)

    @pytest.mark.asyncio
    async def test_different_services_separate_limits(self, service_limiter):
        """测试不同服务独立限制"""
        request = MockRequest(user_id="user_001")
        langgraph = MockService(service_id="langgraph")
        openai = MockService(service_id="openai")

        # langgraph 达到限制
        for _ in range(5):
            await service_limiter.enforce(request, langgraph)

        with pytest.raises(RateLimitExceededError):
            await service_limiter.enforce(request, langgraph)

        # openai 仍可使用
        await service_limiter.enforce(request, openai)


# ============ IP Rate Limiting Tests ============


class TestIPRateLimiting:
    """IP 限流测试"""

    @pytest.fixture
    def ip_config(self):
        """IP 配置"""
        return RateLimitConfig(
            ip_limits=RateLimit(requests=3, window=60),
        )

    @pytest.fixture
    def ip_limiter(self, ip_config):
        """IP RateLimiter"""
        return RateLimiter(ip_config)

    @pytest.mark.asyncio
    async def test_ip_limit_applies(self, ip_limiter):
        """测试 IP 限制生效"""
        request = MockRequest(user_id="user_001")
        service = MockService()

        # 发送 3 个请求
        for _ in range(3):
            await ip_limiter.enforce(request, service, client_ip="192.168.1.1")

        # 第 4 个请求被拒绝
        with pytest.raises(RateLimitExceededError):
            await ip_limiter.enforce(request, service, client_ip="192.168.1.1")

    @pytest.mark.asyncio
    async def test_different_ips_separate_limits(self, ip_limiter):
        """测试不同 IP 独立限制"""
        request = MockRequest(user_id="user_001")
        service = MockService()

        # IP 1 达到限制
        for _ in range(3):
            await ip_limiter.enforce(request, service, client_ip="192.168.1.1")

        with pytest.raises(RateLimitExceededError):
            await ip_limiter.enforce(request, service, client_ip="192.168.1.1")

        # IP 2 仍可发送
        await ip_limiter.enforce(request, service, client_ip="192.168.1.2")


# ============ Burst Tests ============


class TestBurstHandling:
    """Burst 处理测试"""

    @pytest.fixture
    def burst_config(self):
        """Burst 配置"""
        return RateLimitConfig(
            global_limit=RateLimit(requests=5, window=60, burst=3),
        )

    @pytest.fixture
    def burst_limiter(self, burst_config):
        """Burst RateLimiter"""
        return RateLimiter(burst_config)

    @pytest.mark.asyncio
    async def test_burst_allows_extra_requests(self, burst_limiter):
        """测试 burst 允许额外请求"""
        request = MockRequest(user_id="user_001")
        service = MockService()

        # 可以发送 5 + 3 = 8 个请求
        for _ in range(8):
            await burst_limiter.enforce(request, service)

    @pytest.mark.asyncio
    async def test_burst_eventually_limits(self, burst_limiter):
        """测试 burst 最终也会限制"""
        request = MockRequest(user_id="user_001")
        service = MockService()

        # 发送 8 个请求（5 + 3 burst）
        for _ in range(8):
            await burst_limiter.enforce(request, service)

        # 第 9 个请求被拒绝
        with pytest.raises(RateLimitExceededError):
            await burst_limiter.enforce(request, service)


# ============ Multi-Dimension Rate Limiting Tests ============


class TestMultiDimensionRateLimiting:
    """多维度限流测试"""

    @pytest.fixture
    def multi_config(self):
        """多维度配置"""
        return RateLimitConfig(
            global_limit=RateLimit(requests=100, window=60),
            tenant_limits={
                "tenant_001": RateLimit(requests=50, window=60),
            },
            user_limits={
                "user_001": RateLimit(requests=10, window=60),
            },
        )

    @pytest.fixture
    def multi_limiter(self, multi_config):
        """多维度 RateLimiter"""
        return RateLimiter(multi_config)

    @pytest.mark.asyncio
    async def test_strictest_limit_applies(self, multi_limiter):
        """测试最严格的限制生效"""
        request = MockRequest(user_id="user_001", tenant_id="tenant_001")
        service = MockService()

        # user_001 限制 10 次（最严格）
        for _ in range(10):
            await multi_limiter.enforce(request, service)

        # 第 11 次被拒绝（用户限制）
        with pytest.raises(RateLimitExceededError):
            await multi_limiter.enforce(request, service)

    @pytest.mark.asyncio
    async def test_all_dimensions_checked(self, multi_limiter):
        """测试所有维度都被检查"""
        # 创建多个用户发送请求
        service = MockService()

        # 发送超过租户限制的请求（跨用户）
        for i in range(60):
            request = MockRequest(
                user_id=f"user_{i:03d}",
                tenant_id="tenant_001",
            )
            if i < 50:
                await multi_limiter.enforce(request, service)
            else:
                # 超过租户限制
                with pytest.raises(RateLimitExceededError):
                    await multi_limiter.enforce(request, service)
                break


# ============ Sliding Window Tests ============


class TestSlidingWindow:
    """滑动窗口测试"""

    @pytest.mark.asyncio
    async def test_sliding_window_cleanup(self):
        """测试滑动窗口清理旧请求"""
        config = RateLimitConfig(
            global_limit=RateLimit(requests=2, window=1),  # 1 秒窗口
        )
        limiter = RateLimiter(config)

        request = MockRequest(user_id="user_001")
        service = MockService()

        # 发送第一个请求
        await limiter.enforce(request, service)

        # 等待 0.6 秒
        await asyncio.sleep(0.6)

        # 发送第二个请求
        await limiter.enforce(request, service)

        # 等待 0.6 秒（第一个请求已过期）
        await asyncio.sleep(0.6)

        # 应该可以发送新请求
        await limiter.enforce(request, service)


# ============ Concurrent Access Tests ============


class TestConcurrentAccess:
    """并发访问测试"""

    @pytest.mark.asyncio
    async def test_concurrent_requests_handled_safely(self):
        """测试并发请求安全处理"""
        config = RateLimitConfig(
            global_limit=RateLimit(requests=10, window=60),
        )
        limiter = RateLimiter(config)

        request = MockRequest(user_id="user_001")
        service = MockService()

        # 并发发送 10 个请求
        tasks = [limiter.enforce(request, service) for _ in range(10)]

        # 所有请求都应该成功
        await asyncio.gather(*tasks)

    @pytest.mark.asyncio
    async def test_concurrent_requests_respect_limit(self):
        """测试并发请求遵守限制"""
        config = RateLimitConfig(
            global_limit=RateLimit(requests=5, window=60),
        )
        limiter = RateLimiter(config)

        request = MockRequest(user_id="user_001")
        service = MockService()

        # 并发发送 10 个请求
        tasks = [limiter.enforce(request, service) for _ in range(10)]

        results = await asyncio.gather(*tasks, return_exceptions=True)

        # 应该有一些请求成功，一些失败
        successes = [r for r in results if r is None]
        failures = [r for r in results if isinstance(r, RateLimitExceededError)]

        assert len(successes) == 5
        assert len(failures) == 5


# ============ Error Message Tests ============


class TestRateLimitErrorMessages:
    """限流错误消息测试"""

    def test_error_contains_key(self):
        """测试错误包含限流键"""
        error = RateLimitExceededError("user:user_001")

        assert "user:user_001" in str(error)
        assert error.key == "user:user_001"

    def test_global_error_message(self):
        """测试全局限流错误消息"""
        error = RateLimitExceededError("global")

        assert "global" in str(error)

    def test_tenant_error_message(self):
        """测试租户限流错误消息"""
        error = RateLimitExceededError("tenant:tenant_001")

        assert "tenant:tenant_001" in str(error)


# ============ Service Config Rate Limit Tests ============


class TestServiceConfigRateLimit:
    """服务配置限流测试"""

    @pytest.mark.asyncio
    async def test_service_rate_limit_from_dict(self):
        """测试从字典读取服务限流配置"""
        config = RateLimitConfig()
        limiter = RateLimiter(config)

        request = MockRequest(user_id="user_001")
        service = MockService(
            service_id="test_service",
            rate_limit={
                "requests": 3,
                "window": 60,
                "burst": 0,
                "strategy": "sliding_window",
            },
        )

        # 发送 3 个请求
        for _ in range(3):
            await limiter.enforce(request, service)

        # 第 4 个请求被拒绝
        with pytest.raises(RateLimitExceededError):
            await limiter.enforce(request, service)

    @pytest.mark.asyncio
    async def test_service_config_rate_limit(self):
        """测试从服务配置读取限流"""
        config = RateLimitConfig()
        limiter = RateLimiter(config)

        request = MockRequest(user_id="user_001")

        # 创建带有 service_config 的服务
        service_config = MockServiceConfig()
        service_config.rate_limit = MagicMock()
        service_config.rate_limit.enabled = True
        service_config.rate_limit.requests = 2
        service_config.rate_limit.window = 60
        service_config.rate_limit.burst = 0
        service_config.rate_limit.strategy = "sliding_window"

        service = MockService(
            service_id="test_service_2",
            _config=service_config,
        )

        # 发送 2 个请求
        for _ in range(2):
            await limiter.enforce(request, service)

        # 第 3 个请求被拒绝
        with pytest.raises(RateLimitExceededError):
            await limiter.enforce(request, service)
