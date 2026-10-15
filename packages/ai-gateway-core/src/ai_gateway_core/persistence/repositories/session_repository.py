"""
会话仓库

提供会话数据的数据访问接口
"""

from __future__ import annotations

import builtins
import json
import logging
from abc import ABC, abstractmethod
from datetime import datetime
from typing import TYPE_CHECKING, Any

import asyncpg

from .base import BaseRepository

if TYPE_CHECKING:
    from ..redis import RedisStorage

logger = logging.getLogger(__name__)


class SessionRepository(ABC):
    """会话仓库抽象基类"""

    @abstractmethod
    async def save(self, session: dict[str, Any]) -> None:
        """保存会话"""
        pass

    @abstractmethod
    async def get(self, session_id: str) -> dict[str, Any] | None:
        """获取会话"""
        pass

    @abstractmethod
    async def list(
        self,
        user_id: str | None = None,
        tenant_id: str | None = None,
        service_id: str | None = None,
        status: str = "active",
        limit: int = 100,
    ) -> builtins.list[dict[str, Any]]:
        """获取会话列表"""
        pass

    @abstractmethod
    async def delete(self, session_id: str) -> bool:
        """删除会话"""
        pass

    @abstractmethod
    async def update_history(self, session_id: str, history: builtins.list[dict[str, Any]]) -> None:
        """更新会话历史"""
        pass

    @abstractmethod
    async def update_state(self, session_id: str, state: dict[str, Any]) -> None:
        """更新会话状态"""
        pass


