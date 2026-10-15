"""
游客会话管理器

为游客提供会话管理能力：
- 创建临时会话
- 验证会话有效性
- 刷新会话过期时间
- 游客转正式用户（数据迁移）
"""

from __future__ import annotations

import json
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any

from ai_gateway_core.logging import get_logger

logger = get_logger(__name__)


@dataclass
class GuestSessionConfig:
    """游客会话配置"""

    session_ttl: int = 7 * 24 * 3600  # 7 天过期
    max_threads_per_session: int = 5  # 每个游客最多 5 个对话
    memory_enabled: bool = False  # 是否启用跨会话记忆
    key_prefix: str = "guest_session"


@dataclass
class GuestSession:
    """游客会话"""

    session_id: str
    created_at: datetime
    last_active_at: datetime
    thread_ids: list[str] = field(default_factory=list)
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "session_id": self.session_id,
            "created_at": self.created_at.isoformat(),
            "last_active_at": self.last_active_at.isoformat(),
            "thread_ids": self.thread_ids,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> GuestSession:
        return cls(
            session_id=data["session_id"],
            created_at=datetime.fromisoformat(data["created_at"]),
            last_active_at=datetime.fromisoformat(data["last_active_at"]),
            thread_ids=data.get("thread_ids", []),
            metadata=data.get("metadata", {}),
        )


