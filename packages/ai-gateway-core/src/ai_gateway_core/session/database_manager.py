"""
数据库会话管理器

基于 PostgreSQL 的 SessionManager 实现，支持会话持久化
"""

from __future__ import annotations

import time
import uuid
from collections import OrderedDict
from datetime import datetime, timedelta
from typing import TYPE_CHECKING, Any

from ai_gateway_core.exceptions import PermissionDeniedError, SessionAlreadyExistsError

from .models import Session, SessionMessage

if TYPE_CHECKING:
    from ai_gateway_core.persistence import DatabaseStorage
    # RedisStorage still lives in gateway src/persistence/redis.py and is
    # only used when callers wire a Redis client (gateway does, AS does
    # not). Aliasing to Any here keeps this module importable without
    # gateway src/ on PYTHONPATH; the runtime type is duck-typed anyway.
    RedisStorage = Any  # type: ignore[assignment, misc]


class DatabaseSessionManager:
    """基于数据库的会话管理器"""

    def __init__(
        self,
        database: DatabaseStorage,
        redis: RedisStorage | None = None,
        cache_ttl: int = 3600,  # Redis 缓存 1 小时
        memory_cache_max_entries: int = 1024,
        default_session_ttl: int = 86400 * 7,  # 默认会话有效期 7 天
        anonymous_session_ttl: int = 86400,  # 匿名会话有效期 24 小时
    ):
        self.database = database
        self.redis = redis
        self.cache_ttl = cache_ttl
        self.memory_cache_max_entries = max(int(memory_cache_max_entries), 1)
        self.default_session_ttl = default_session_ttl
        self.anonymous_session_ttl = anonymous_session_ttl

        # 内存缓存，用于没有 Redis 时
        self._memory_cache: OrderedDict[str, Session] = OrderedDict()
        self._memory_cache_deadlines: dict[str, float] = {}

    async def create(
        self,
        user_id: str,
        tenant_id: str = "",
        service_id: str = None,
        session_id: str = None,
        metadata: dict | None = None,
        config: dict | None = None,
        ttl: int = None,
        fail_if_exists: bool = False,
    ) -> Session:
        """创建新会话"""
        session_id_value = session_id or str(uuid.uuid4())
        now = datetime.utcnow()

        ttl_seconds = ttl
        if ttl_seconds is None:
            ttl_seconds = (
                self.anonymous_session_ttl
                if (user_id or "").startswith("anon:")
                else self.default_session_ttl
            )
        expires_at = now + timedelta(seconds=int(ttl_seconds)) if ttl_seconds else None

        session = Session(
            session_id=session_id_value,
            user_id=user_id,
            tenant_id=tenant_id,
            service_id=service_id,
            created_at=now,
            updated_at=now,
            metadata=metadata or {},
            history=[],
            state={},
            config=config or {},
            status="active",
            expires_at=expires_at,
        )

        # Client-selected ids must be inserted atomically. A read-before-write
        # check would still allow two tenants to race on the same UUID.
        if fail_if_exists or session_id is not None:
            created = await self.database.create_session_if_absent(
                self._session_to_dict(session)
            )
            if not created:
                raise SessionAlreadyExistsError("session already exists")
        else:
            await self._save_to_db(session)

        # 写入缓存
        await self._cache_session(session)

        return session

    async def bind_agent_runtime(
        self,
        *,
        session_id: str,
        user_id: str,
        tenant_id: str,
        agent_id: str,
        agent_version_id: str | None,
        agent_draft_revision: int | None,
        publication_id: str | None,
        channel: str,
        runtime_fingerprint: str,
        agent_spec_hash: str,
        ttl: int | None = None,
    ) -> Session:
        """Atomically create or verify an immutable Agent runtime session pin."""

        ttl_seconds = self.default_session_ttl if ttl is None else int(ttl)
        expires_at = (
            datetime.utcnow() + timedelta(seconds=ttl_seconds)
            if ttl_seconds
            else None
        )
        row = await self.database.bind_agent_runtime_session(
            session_id=session_id,
            service_id="__builtin_assistant__",
            user_id=user_id,
            tenant_id=tenant_id,
            agent_id=agent_id,
            agent_version_id=agent_version_id,
            agent_draft_revision=agent_draft_revision,
            publication_id=publication_id,
            channel=channel,
            runtime_fingerprint=runtime_fingerprint,
            agent_spec_hash=agent_spec_hash,
            expires_at=expires_at,
        )
        if not row:
            raise PermissionDeniedError("Session is bound to a different Agent runtime")
        session = self._dict_to_session(row)
        await self._remove_from_cache(session_id)
        await self._cache_session(session)
        return session

    async def get(self, session_id: str) -> Session | None:
        """获取会话"""
        # 尝试从缓存获取
        session = await self._get_from_cache(session_id)
        if session:
            return session

        # 从数据库获取
        session_dict = await self.database.get_session(session_id)
        if not session_dict:
            return None

        session = self._dict_to_session(session_dict)

        # Don't reject expired sessions — auto-extend TTL on access instead.
        # This prevents sessions from silently vanishing while users actively use them.
        if session.expires_at:
            expires = session.expires_at.replace(tzinfo=None) if session.expires_at.tzinfo else session.expires_at
            if expires < datetime.utcnow():
                # Auto-extend by 7 days on access
                new_ttl = self.default_session_ttl
                await self.extend_ttl(session.session_id, new_ttl)
                session.expires_at = datetime.utcnow() + timedelta(seconds=new_ttl)

        # 写入缓存
        await self._cache_session(session)

        return session

    async def get_or_create(
        self,
        session_id: str | None,
        user_id: str,
        tenant_id: str = "",
        service_id: str = None,
        ttl: int = None,
    ) -> Session:
        """获取或创建会话"""
        if session_id:
            session = await self.get(session_id)
            if session:
                if session.user_id != user_id or session.tenant_id != tenant_id:
                    raise PermissionDeniedError("Session does not belong to current user")
                if service_id:
                    if session.service_id and session.service_id != service_id:
                        raise PermissionDeniedError("Session is bound to a different service")
                    if not session.service_id:
                        session.service_id = service_id
                return session

        return await self.create(
            user_id=user_id,
            tenant_id=tenant_id,
            service_id=service_id,
            session_id=None,  # do not accept unknown client-provided IDs
            ttl=ttl,
        )

    async def list_sessions(
        self,
        user_id: str | None = None,
        tenant_id: str | None = None,
        service_id: str | None = None,
        status: str = "active",
        limit: int = 100,
    ) -> list[Session]:
        """获取会话列表"""
        sessions_dict = await self.database.list_sessions(
            user_id=user_id,
            tenant_id=tenant_id,
            service_id=service_id,
            status=status,
            limit=limit,
        )
        sessions: list[Session] = []
        for row in sessions_dict:
            s = self._dict_to_session(row)
            # Don't silently filter expired sessions — return them all.
            # The UI should show them (with optional "expired" badge).
            # Hard expiry cleanup is handled separately by cleanup_expired_sessions().
            sessions.append(s)
        return sessions

    async def list_session_summaries(
        self,
        user_id: str | None = None,
        tenant_id: str | None = None,
        service_ids: list[str] | None = None,
        include_null_service_id: bool = False,
        status: str = "active",
        limit: int = 100,
    ) -> list[dict]:
        """Lightweight session list for sidebar — skips history/state, no Session object construction."""
        return await self.database.list_session_summaries(
            user_id=user_id,
            tenant_id=tenant_id,
            service_ids=service_ids,
            include_null_service_id=include_null_service_id,
            status=status,
            limit=limit,
        )

    async def delete(self, session_id: str) -> bool:
        """删除会话"""
        result = await self.database.delete_session(session_id)

        # 清除缓存
        await self._remove_from_cache(session_id)

        return result

    async def add_message(
        self,
        session_id: str,
        role: str,
        content: str,
        metadata: dict | None = None,
    ) -> bool:
        """
        添加消息到会话历史

        使用数据库原子操作追加消息，避免竞态条件导致的消息丢失。
        """
        now = datetime.utcnow()

        # 构建消息字典
        message_dict = {
            "role": role,
            "content": content,
            "timestamp": now.isoformat(),
            "metadata": metadata,
        }

        # Auto-title from first user message (ChatGPT-like sidebar)
        metadata_update = None
        if role == "user":
            # 检查是否需要设置标题
            session = await self.get(session_id)
            if session and not session.metadata.get("title"):
                title = str(content).strip().splitlines()[0][:40]
                if title:
                    metadata_update = {"title": title}

        # 使用原子操作追加消息（避免竞态条件）
        result = await self.database.append_session_message(
            session_id=session_id,
            message=message_dict,
            metadata_update=metadata_update,
        )

        if result:
            # 清除缓存，下次获取时从数据库重新加载
            await self._remove_from_cache(session_id)

        return result

    async def history(
        self,
        session_id: str,
        limit: int = 50,
    ) -> list[SessionMessage]:
        """获取会话历史"""
        session = await self.get(session_id)
        if not session:
            return []
        return session.history[-limit:]

    async def update_state(
        self,
        session_id: str,
        state: dict[str, Any],
    ) -> bool:
        """更新会话状态"""
        session = await self.get(session_id)
        if not session:
            return False

        session.state = state
        session.updated_at = datetime.utcnow()

        # 更新数据库
        await self.database.update_session_state(session_id, state)

        # A concurrent append or metadata patch may have advanced the
        # database after the snapshot loaded above. Do not put it back.
        await self._remove_from_cache(session_id)

        return True

    async def update_metadata(
        self,
        session_id: str,
        metadata: dict[str, Any],
    ) -> bool:
        """更新会话元数据"""
        session = await self.get(session_id)
        if not session:
            return False

        # Safety net: reject metadata > 1MB to prevent JSONB bloat
        # (2026-04-10: image_chat_history was writing 15MB of base64)
        import json as _json
        try:
            serialized = _json.dumps(metadata)
            if len(serialized) > 1_048_576:  # 1 MB
                import logging
                logging.getLogger(__name__).error(
                    f"[SESSION] Refusing metadata write for {session_id}: "
                    f"{len(serialized)} bytes > 1MB limit. Keys: {list(metadata.keys())}"
                )
                return False
        except (TypeError, ValueError):
            pass  # Non-serializable — let the DB error handler catch it

        # 原子更新 metadata，避免 save_session 覆盖 history
        result = await self.database.update_session_metadata(session_id, metadata)
        if not result:
            return False

        # 清除缓存，避免并发情况下写回过期 history
        await self._remove_from_cache(session_id)
        return True

    async def update_config(
        self,
        session_id: str,
        config: dict[str, Any],
    ) -> bool:
        """更新会话配置（知识库、模型等）"""
        session = await self.get(session_id)
        if not session:
            return False

        # 原子更新 config，避免 save_session 覆盖 history
        result = await self.database.update_session_config(session_id, config)
        if not result:
            return False

        # 清除缓存，避免并发情况下写回过期 history
        await self._remove_from_cache(session_id)
        return True

    async def extend_ttl(
        self,
        session_id: str,
        additional_seconds: int = None,
    ) -> bool:
        """延长会话有效期 — only updates expires_at, NOT updated_at.

        Uses direct SQL instead of save_session() which hardcodes updated_at=NOW().
        """
        seconds = additional_seconds or self.default_session_ttl
        new_expires = datetime.utcnow() + timedelta(seconds=seconds)

        if self.database and getattr(self.database, "_pool", None):
            async with self.database._pool.acquire() as conn:
                await conn.execute(
                    "UPDATE assistant.sessions SET expires_at = $1 WHERE session_id = $2",
                    new_expires, session_id,
                )
        return True

    async def cleanup_expired(self) -> int:
        """清理过期会话"""
        count = await self.database.cleanup_expired_sessions()
        return count

    # =========================================================================
    # 私有方法
    # =========================================================================

    async def _save_to_db(self, session: Session) -> None:
        """保存会话到数据库"""
        session_dict = self._session_to_dict(session)
        await self.database.save_session(session_dict)

    async def _cache_session(self, session: Session) -> None:
        """缓存会话"""
        if self.redis and self.redis.enabled:
            session_dict = self._session_to_dict(session)
            await self.redis.save_session(session.session_id, session_dict, self.cache_ttl)
        else:
            self._memory_cache[session.session_id] = session
            self._memory_cache.move_to_end(session.session_id)
            self._memory_cache_deadlines[session.session_id] = (
                time.monotonic() + max(int(self.cache_ttl), 1)
            )
            while len(self._memory_cache) > self.memory_cache_max_entries:
                evicted_id, _ = self._memory_cache.popitem(last=False)
                self._memory_cache_deadlines.pop(evicted_id, None)

    async def _get_from_cache(self, session_id: str) -> Session | None:
        """从缓存获取会话"""
        if self.redis and self.redis.enabled:
            cached = await self.redis.get_session(session_id)
            if cached:
                return self._dict_to_session(cached)
            return None
        else:
            session = self._memory_cache.get(session_id)
            if session is None:
                return None
            deadline = self._memory_cache_deadlines.get(session_id)
            if deadline is not None and deadline <= time.monotonic():
                self._memory_cache.pop(session_id, None)
                self._memory_cache_deadlines.pop(session_id, None)
                return None
            if deadline is None:
                self._memory_cache_deadlines[session_id] = (
                    time.monotonic() + max(int(self.cache_ttl), 1)
                )
            self._memory_cache.move_to_end(session_id)
            return session

    async def _remove_from_cache(self, session_id: str) -> None:
        """从缓存移除会话"""
        if self.redis and self.redis.enabled:
            await self.redis.delete_session(session_id)
        else:
            self._memory_cache.pop(session_id, None)
            self._memory_cache_deadlines.pop(session_id, None)

    def _session_to_dict(self, session: Session) -> dict[str, Any]:
        """将 Session 转换为字典"""
        return {
            "session_id": session.session_id,
            "service_id": session.service_id,
            "user_id": session.user_id,
            "tenant_id": session.tenant_id,
            "state": session.state or {},
            "history": [self._message_to_dict(m) for m in session.history],
            "metadata": session.metadata or {},
            "config": getattr(session, "config", {}) or {},
            "status": getattr(session, "status", "active"),
            "expires_at": session.expires_at,
            "agent_id": session.agent_id,
            "agent_version_id": session.agent_version_id,
            "agent_draft_revision": session.agent_draft_revision,
            "publication_id": session.publication_id,
            "channel": session.channel,
            "runtime_fingerprint": session.runtime_fingerprint,
            "agent_spec_hash": session.agent_spec_hash,
        }

    def _message_to_dict(self, message: SessionMessage) -> dict[str, Any]:
        """将 SessionMessage 转换为字典"""
        return {
            "role": message.role,
            "content": message.content,
            "timestamp": message.timestamp.isoformat() if message.timestamp else None,
            "metadata": getattr(message, "metadata", None),
        }

    def _dict_to_session(self, data: dict[str, Any]) -> Session:
        """将字典转换为 Session"""
        history = []
        for msg_data in data.get("history", []):
            if isinstance(msg_data, dict):
                timestamp = msg_data.get("timestamp")
                if isinstance(timestamp, str):
                    timestamp = datetime.fromisoformat(timestamp.replace("Z", "+00:00"))
                history.append(
                    SessionMessage(
                        role=msg_data.get("role", "user"),
                        content=msg_data.get("content", ""),
                        timestamp=timestamp,
                        metadata=msg_data.get("metadata"),
                    )
                )

        created_at = data.get("created_at")
        if isinstance(created_at, str):
            created_at = datetime.fromisoformat(created_at.replace("Z", "+00:00"))

        updated_at = data.get("updated_at")
        if isinstance(updated_at, str):
            updated_at = datetime.fromisoformat(updated_at.replace("Z", "+00:00"))

        expires_at = data.get("expires_at")
        if isinstance(expires_at, str):
            expires_at = datetime.fromisoformat(expires_at.replace("Z", "+00:00"))

        return Session(
            session_id=data.get("session_id"),
            service_id=data.get("service_id"),
            user_id=data.get("user_id", ""),
            tenant_id=data.get("tenant_id", ""),
            created_at=created_at or datetime.utcnow(),
            updated_at=updated_at,
            metadata=data.get("metadata") or {},
            history=history,
            state=data.get("state") or {},
            config=data.get("config") or {},
            status=data.get("status", "active"),
            expires_at=expires_at,
            agent_id=str(data.get("agent_id")) if data.get("agent_id") else None,
            agent_version_id=(
                str(data.get("agent_version_id"))
                if data.get("agent_version_id")
                else None
            ),
            agent_draft_revision=(
                int(data["agent_draft_revision"])
                if data.get("agent_draft_revision") is not None
                else None
            ),
            publication_id=(
                str(data.get("publication_id"))
                if data.get("publication_id")
                else None
            ),
            channel=str(data.get("channel")) if data.get("channel") else None,
            runtime_fingerprint=(
                str(data.get("runtime_fingerprint"))
                if data.get("runtime_fingerprint")
                else None
            ),
            agent_spec_hash=(
                str(data.get("agent_spec_hash"))
                if data.get("agent_spec_hash")
                else None
            ),
        )
