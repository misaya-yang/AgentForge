from __future__ import annotations

import json
import logging
from datetime import date, datetime
from enum import Enum
from typing import Any

try:
    import redis.asyncio as aioredis

    HAS_REDIS = True
except ImportError:
    HAS_REDIS = False
    aioredis = None

logger = logging.getLogger(__name__)
_warned_degraded = False


class RedisStorage:
    """Redis 缓存存储（可选启用）"""

    def __init__(self, url: str | None = None, enabled: bool = False):
        self.url = url
        self.enabled = enabled and HAS_REDIS and url
        self._client: Any | None = None

    async def connect(self) -> None:
        if not self.enabled:
            return
        if not HAS_REDIS:
            raise RuntimeError("redis is not installed. Run: pip install redis")
        self._client = await aioredis.from_url(
            self.url,
            encoding="utf-8",
            decode_responses=True,
        )

    def get_native_client(self) -> Any | None:
        """
        Return the underlying native redis client for advanced use cases.

        This is needed by components like MultiDimensionRateLimiter that
        require direct access to the redis client rather than the wrapper.

        Returns:
            The native redis.asyncio client, or None if not connected/enabled.
        """
        return self._client if self.enabled else None

    async def close(self) -> None:
        if self._client:
            await self._client.close()
            self._client = None

    async def ping(self) -> bool:
        """检查 Redis 连接"""
        if not self._client:
            return False
        try:
            return await self._client.ping()
        except Exception:
            return False

    # ===== 基础 KV 操作 =====

    @staticmethod
    def _json_default(obj: Any) -> Any:
        if isinstance(obj, (datetime, date)):
            return obj.isoformat()
        if isinstance(obj, Enum):
            return obj.value
        return str(obj)

    async def save(self, key: str, value: Any, ttl: int | None = None) -> None:
        """保存键值对"""
        if not self._client:
            return
        if isinstance(value, (dict, list)):
            value = json.dumps(value, ensure_ascii=False, default=self._json_default)
        if ttl:
            await self._client.setex(key, ttl, value)
        else:
            await self._client.set(key, value)

    async def get(self, key: str) -> Any | None:
        """获取值"""
        if not self._client:
            return None
        value = await self._client.get(key)
        if value:
            try:
                return json.loads(value)
            except (json.JSONDecodeError, TypeError):
                return value
        return None

    async def getdel(self, key: str) -> Any | None:
        """Atomically consume a value (Redis >= 6.2); never emulate GET + DELETE."""
        if not self._client:
            return None
        value = await self._client.getdel(key)
        if value:
            try:
                return json.loads(value)
            except (json.JSONDecodeError, TypeError):
                return value
        return None

    async def delete(self, key: str) -> bool:
        """删除键"""
        if not self._client:
            return False
        result = await self._client.delete(key)
        return result > 0

    async def exists(self, key: str, degraded_default: bool = False) -> bool:
        """
        检查键是否存在

        Args:
            key: Redis 键名
            degraded_default: 当 Redis 未连接时返回的默认值
                             对于 token 验证场景应设为 True（降级模式允许通过）

        Returns:
            键是否存在，或 Redis 未连接时返回 degraded_default
        """
        if not self._client:
            if degraded_default:
                global _warned_degraded
                if not _warned_degraded:
                    logger.warning(
                        "Redis not connected, degraded mode: assuming token exists "
                        "(first warning only)"
                    )
                    _warned_degraded = True
            return degraded_default
        try:
            return await self._client.exists(key) > 0
        except Exception as e:
            logger.error(f"Redis exists check failed for key '{key}': {e}")
            return degraded_default

    async def incr(self, key: str) -> int:
        """增加计数器"""
        if not self._client:
            return 0
        return await self._client.incr(key)

    async def decr(self, key: str) -> int:
        """减少计数器"""
        if not self._client:
            return 0
        return await self._client.decr(key)

    # ===== 限流相关 =====

    async def incr_rate_limit(self, key: str, window: int) -> int:
        """增加限流计数器"""
        if not self._client:
            return 0
        pipe = self._client.pipeline()
        pipe.incr(key)
        pipe.expire(key, window)
        results = await pipe.execute()
        return results[0]

    async def get_rate_limit_count(self, key: str) -> int:
        """获取当前计数"""
        if not self._client:
            return 0
        count = await self._client.get(key)
        return int(count) if count else 0

    # ===== 会话存储 =====

    async def save_session(self, session_id: str, data: dict[str, Any], ttl: int = 3600) -> None:
        """保存会话数据"""
        key = f"session:{session_id}"
        await self.save(key, data, ttl)

    async def get_session(self, session_id: str) -> dict[str, Any] | None:
        """获取会话数据"""
        key = f"session:{session_id}"
        return await self.get(key)

    async def delete_session(self, session_id: str) -> bool:
        """删除会话"""
        key = f"session:{session_id}"
        return await self.delete(key)

    # ===== 语义缓存 =====

    async def cache_response(
        self,
        service_id: str,
        input_hash: str,
        response: str,
        ttl: int = 3600,
    ) -> None:
        """缓存响应"""
        key = f"cache:{service_id}:{input_hash}"
        await self.save(key, {"response": response}, ttl)

    async def get_cached_response(self, service_id: str, input_hash: str) -> str | None:
        """获取缓存的响应"""
        key = f"cache:{service_id}:{input_hash}"
        data = await self.get(key)
        if data and isinstance(data, dict):
            return data.get("response")
        return None

    # ===== 服务健康状态缓存 =====

    async def set_service_health(
        self, service_id: str, status: dict[str, Any], ttl: int = 60
    ) -> None:
        """缓存服务健康状态"""
        key = f"health:{service_id}"
        await self.save(key, status, ttl)

    async def get_service_health(self, service_id: str) -> dict[str, Any] | None:
        """获取服务健康状态"""
        key = f"health:{service_id}"
        return await self.get(key)

    # ===== 配置缓存 =====

    async def cache_config(self, config_type: str, config: dict[str, Any], ttl: int = 300) -> None:
        """缓存配置"""
        key = f"config:{config_type}"
        await self.save(key, config, ttl)

    async def get_cached_config(self, config_type: str) -> dict[str, Any] | None:
        """获取缓存的配置"""
        key = f"config:{config_type}"
        return await self.get(key)

    async def invalidate_config(self, config_type: str) -> None:
        """使配置缓存失效"""
        key = f"config:{config_type}"
        await self.delete(key)

    # ===== LangGraph 专用缓存 =====

    async def get_thread_mapping(self, session_id: str) -> str | None:
        """
        获取 session_id -> thread_id 映射

        用于 LangGraph 的会话管理，将网关的 session_id 映射到 LangGraph 的 thread_id
        """
        key = f"lg:thread_map:{session_id}"
        result = await self.get(key)
        return str(result) if result else None

    async def set_thread_mapping(self, session_id: str, thread_id: str, ttl: int = 604800) -> None:
        """
        设置 session_id -> thread_id 映射

        Args:
            session_id: 网关会话 ID
            thread_id: LangGraph 线程 ID (UUID 格式)
            ttl: 过期时间，默认 7 天 (604800 秒)
        """
        key = f"lg:thread_map:{session_id}"
        await self.save(key, thread_id, ttl)

    async def cache_thread(self, thread_id: str, data: dict[str, Any], ttl: int = 60) -> None:
        """
        缓存 Thread 元数据

        Args:
            thread_id: LangGraph 线程 ID
            data: Thread 元数据（包含 metadata、created_at 等）
            ttl: 过期时间，默认 60 秒
        """
        key = f"lg:thread:{thread_id}"
        await self.save(key, data, ttl)

    async def get_cached_thread(self, thread_id: str) -> dict[str, Any] | None:
        """获取缓存的 Thread 元数据"""
        key = f"lg:thread:{thread_id}"
        return await self.get(key)

    async def invalidate_thread(self, thread_id: str) -> None:
        """使 Thread 缓存失效"""
        key = f"lg:thread:{thread_id}"
        await self.delete(key)

    async def cache_assistant(
        self, assistant_id: str, data: dict[str, Any], ttl: int = 300
    ) -> None:
        """
        缓存 Assistant 信息

        Args:
            assistant_id: LangGraph 助手 ID
            data: Assistant 配置信息
            ttl: 过期时间，默认 300 秒 (5 分钟)
        """
        key = f"lg:assistant:{assistant_id}"
        await self.save(key, data, ttl)

    async def get_cached_assistant(self, assistant_id: str) -> dict[str, Any] | None:
        """获取缓存的 Assistant 信息"""
        key = f"lg:assistant:{assistant_id}"
        return await self.get(key)

    async def cache_assistants_list(
        self, user_id: str, data: list[dict[str, Any]], ttl: int = 60
    ) -> None:
        """
        缓存用户的 Assistants 列表

        Args:
            user_id: 用户 ID
            data: Assistants 列表
            ttl: 过期时间，默认 60 秒
        """
        key = f"lg:assistants_list:{user_id}"
        await self.save(key, data, ttl)

    async def get_cached_assistants_list(self, user_id: str) -> list[dict[str, Any]] | None:
        """获取缓存的 Assistants 列表"""
        key = f"lg:assistants_list:{user_id}"
        result = await self.get(key)
        if isinstance(result, list):
            return result
        return None

    async def incr_user_thread_count(self, user_id: str) -> int:
        """
        增加用户 Thread 计数

        用于配额管理，跟踪用户创建的 Thread 数量
        """
        key = f"lg:quota:{user_id}:threads"
        return await self.incr(key)

    async def decr_user_thread_count(self, user_id: str) -> int:
        """减少用户 Thread 计数（删除 Thread 时调用）"""
        key = f"lg:quota:{user_id}:threads"
        return await self.decr(key)

    async def get_user_thread_count(self, user_id: str) -> int:
        """获取用户 Thread 计数"""
        key = f"lg:quota:{user_id}:threads"
        count = await self.get(key)
        return int(count) if count else 0

    async def incr_user_run_count(self, user_id: str, window: int = 3600) -> int:
        """
        增加用户 Run 计数（带滑动窗口）

        用于限流，跟踪用户在时间窗口内的 Run 调用次数

        Args:
            user_id: 用户 ID
            window: 时间窗口（秒），默认 1 小时
        """
        key = f"lg:quota:{user_id}:runs:{window}"
        return await self.incr_rate_limit(key, window)

    async def get_user_run_count(self, user_id: str, window: int = 3600) -> int:
        """获取用户 Run 计数"""
        key = f"lg:quota:{user_id}:runs:{window}"
        return await self.get_rate_limit_count(key)

    # ===== Token 存储 =====

    async def save_token(
        self,
        token_id: str,
        user_id: str,
        data: dict[str, Any],
        ttl: int = 10800,  # 默认 3 小时
    ) -> None:
        """
        保存 JWT Token 到 Redis

        Args:
            token_id: Token 的唯一标识（可以是 jti 或 token 的 hash）
            user_id: 用户 ID
            data: Token 关联的数据（用户信息、权限等）
            ttl: 过期时间（秒），默认 3 小时
        """
        # 保存 token 数据
        token_key = f"auth:token:{token_id}"
        await self.save(token_key, data, ttl)

        # 维护用户的 token 列表（用于登出所有设备）
        user_tokens_key = f"auth:user_tokens:{user_id}"
        if self._client:
            await self._client.sadd(user_tokens_key, token_id)
            await self._client.expire(user_tokens_key, ttl)

    async def get_token(self, token_id: str) -> dict[str, Any] | None:
        """
        获取 Token 数据

        Args:
            token_id: Token 的唯一标识

        Returns:
            Token 数据，如果不存在或已过期返回 None
        """
        key = f"auth:token:{token_id}"
        return await self.get(key)

    def is_connected(self) -> bool:
        """检查 Redis 客户端是否已连接"""
        return self._client is not None

    async def validate_token(self, token_id: str) -> bool:
        """
        验证 Token 是否有效（存在于 Redis 中）

        当 Redis 未连接时，采用降级模式，假设 token 有效。
        这确保了系统在 Redis 故障时仍能正常工作（无状态 JWT 验证）。

        Args:
            token_id: Token 的唯一标识

        Returns:
            True 如果 Token 有效或 Redis 未连接（降级模式），否则 False
        """
        key = f"auth:token:{token_id}"
        # 使用降级模式：Redis 未连接时假设 token 有效
        return await self.exists(key, degraded_default=True)

    async def revoke_token(self, token_id: str, user_id: str) -> bool:
        """
        撤销单个 Token（登出）

        Args:
            token_id: Token 的唯一标识
            user_id: 用户 ID

        Returns:
            True 如果成功撤销
        """
        token_key = f"auth:token:{token_id}"
        user_tokens_key = f"auth:user_tokens:{user_id}"

        result = await self.delete(token_key)

        if self._client:
            await self._client.srem(user_tokens_key, token_id)

        return result

    async def revoke_all_user_tokens(self, user_id: str) -> int:
        """
        撤销用户所有 Token（登出所有设备）

        Args:
            user_id: 用户 ID

        Returns:
            撤销的 Token 数量
        """
        if not self._client:
            return 0

        user_tokens_key = f"auth:user_tokens:{user_id}"
        token_ids = await self._client.smembers(user_tokens_key)

        count = 0
        for token_id in token_ids:
            token_key = f"auth:token:{token_id}"
            if await self.delete(token_key):
                count += 1

        await self.delete(user_tokens_key)
        return count

    async def refresh_token_ttl(self, token_id: str, ttl: int = 10800) -> bool:
        """
        刷新 Token 的过期时间

        Args:
            token_id: Token 的唯一标识
            ttl: 新的过期时间（秒）

        Returns:
            True 如果成功刷新
        """
        if not self._client:
            return False

        key = f"auth:token:{token_id}"
        return await self._client.expire(key, ttl)