class GuestSessionManager:
    """
    游客会话管理器

    游客没有用户账号，通过 session 识别身份。
    支持 Redis 和内存两种存储后端。
    """

    def __init__(
        self,
        config: GuestSessionConfig,
        redis_client: Any | None = None,
    ):
        self.config = config
        self.redis = redis_client

        # 内存存储（当 Redis 不可用时）
        self._sessions: dict[str, GuestSession] = {}

    async def create_session(
        self,
        metadata: dict[str, Any] | None = None,
    ) -> GuestSession:
        """
        创建游客会话

        1. 生成唯一 session_id
        2. 存储到 Redis（或内存），设置过期时间
        3. 返回 session 信息
        """
        session_id = f"guest_{uuid.uuid4().hex}"
        now = datetime.now(timezone.utc)

        session = GuestSession(
            session_id=session_id,
            created_at=now,
            last_active_at=now,
            metadata=metadata or {},
        )

        await self._save_session(session)

        logger.info(f"Created guest session: {session_id}")
        return session

    async def validate_session(self, session_id: str) -> bool:
        """验证会话是否有效"""
        session = await self._get_session(session_id)
        return session is not None

    async def get_session(self, session_id: str) -> GuestSession | None:
        """获取会话信息"""
        return await self._get_session(session_id)

    async def refresh_session(self, session_id: str) -> bool:
        """刷新会话过期时间"""
        session = await self._get_session(session_id)
        if not session:
            return False

        session.last_active_at = datetime.now(timezone.utc)
        await self._save_session(session)
        return True

    async def delete_session(self, session_id: str) -> bool:
        """删除会话"""
        key = self._session_key(session_id)

        if self.redis:
            result = await self.redis.delete(key)
            return result > 0
        else:
            if session_id in self._sessions:
                del self._sessions[session_id]
                return True
            return False

    async def add_thread_to_session(
        self,
        session_id: str,
        thread_id: str,
    ) -> bool:
        """将 Thread 关联到会话"""
        session = await self._get_session(session_id)
        if not session:
            return False

        # 检查配额
        if len(session.thread_ids) >= self.config.max_threads_per_session:
            logger.warning(
                f"Guest session {session_id} reached thread limit: "
                f"{len(session.thread_ids)}/{self.config.max_threads_per_session}"
            )
            return False

        if thread_id not in session.thread_ids:
            session.thread_ids.append(thread_id)
            session.last_active_at = datetime.now(timezone.utc)
            await self._save_session(session)

        return True

    async def remove_thread_from_session(
        self,
        session_id: str,
        thread_id: str,
    ) -> bool:
        """从会话移除 Thread"""
        session = await self._get_session(session_id)
        if not session:
            return False

        if thread_id in session.thread_ids:
            session.thread_ids.remove(thread_id)
            await self._save_session(session)

        return True

    async def get_session_threads(self, session_id: str) -> list[str]:
        """获取会话关联的所有 Thread"""
        session = await self._get_session(session_id)
        if not session:
            return []
        return session.thread_ids

    async def convert_to_user(
        self,
        session_id: str,
        user_id: str,
        langgraph_proxy: Any | None = None,
    ) -> dict[str, Any]:
        """
        游客转正式用户

        1. 获取游客的所有 Thread
        2. 更新 Thread 的 owner metadata（如果提供了 langgraph_proxy）
        3. 删除游客 session
        4. 返回迁移结果
        """
        session = await self._get_session(session_id)
        if not session:
            return {
                "success": False,
                "error": "Session not found",
            }

        migrated_threads = []
        failed_threads = []

        # 迁移 Thread 所有权
        if langgraph_proxy:
            from ...core.auth.user_resolver import UserContext

            # 创建管理员上下文用于迁移
            admin_context = UserContext(
                user_id=user_id,
                tenant_id="",
                tier="admin",
                is_authenticated=True,
                roles=["admin"],
            )

            for thread_id in session.thread_ids:
                try:
                    # 更新 Thread 的 owner_id
                    await langgraph_proxy.update_thread(
                        user=admin_context,
                        thread_id=thread_id,
                        metadata={
                            "owner_id": user_id,
                            "migrated_from_guest": session_id,
                            "migrated_at": datetime.now(timezone.utc).isoformat(),
                        },
                    )
                    migrated_threads.append(thread_id)
                except Exception as e:
                    logger.warning(f"Failed to migrate thread {thread_id}: {e}")
                    failed_threads.append(thread_id)
        else:
            # 没有代理时，只记录 thread_ids
            migrated_threads = session.thread_ids

        # 删除游客 session
        await self.delete_session(session_id)

        logger.info(
            f"Converted guest session {session_id} to user {user_id}, "
            f"migrated {len(migrated_threads)} threads"
        )

        return {
            "success": True,
            "user_id": user_id,
            "migrated_threads": migrated_threads,
            "failed_threads": failed_threads,
        }

    async def cleanup_expired_sessions(self) -> int:
        """
        清理过期的游客会话

        此方法应该由后台任务定期调用
        返回清理的会话数量
        """
        if self.redis:
            # Redis 会自动过期，不需要手动清理
            return 0

        # 内存存储需要手动清理
        now = datetime.now(timezone.utc)
        expired_sessions = []

        for session_id, session in self._sessions.items():
            age = (now - session.last_active_at).total_seconds()
            if age > self.config.session_ttl:
                expired_sessions.append(session_id)

        for session_id in expired_sessions:
            del self._sessions[session_id]

        if expired_sessions:
            logger.info(f"Cleaned up {len(expired_sessions)} expired guest sessions")

        return len(expired_sessions)

    # ============ 私有方法 ============

    def _session_key(self, session_id: str) -> str:
        """生成 Redis key"""
        return f"{self.config.key_prefix}:{session_id}"

    async def _get_session(self, session_id: str) -> GuestSession | None:
        """获取会话"""
        key = self._session_key(session_id)

        if self.redis:
            data = await self.redis.get(key)
            if not data:
                return None
            try:
                if isinstance(data, dict):
                    return GuestSession.from_dict(data)
                return GuestSession.from_dict(json.loads(data))
            except Exception:
                return None
        else:
            return self._sessions.get(session_id)

    async def _save_session(self, session: GuestSession) -> None:
        """保存会话"""
        key = self._session_key(session.session_id)

        if self.redis:
            data = json.dumps(session.to_dict())
            if hasattr(self.redis, "save"):
                await self.redis.save(
                    key,
                    data,
                    self.config.session_ttl,
                )
            else:
                await self.redis.setex(
                    key,
                    self.config.session_ttl,
                    data,
                )
        else:
            self._sessions[session.session_id] = session


# ============ 游客会话验证器 ============


async def create_guest_session_validator(
    manager: GuestSessionManager,
):
    """创建游客会话验证器（用于 AuthMiddleware）"""

    async def validator(session_id: str) -> dict[str, Any] | None:
        session = await manager.get_session(session_id)
        if not session:
            return None

        # 刷新会话
        await manager.refresh_session(session_id)

        return {
            "session_id": session.session_id,
            "created_at": session.created_at.isoformat(),
            "thread_count": len(session.thread_ids),
        }

    return validator
