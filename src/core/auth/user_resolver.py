"""
用户识别与分层

支持：
- JWT Token 认证
- API Key 认证
- 匿名用户（基于 IP）

``UserContext`` itself is declared in ``ai_gateway_core.auth`` so the
internal services can build identical records without a Gateway import.
Re-exported below so existing ``from src.core.auth.user_resolver import
UserContext`` call sites keep working.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import jwt
from ai_gateway_core.auth._core import UserContext  # noqa: F401 — re-export
from fastapi import Request

from ..client_ip import get_client_ip_from_request


@dataclass
class UserResolverConfig:
    """用户识别配置"""

    # JWT 配置
    jwt_enabled: bool = False
    jwt_secret: str = ""
    jwt_algorithms: list[str] = None
    jwt_audience: str | None = None
    jwt_issuer: str | None = None

    # API Key 配置
    api_key_enabled: bool = False
    api_key_header: str = "X-API-Key"
    api_keys: dict[str, dict[str, Any]] = None  # key -> {user_id, tenant_id, tier, roles}

    # 用户层级配置
    tier_claim: str = "tier"
    tenant_claim: str = "tenant_id"

    def __post_init__(self):
        if self.jwt_algorithms is None:
            self.jwt_algorithms = ["HS256"]
        if self.api_keys is None:
            self.api_keys = {}


class UserResolver:
    """用户识别与分层"""

    def __init__(self, config: UserResolverConfig):
        self.config = config

    async def resolve(self, request: Request) -> UserContext:
        """
        从请求中识别用户
        按优先级尝试：JWT Token -> API Key -> 匿名用户
        """
        client_ip = self._get_client_ip(request)

        # 1. 尝试从 JWT Token 获取
        if self.config.jwt_enabled:
            user = await self._resolve_from_jwt(request)
            if user:
                user.ip = client_ip
                return user

        # 2. 尝试从 API Key 获取
        if self.config.api_key_enabled:
            user = await self._resolve_from_api_key(request)
            if user:
                user.ip = client_ip
                return user

        # 3. 匿名用户（基于 IP）
        return UserContext(
            user_id=f"anon:{client_ip}",
            tenant_id="public",
            tier="anonymous",
            is_authenticated=False,
            ip=client_ip,
            roles=["guest"],
        )

    async def _resolve_from_jwt(self, request: Request) -> UserContext | None:
        """从 JWT Token 解析用户"""
        token = self._extract_bearer_token(request)
        if not token:
            return None

        try:
            payload = jwt.decode(
                token,
                self.config.jwt_secret,
                algorithms=self.config.jwt_algorithms,
                audience=self.config.jwt_audience,
                issuer=self.config.jwt_issuer,
            )

            # 提取用户信息
            user_id = payload.get("sub") or payload.get("user_id") or ""
            if not user_id:
                return None

            # 验证 token 是否在 Redis 中有效（如果 Redis 可用）
            token_id = payload.get("jti")
            if token_id:
                redis = getattr(request.app.state, "redis", None)
                if redis and getattr(redis, "enabled", False):
                    is_valid = await redis.validate_token(token_id)
                    if not is_valid:
                        # Token 已被撤销
                        return None

            tenant_id = payload.get(self.config.tenant_claim, "")
            tier = payload.get(self.config.tier_claim, "normal")
            roles = payload.get("roles", [])

            if isinstance(roles, str):
                roles = [roles]

            # Optional claims — absent in some JWTs, so default to safe values.
            # These populate the X-User-Type / X-User-Email / X-User-Name
            # headers forwarded to authenticated internal services.
            user_type = payload.get("user_type", "user")
            email = payload.get("email", "")
            name = payload.get("name", "")

            return UserContext(
                user_id=user_id,
                tenant_id=tenant_id,
                tier=tier,
                is_authenticated=True,
                roles=roles,
                user_type=user_type,
                email=email,
                name=name,
            )
        except jwt.PyJWTError:
            return None

    async def _resolve_from_api_key(self, request: Request) -> UserContext | None:
        """从 API Key 解析用户"""
        api_key = request.headers.get(self.config.api_key_header)
        if not api_key:
            return None

        key_info = self.config.api_keys.get(api_key)
        if not key_info:
            return None

        return UserContext(
            user_id=key_info.get("user_id", ""),
            tenant_id=key_info.get("tenant_id", ""),
            tier=key_info.get("tier", "normal"),
            is_authenticated=True,
            roles=key_info.get("roles", []),
        )

    def _extract_bearer_token(self, request: Request) -> str | None:
        """从 Authorization header 提取 Bearer token"""
        auth_header = request.headers.get("Authorization", "")
        if auth_header.startswith("Bearer "):
            return auth_header[7:]
        return None

    def _get_client_ip(self, request: Request) -> str:
        """获取真实客户端 IP（考虑代理）"""
        return get_client_ip_from_request(request)


# ============ 匿名用户记忆策略 ============


class AnonymousMemoryPolicy:
    """匿名用户记忆策略"""

    # 匿名用户限制
    MAX_THREADS_PER_SESSION = 1
    THREAD_TTL = 3600 * 24  # 24小时过期
    MEMORY_ENABLED = False  # 禁用跨会话记忆

    def __init__(self, redis_client=None):
        self.redis = redis_client

    async def get_or_create_thread(
        self,
        session_id: str,
        langgraph_proxy,
        user: UserContext,
    ) -> dict[str, Any]:
        """为匿名用户获取或创建临时 Thread"""

        # 检查是否已有 Thread
        if self.redis:
            existing_thread_id = await self.redis.get(f"anonymous:session:{session_id}:thread")
            if existing_thread_id:
                try:
                    return await langgraph_proxy.get_thread(user, existing_thread_id.decode())
                except Exception:
                    pass

        # 创建新 Thread，添加过期标记
        from datetime import datetime, timedelta

        thread = await langgraph_proxy.create_thread(
            user=user,
            metadata={
                "type": "anonymous",
                "session_id": session_id,
                "expires_at": (datetime.utcnow() + timedelta(seconds=self.THREAD_TTL)).isoformat(),
            },
        )

        # 记录映射
        if self.redis:
            await self.redis.setex(
                f"anonymous:session:{session_id}:thread", self.THREAD_TTL, thread["thread_id"]
            )

        return thread

    async def cleanup_expired_threads(self, _langgraph_proxy) -> int:
        """
        定时清理过期的匿名 Thread
        返回清理的数量
        """
        # 此方法应该由后台任务调用
        # 实际实现需要遍历 LangGraph 的 threads 并检查 expires_at
        return 0
