"""
用户仓库

提供用户数据的数据访问接口
"""

from __future__ import annotations

import builtins
import json
import logging
from abc import ABC, abstractmethod
from datetime import datetime
from typing import Any

from .base import BaseRepository

logger = logging.getLogger(__name__)


class UserRepository(ABC):
    """用户仓库抽象基类"""

    @abstractmethod
    async def save(self, user: dict[str, Any]) -> None:
        """保存用户"""
        pass

    @abstractmethod
    async def get(self, user_id: str) -> dict[str, Any] | None:
        """获取用户"""
        pass

    @abstractmethod
    async def list(
        self, tenant_id: str | None = None, status: str = "active", limit: int = 100
    ) -> builtins.list[dict[str, Any]]:
        """获取用户列表"""
        pass

    @abstractmethod
    async def update_last_active(self, user_id: str) -> None:
        """更新用户最后活跃时间"""
        pass

    @abstractmethod
    async def get_by_email(self, email: str) -> dict[str, Any] | None:
        """通过邮箱获取用户"""
        pass

    @abstractmethod
    async def update(self, user_id: str, updates: dict[str, Any]) -> None:
        """更新用户字段"""
        pass

    @abstractmethod
    async def delete(self, user_id: str) -> bool:
        """删除用户"""
        pass

    @abstractmethod
    async def list_paginated(
        self,
        status: str | None = None,
        search: str | None = None,
        tenant_id: str | None = None,
        limit: int = 20,
        offset: int = 0,
    ) -> tuple:
        """分页获取用户列表"""
        pass

    @abstractmethod
    async def save_with_password(self, user: dict[str, Any]) -> None:
        """保存或更新用户（包含密码字段）"""
        pass