class DatabaseSessionRepository(SessionRepository, BaseRepository):
    """基于 PostgreSQL 的会话仓库实现"""

    # JSON fields that should be parsed as dicts
    _JSON_DICT_FIELDS = {"metadata", "config", "state"}
    # JSON fields that should be parsed as lists
    _JSON_LIST_FIELDS = {"history"}

    def __init__(
        self,
        pool_holder: Any,
        redis: RedisStorage | None = None,
        cache_ttl: int = 3600,  # 会话缓存1小时
    ):
        BaseRepository.__init__(self, pool_holder)
        self.redis = redis
        self.cache_ttl = cache_ttl

    # ------------------------------------------------------------------
    # Row conversion
    # ------------------------------------------------------------------

    @staticmethod
    def _row_to_dict(row: Any) -> dict[str, Any]:
        """将数据库行转换为字典，正确处理 JSON 和 datetime 字段"""
        if not row:
            return {}
        result = dict(row)

        json_dict_fields = DatabaseSessionRepository._JSON_DICT_FIELDS
        json_list_fields = DatabaseSessionRepository._JSON_LIST_FIELDS

        for key, value in result.items():
            if isinstance(value, datetime):
                result[key] = value.isoformat()
            elif key in json_dict_fields and value is not None:
                if isinstance(value, str):
                    try:
                        result[key] = json.loads(value)
                    except (json.JSONDecodeError, TypeError):
                        result[key] = {}
                elif not isinstance(value, dict):
                    result[key] = {}
            elif key in json_list_fields and value is not None:
                if isinstance(value, str):
                    try:
                        result[key] = json.loads(value)
                    except (json.JSONDecodeError, TypeError):
                        result[key] = []
                elif not isinstance(value, list):
                    result[key] = []
        return result

    # ------------------------------------------------------------------
    # CRUD
    # ------------------------------------------------------------------

    async def save(self, session: dict[str, Any]) -> None:
        """保存或更新会话"""
        await self.execute(
            """
            INSERT INTO assistant.sessions (
                session_id, service_id, user_id, tenant_id,
                state, history, metadata, config, status, expires_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (session_id) DO UPDATE SET
                service_id = EXCLUDED.service_id,
                state = EXCLUDED.state,
                history = EXCLUDED.history,
                metadata = EXCLUDED.metadata,
                config = EXCLUDED.config,
                status = EXCLUDED.status,
                expires_at = EXCLUDED.expires_at,
                updated_at = NOW()
        """,
            session.get("session_id"),
            session.get("service_id"),
            session.get("user_id"),
            session.get("tenant_id"),
            json.dumps(session.get("state", {})),
            json.dumps(session.get("history", [])),
            json.dumps(session.get("metadata", {})),
            json.dumps(session.get("config", {})),
            session.get("status", "active"),
            session.get("expires_at"),
        )

        # 更新 Redis 缓存（热数据）
        if self.redis and self.redis.enabled:
            session_id = session.get("session_id")
            await self.redis.save_session(session_id, session, self.cache_ttl)

    async def get(self, session_id: str) -> dict[str, Any] | None:
        """获取会话，优先从 Redis 读取"""
        # 尝试从 Redis 获取（热缓存）
        if self.redis and self.redis.enabled:
            cached = await self.redis.get_session(session_id)
            if cached:
                return cached

        # 从数据库获取
        row = await self.fetchrow(
            "SELECT * FROM assistant.sessions "
            "WHERE session_id = $1 AND status <> 'deleted'",
            session_id,
        )
        session = self._row_to_dict(row) if row else None

        # 写入 Redis 缓存
        if session and self.redis and self.redis.enabled:
            await self.redis.save_session(session_id, session, self.cache_ttl)

        return session

    async def append_message(
        self,
        session_id: str,
        message: dict[str, Any],
        metadata_update: dict[str, Any] | None = None,
    ) -> bool:
        """
        原子追加消息到会话历史（避免竞态条件）

        使用 PostgreSQL 的 JSONB 操作原子地追加消息，而不是读取-修改-写入模式。

        Args:
            session_id: 会话 ID
            message: 消息字典 {role, content, timestamp, metadata}
            metadata_update: 可选的 metadata 更新（如自动标题）

        Returns:
            是否成功
        """
        if metadata_update:
            result = await self.execute(
                """
                UPDATE assistant.sessions
                SET history = history || $2::jsonb,
                    metadata = metadata || $3::jsonb,
                    updated_at = NOW()
                WHERE session_id = $1
            """,
                session_id,
                json.dumps([message]),
                json.dumps(metadata_update),
            )
        else:
            result = await self.execute(
                """
                UPDATE assistant.sessions
                SET history = history || $2::jsonb,
                    updated_at = NOW()
                WHERE session_id = $1
            """,
                session_id,
                json.dumps([message]),
            )
        return result == "UPDATE 1"

    async def list(
        self,
        user_id: str | None = None,
        tenant_id: str | None = None,
        service_id: str | None = None,
        status: str = "active",
        limit: int = 100,
    ) -> builtins.list[dict[str, Any]]:
        """获取会话列表"""
        if not self.enabled:
            return []

        query = "SELECT * FROM assistant.sessions WHERE 1=1"
        params: builtins.list[Any] = []
        param_idx = 1

        if user_id:
            query += f" AND user_id = ${param_idx}"
            params.append(user_id)
            param_idx += 1

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        if service_id:
            query += f" AND service_id = ${param_idx}"
            params.append(service_id)
            param_idx += 1

        if status:
            query += f" AND status = ${param_idx}"
            params.append(status)
            param_idx += 1

        # By default, hide expired sessions from list views.
        if status == "active":
            query += " AND (expires_at IS NULL OR expires_at > NOW())"

        query += f" ORDER BY updated_at DESC LIMIT ${param_idx}"
        params.append(limit)

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [self._row_to_dict(row) for row in rows]

    async def update_history(self, session_id: str, history: builtins.list[dict[str, Any]]) -> None:
        """更新会话历史"""
        await self.execute(
            "UPDATE assistant.sessions SET history = $1, updated_at = NOW() WHERE session_id = $2",
            json.dumps(history),
            session_id,
        )
        # 更新缓存
        if self.redis and self.redis.enabled:
            session = await self.get(session_id)
            if session:
                await self.redis.save_session(session_id, session, self.cache_ttl)

    async def update_state(self, session_id: str, state: dict[str, Any]) -> None:
        """更新会话状态"""
        await self.execute(
            "UPDATE assistant.sessions SET state = $1, updated_at = NOW() WHERE session_id = $2",
            json.dumps(state),
            session_id,
        )
        # 更新缓存
        if self.redis and self.redis.enabled:
            session = await self.get(session_id)
            if session:
                await self.redis.save_session(session_id, session, self.cache_ttl)

    async def update_metadata(self, session_id: str, metadata: dict[str, Any]) -> bool:
        """原子更新会话 metadata，避免覆盖 history。"""
        if not metadata:
            return True
        result = await self.execute(
            """
            UPDATE assistant.sessions
            SET metadata = COALESCE(metadata, '{}'::jsonb) || $2::jsonb,
                updated_at = NOW()
            WHERE session_id = $1
        """,
            session_id,
            json.dumps(metadata),
        )
        return result == "UPDATE 1"

    async def update_config(self, session_id: str, config: dict[str, Any]) -> bool:
        """原子更新会话 config，避免覆盖 history。"""
        if not config:
            return True
        result = await self.execute(
            """
            UPDATE assistant.sessions
            SET config = COALESCE(config, '{}'::jsonb) || $2::jsonb,
                updated_at = NOW()
            WHERE session_id = $1
        """,
            session_id,
            json.dumps(config),
        )
        return result == "UPDATE 1"

    async def delete(self, session_id: str) -> bool:
        """删除会话。

        与 ``DatabaseStorage.delete_session`` 同源：Agent Runtime 的审计链以
        RESTRICT 外键指向会话行，跑过对话的会话只能打墓碑标记，否则硬删除会以
        外键冲突失败。
        """
        try:
            result = await self.execute(
                "DELETE FROM assistant.sessions WHERE session_id = $1",
                session_id,
            )
        except asyncpg.exceptions.ForeignKeyViolationError:
            result = await self.execute(
                "UPDATE assistant.sessions "
                "SET status = 'deleted', updated_at = NOW() "
                "WHERE session_id = $1 AND status <> 'deleted'",
                session_id,
            )
            if self.redis and self.redis.enabled:
                await self.redis.delete_session(session_id)
            return result == "UPDATE 1"
        # 清除缓存
        if self.redis and self.redis.enabled:
            await self.redis.delete_session(session_id)
        return result == "DELETE 1"

    async def cleanup_expired(self) -> int:
        """清理过期会话"""
        if not self.enabled:
            return 0
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                "DELETE FROM assistant.sessions "
                "WHERE expires_at IS NOT NULL AND expires_at < NOW()"
            )
            # 解析结果获取删除行数
            if result.startswith("DELETE "):
                return int(result.split()[-1])
            return 0
