"""
API Key 仓库

提供 API Key 的数据访问接口
"""

from __future__ import annotations

import builtins
import contextlib
import hashlib
import json
import logging
import secrets
from abc import ABC, abstractmethod
from datetime import datetime
from typing import Any

from .base import BaseRepository

logger = logging.getLogger(__name__)


class APIKeyRepository(ABC):
    """API Key 仓库抽象基类"""

    @abstractmethod
    async def create(
        self,
        name: str = None,
        description: str = None,
        tenant_id: str = None,
        user_id: str = None,
        roles: builtins.list[str] = None,
        permissions: builtins.list[str] = None,
        tier: str = "normal",
        rate_limit: dict = None,
        allowed_services: builtins.list[str] = None,
        expires_at: datetime = None,
    ) -> str:
        """创建 API Key，返回明文 Key（仅此一次）"""
        pass

    @abstractmethod
    async def validate(self, api_key: str) -> dict[str, Any] | None:
        """验证 API Key，返回 Key 信息"""
        pass

    @abstractmethod
    async def list(
        self, tenant_id: str | None = None, user_id: str | None = None, enabled: bool = None
    ) -> builtins.list[dict[str, Any]]:
        """获取 API Key 列表"""
        pass

    @abstractmethod
    async def disable(self, key_id: int) -> bool:
        """禁用 API Key"""
        pass

    @abstractmethod
    async def delete(self, key_id: int) -> bool:
        """删除 API Key"""
        pass


class DatabaseAPIKeyRepository(APIKeyRepository, BaseRepository):
    """基于 PostgreSQL 的 API Key 仓库实现"""

    def __init__(self, pool_holder: Any):
        BaseRepository.__init__(self, pool_holder)

    # ------------------------------------------------------------------
    # Key generation helpers
    # ------------------------------------------------------------------

    @staticmethod
    def _generate_key() -> str:
        """生成 API Key"""
        return f"gw_{secrets.token_urlsafe(32)}"

    @staticmethod
    def _hash_key(api_key: str) -> str:
        """计算 API Key 哈希"""
        return hashlib.sha256(api_key.encode()).hexdigest()

    @staticmethod
    def _row_to_dict(row) -> dict[str, Any]:
        """将数据库行转换为字典，正确处理 JSON 和 datetime 字段"""
        if not row:
            return {}
        result = dict(row)

        # JSON 字段列表 - 需要解析为 Python 对象（字典类型）
        json_dict_fields = {"metadata", "embedding_config", "index_config", "result", "config"}

        # JSON 字段列表 - 需要解析为 Python 对象（列表类型）
        json_list_fields = {
            "roles",
            "keywords",
            "include_patterns",
            "exclude_patterns",
            "labels",
            "events",
            "history",
        }

        for key, value in result.items():
            # 处理 datetime 类型
            if isinstance(value, datetime):
                result[key] = value.isoformat()
            # 处理 JSON 字典字段
            elif key in json_dict_fields and value is not None:
                if isinstance(value, str):
                    try:
                        result[key] = json.loads(value)
                    except (json.JSONDecodeError, TypeError):
                        result[key] = {}
                elif not isinstance(value, dict):
                    result[key] = {}
            # 处理 JSON 列表字段
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
    # CRUD operations
    # ------------------------------------------------------------------

    async def create(
        self,
        name: str = None,
        description: str = None,
        tenant_id: str = None,
        user_id: str = None,
        roles: builtins.list[str] = None,
        permissions: builtins.list[str] = None,
        tier: str = "normal",
        rate_limit: dict = None,
        allowed_services: builtins.list[str] = None,
        expires_at: datetime = None,
    ) -> str:
        """创建 API Key，返回明文 Key（仅此一次）"""
        api_key = self._generate_key()
        key_hash = self._hash_key(api_key)

        await self.fetchrow(
            """
                INSERT INTO api_keys (
                    key_hash, name, description, tenant_id, user_id,
                    roles, permissions, tier, rate_limit, allowed_services, allowed_models, expires_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                ON CONFLICT (key_hash) DO UPDATE SET
                    name = EXCLUDED.name,
                    description = EXCLUDED.description,
                    tenant_id = EXCLUDED.tenant_id,
                    user_id = EXCLUDED.user_id,
                    roles = EXCLUDED.roles,
                    permissions = EXCLUDED.permissions,
                    tier = EXCLUDED.tier,
                    rate_limit = EXCLUDED.rate_limit,
                    allowed_services = EXCLUDED.allowed_services,
                    allowed_models = EXCLUDED.allowed_models,
                    expires_at = EXCLUDED.expires_at,
                    updated_at = NOW()
                RETURNING id
            """,
            key_hash,
            name,
            description,
            tenant_id,
            user_id,
            roles or ["user"],
            permissions or [],
            tier,
            json.dumps(rate_limit) if rate_limit else None,
            allowed_services or [],
            [],  # allowed_models
            expires_at,
        )

        return api_key

    async def validate(self, api_key: str) -> dict[str, Any] | None:
        """验证 API Key，返回 Key 信息"""
        key_hash = self._hash_key(api_key)

        row = await self.fetchrow(
            """
                SELECT * FROM api_keys
                WHERE key_hash = $1 AND enabled = TRUE
                AND (expires_at IS NULL OR expires_at > NOW())
            """,
            key_hash,
        )

        if row:
            with contextlib.suppress(Exception):
                await self._track_api_key_usage(key_hash)

        return self._row_to_dict(row) if row else None

    async def list(
        self, tenant_id: str | None = None, user_id: str | None = None, enabled: bool = None
    ) -> builtins.list[dict[str, Any]]:
        """获取 API Key 列表（不返回哈希）"""
        query = """
            SELECT id, name, description, tenant_id, user_id, roles,
                   permissions, tier, rate_limit, allowed_services, allowed_models,
                   expires_at, enabled, last_used_at, use_count, created_at, updated_at
            FROM api_keys WHERE 1=1
        """
        params: builtins.list[Any] = []
        param_idx = 1

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        if user_id:
            query += f" AND user_id = ${param_idx}"
            params.append(user_id)
            param_idx += 1

        if enabled is not None:
            query += f" AND enabled = ${param_idx}"
            params.append(enabled)
            param_idx += 1

        query += " ORDER BY created_at DESC"

        rows = await self.fetch(query, *params)
        return [self._row_to_dict(row) for row in rows]

    async def disable(self, key_id: int) -> bool:
        """禁用 API Key"""
        result = await self.execute(
            "UPDATE api_keys SET enabled = FALSE, updated_at = NOW() WHERE id = $1", key_id
        )
        return result == "UPDATE 1"

    async def delete(self, key_id: int) -> bool:
        """删除 API Key"""
        result = await self.execute("DELETE FROM api_keys WHERE id = $1", key_id)
        return result == "DELETE 1"

    # ------------------------------------------------------------------
    # Usage tracking
    # ------------------------------------------------------------------

    async def _track_api_key_usage(self, key_hash: str) -> None:
        """Update API key last-used timestamp and use count."""
        await self.execute(
            """
                UPDATE api_keys SET
                    last_used_at = NOW(),
                    use_count = use_count + 1
                WHERE key_hash = $1
            """,
            key_hash,
        )