class DatabaseUserRepository(UserRepository, BaseRepository):
    """基于 PostgreSQL 的用户仓库实现"""

    def __init__(self, pool_holder: Any):
        BaseRepository.__init__(self, pool_holder)

    # ------------------------------------------------------------------
    # Row conversion (mirrors DatabaseStorage._row_to_dict for user rows)
    # ------------------------------------------------------------------

    @staticmethod
    def _row_to_dict(row) -> dict[str, Any]:
        """将数据库行转换为字典，正确处理 JSON 和 datetime 字段"""
        if not row:
            return {}
        result = dict(row)

        json_dict_fields = {"metadata", "quota_config"}
        json_list_fields = {"roles", "permissions"}

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

    async def save(self, user: dict[str, Any]) -> None:
        """保存或更新用户"""
        await self.execute(
            """
            INSERT INTO users (
                user_id, username, email, display_name, tenant_id,
                tier, roles, permissions, quota_config, status, metadata
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT (user_id) DO UPDATE SET
                username = EXCLUDED.username,
                email = EXCLUDED.email,
                display_name = EXCLUDED.display_name,
                tier = EXCLUDED.tier,
                roles = EXCLUDED.roles,
                permissions = EXCLUDED.permissions,
                quota_config = EXCLUDED.quota_config,
                status = EXCLUDED.status,
                metadata = EXCLUDED.metadata,
                updated_at = NOW()
            """,
            user.get("user_id"),
            user.get("username"),
            user.get("email"),
            user.get("display_name"),
            user.get("tenant_id", "default"),
            user.get("tier", "normal"),
            user.get("roles", ["user"]),
            user.get("permissions", []),
            json.dumps(user.get("quota_config", {})),
            user.get("status", "active"),
            json.dumps(user.get("metadata", {})),
        )

    async def get(self, user_id: str) -> dict[str, Any] | None:
        """获取用户"""
        row = await self.fetchrow("SELECT * FROM users WHERE user_id = $1", user_id)
        return self._row_to_dict(row) if row else None

    async def list(
        self, tenant_id: str | None = None, status: str = "active", limit: int = 100
    ) -> builtins.list[dict[str, Any]]:
        """获取用户列表"""
        query = "SELECT * FROM users WHERE 1=1"
        params: builtins.list[Any] = []
        param_idx = 1

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        if status:
            query += f" AND status = ${param_idx}"
            params.append(status)
            param_idx += 1

        query += f" ORDER BY created_at DESC LIMIT ${param_idx}"
        params.append(limit)

        rows = await self.fetch(query, *params)
        return [self._row_to_dict(row) for row in rows]

    async def update_last_active(self, user_id: str) -> None:
        """更新用户最后活跃时间"""
        await self.execute(
            "UPDATE users SET last_active_at = NOW() WHERE user_id = $1", user_id
        )

    async def get_by_email(self, email: str) -> dict[str, Any] | None:
        """通过邮箱获取用户"""
        row = await self.fetchrow(
            "SELECT * FROM users WHERE LOWER(email) = LOWER($1)", email
        )
        return self._row_to_dict(row) if row else None

    async def save_with_password(self, user: dict[str, Any]) -> None:
        """保存或更新用户（包含密码字段）"""
        await self.execute(
            """
            INSERT INTO users (
                user_id, username, email, display_name, department, tenant_id,
                tier, roles, permissions, quota_config, status,
                password_hash, force_password_change, email_verified,
                created_by, metadata
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
            ON CONFLICT (user_id) DO UPDATE SET
                username = EXCLUDED.username,
                email = EXCLUDED.email,
                display_name = EXCLUDED.display_name,
                department = EXCLUDED.department,
                tier = EXCLUDED.tier,
                roles = EXCLUDED.roles,
                permissions = EXCLUDED.permissions,
                quota_config = EXCLUDED.quota_config,
                status = EXCLUDED.status,
                updated_at = NOW()
            """,
            user.get("user_id"),
            user.get("username") or user.get("email", "").split("@")[0],
            user.get("email"),
            user.get("display_name"),
            user.get("department"),
            user.get("tenant_id", "default"),
            user.get("tier", "normal"),
            user.get("roles", ["user"]),
            user.get("permissions", []),
            json.dumps(user.get("quota_config", {})),
            user.get("status", "active"),
            user.get("password_hash"),
            user.get("force_password_change", True),
            user.get("email_verified", False),
            user.get("created_by"),
            json.dumps(user.get("metadata", {})),
        )

    async def update(self, user_id: str, updates: dict[str, Any]) -> None:
        """更新用户字段"""
        if not self.enabled or not updates:
            return

        allowed_fields = {
            "display_name",
            "username",
            "department",
            "tier",
            "roles",
            "permissions",
            "quota_config",
            "status",
            "metadata",
            "email_verified",
        }
        filtered = {k: v for k, v in updates.items() if k in allowed_fields}
        if not filtered:
            return

        set_clauses = ["updated_at = NOW()"]
        params: builtins.list[Any] = []
        param_idx = 1

        for key, value in filtered.items():
            if key in ("quota_config", "metadata") and isinstance(value, dict):
                set_clauses.append(f"{key} = ${param_idx}")
                params.append(json.dumps(value))
            else:
                set_clauses.append(f"{key} = ${param_idx}")
                params.append(value)
            param_idx += 1

        params.append(user_id)
        from ..database import _build_safe_set_clause
        query = f"UPDATE users SET {_build_safe_set_clause(set_clauses)} WHERE user_id = ${param_idx}"

        await self.execute(query, *params)

    async def delete(self, user_id: str) -> bool:
        """删除用户"""
        if not self.enabled:
            return False
        async with self._pool.acquire() as conn:
            # 先删除 user_roles
            await conn.execute("DELETE FROM user_roles WHERE user_id = $1", user_id)
            # 再删除用户
            result = await conn.execute("DELETE FROM users WHERE user_id = $1", user_id)
            return result == "DELETE 1"

    async def list_paginated(
        self,
        status: str | None = None,
        search: str | None = None,
        tenant_id: str | None = None,
        limit: int = 20,
        offset: int = 0,
    ) -> tuple:
        """分页获取用户列表"""
        if not self.enabled:
            return [], 0

        query = "SELECT * FROM users WHERE 1=1"
        count_query = "SELECT COUNT(*) FROM users WHERE 1=1"
        params: builtins.list[Any] = []
        param_idx = 1

        if status:
            query += f" AND status = ${param_idx}"
            count_query += f" AND status = ${param_idx}"
            params.append(status)
            param_idx += 1

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            count_query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        if search:
            query += f" AND (email ILIKE ${param_idx} OR display_name ILIKE ${param_idx} OR username ILIKE ${param_idx})"
            count_query += f" AND (email ILIKE ${param_idx} OR display_name ILIKE ${param_idx} OR username ILIKE ${param_idx})"
            params.append(f"%{search}%")
            param_idx += 1

        async with self._pool.acquire() as conn:
            total = await conn.fetchval(count_query, *params)

            query += f" ORDER BY created_at DESC LIMIT ${param_idx} OFFSET ${param_idx + 1}"
            params.extend([limit, offset])

            rows = await conn.fetch(query, *params)
            users = [self._row_to_dict(row) for row in rows]

            return users, total or 0

    # ------------------------------------------------------------------
    # Convenience
    # ------------------------------------------------------------------

    async def get_or_create(
        self,
        user_id: str,
        tenant_id: str = "default",
        tier: str = "normal",
        roles: builtins.list[str] | None = None,
    ) -> dict[str, Any]:
        """获取或创建用户"""
        user = await self.get(user_id)
        if not user:
            user = {
                "user_id": user_id,
                "tenant_id": tenant_id,
                "tier": tier,
                "roles": roles or ["user"],
                "status": "active",
            }
            await self.save(user)
        return user
