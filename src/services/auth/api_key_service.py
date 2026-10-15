"""
API Key 服务

提供 API Key 的生成、验证、管理功能。
用于服务间集成，如 LangGraph 访问知识库。
"""

import hashlib
import secrets
from datetime import datetime, timezone
from typing import Any

from ai_gateway_core.logging import get_logger

from ...persistence.database import DatabaseStorage

logger = get_logger(__name__)


def generate_api_key() -> tuple[str, str, str]:
    """
    生成 API Key

    Returns:
        tuple: (full_key, key_id, key_prefix)
        - full_key: 完整的 API Key，只在创建时返回一次
        - key_id: 唯一标识符，用于管理
        - key_prefix: 前缀，用于识别
    """
    # 生成 32 字节随机数 (64 hex chars)
    random_bytes = secrets.token_hex(32)

    # key_id: ak_开头的短ID用于管理
    key_id = f"ak_{secrets.token_hex(12)}"

    # 完整的 API Key: agk_xxx 格式
    full_key = f"agk_{random_bytes}"

    # 前缀用于显示
    key_prefix = full_key[:12]

    return full_key, key_id, key_prefix


def hash_api_key(api_key: str) -> str:
    """哈希 API Key"""
    return hashlib.sha256(api_key.encode()).hexdigest()


class APIKeyService:
    """API Key 管理服务"""

    def __init__(self, database: DatabaseStorage):
        self.db = database
        self._scope_column: str | None = None

    @staticmethod
    def _public_key_id_sql(alias: str = "") -> str:
        """Stable public key_id expression compatible with legacy rows."""
        prefix = f"{alias}." if alias else ""
        return f"COALESCE({prefix}key_id, ('ak_legacy_' || {prefix}id::text))"

    async def _api_key_scope_column(self) -> str:
        """Return the API key permission column exposed as response `scopes`."""
        if self._scope_column is not None:
            return self._scope_column

        row = await self.db.fetchrow(
            """
            SELECT column_name
            FROM information_schema.columns
            WHERE table_schema = ANY (current_schemas(FALSE))
              AND table_name = 'api_keys'
              AND column_name IN ('scopes', 'permissions')
            ORDER BY CASE column_name WHEN 'scopes' THEN 0 ELSE 1 END
            LIMIT 1
            """
        )
        column = row["column_name"] if row else None
        self._scope_column = column if column in {"scopes", "permissions"} else "scopes"
        return self._scope_column

    async def create_api_key(
        self,
        name: str,
        user_id: str,  # VARCHAR in existing schema
        tenant_id: str = "",
        description: str = "",
        scopes: list[str] | None = None,
        roles: list[str] | None = None,
        tier: str = "normal",
        rate_limit: dict | None = None,
        expires_at: datetime | None = None,
    ) -> dict[str, Any]:
        """
        创建 API Key

        注意：返回的 api_key 只会显示一次，之后无法获取
        """
        full_key, key_id, key_prefix = generate_api_key()
        key_hash = hash_api_key(full_key)

        if scopes is None:
            scopes = ["knowledge:read", "knowledge:write"]
        if roles is None:
            roles = ["user"]

        scope_column = await self._api_key_scope_column()
        query = f"""
            INSERT INTO api_keys (
                key_id, key_hash, key_prefix, name, description,
                user_id, tenant_id, {scope_column}, roles, tier, rate_limit, expires_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12
            )
            RETURNING id, key_id, key_prefix, name, description,
                      {scope_column} AS scopes, roles, tier, rate_limit,
                      created_at, expires_at, enabled
        """

        row = await self.db.fetchrow(
            query,
            key_id,
            key_hash,
            key_prefix,
            name,
            description,
            str(user_id),
            tenant_id,
            scopes,
            roles,
            tier,
            rate_limit,
            expires_at,
        )

        result = dict(row)
        result["is_active"] = result.pop("enabled", True)
        # 只在创建时返回完整的 API Key
        result["api_key"] = full_key
        result["warning"] = "请妥善保存此 API Key，它只会显示一次"

        logger.info(f"Created API key: {key_id} for user {user_id}")
        return result

    async def validate_api_key(self, api_key: str) -> dict[str, Any] | None:
        """
        验证 API Key

        Returns:
            验证成功返回 key 信息，失败返回 None
        """
        if not api_key or not api_key.startswith("agk_"):
            return None

        key_hash = hash_api_key(api_key)

        public_key_id_expr = self._public_key_id_sql("ak")
        scope_column = await self._api_key_scope_column()
        query = f"""
            SELECT
                ak.id,
                {public_key_id_expr} AS key_id,
                ak.name, ak.user_id, ak.tenant_id,
                ak.{scope_column} AS scopes, ak.roles, ak.tier, ak.rate_limit,
                ak.enabled, ak.expires_at
            FROM api_keys ak
            WHERE ak.key_hash = $1
        """

        row = await self.db.fetchrow(query, key_hash)

        if not row:
            logger.warning(f"Invalid API key attempted: {api_key[:12]}...")
            return None

        # 检查状态
        if not row["enabled"]:
            logger.warning(f"Disabled API key used: {row['key_id']}")
            return None

        # 检查过期
        if row["expires_at"] and row["expires_at"] < datetime.now(timezone.utc):
            logger.warning(f"Expired API key used: {row['key_id']}")
            return None

        # 更新使用记录
        await self._update_usage(row["id"])

        return {
            "key_id": row["key_id"],
            "name": row["name"],
            "user_id": str(row["user_id"]),
            "tenant_id": row["tenant_id"] or "",
            "scopes": row["scopes"] or [],
            "tier": row["tier"] or "normal",
            "roles": row["roles"] or ["user"],
            "rate_limit": row["rate_limit"],
        }

    async def _update_usage(self, key_db_id: int):
        """更新使用记录"""
        query = """
            UPDATE api_keys
            SET last_used_at = NOW(), use_count = use_count + 1
            WHERE id = $1
            RETURNING id
        """
        await self.db.fetchrow(query, key_db_id)

    async def list_api_keys(
        self,
        user_id: str | None = None,
        tenant_id: str | None = None,
        include_inactive: bool = False,
    ) -> list[dict[str, Any]]:
        """列出 API Keys（不返回实际的 key）"""
        conditions = []
        params = []
        param_idx = 1

        if user_id:
            conditions.append(f"user_id = ${param_idx}")
            params.append(str(user_id))
            param_idx += 1

        if tenant_id:
            conditions.append(
                f"COALESCE(NULLIF(tenant_id, ''), 'default') = ${param_idx}"
            )
            params.append(tenant_id)
            param_idx += 1

        if not include_inactive:
            conditions.append("enabled = TRUE")

        where_clause = " AND ".join(conditions) if conditions else "1=1"

        scope_column = await self._api_key_scope_column()
        query = f"""
            SELECT
                {self._public_key_id_sql()} AS key_id,
                key_prefix, name, description,
                user_id, tenant_id, {scope_column} AS scopes, roles, tier,
                rate_limit, enabled as is_active, created_at,
                ('apikey:' || SUBSTRING(key_hash, 1, 16)) AS derived_user_id,
                last_used_at, expires_at, use_count
            FROM api_keys
            WHERE {where_clause}
            ORDER BY created_at DESC
        """

        rows = await self.db.fetch(query, *params)
        return [dict(row) for row in rows]

    async def get_api_key(
        self,
        key_id: str,
        tenant_id: str | None = None,
    ) -> dict[str, Any] | None:
        """获取 API Key 详情，可选限制到单个租户。"""
        public_key_id_expr = self._public_key_id_sql()
        scope_column = await self._api_key_scope_column()
        tenant_clause = ""
        params: list[Any] = [key_id]
        if tenant_id:
            tenant_clause = " AND COALESCE(NULLIF(tenant_id, ''), 'default') = $2"
            params.append(tenant_id)
        query = f"""
            SELECT
                {public_key_id_expr} AS key_id,
                key_prefix, name, description,
                user_id, tenant_id, {scope_column} AS scopes, roles, tier,
                rate_limit, enabled as is_active,
                ('apikey:' || SUBSTRING(key_hash, 1, 16)) AS derived_user_id,
                created_at, last_used_at, expires_at, use_count
            FROM api_keys
            WHERE {public_key_id_expr} = $1
              {tenant_clause}
        """

        row = await self.db.fetchrow(query, *params)
        return dict(row) if row else None

    async def revoke_api_key(
        self,
        key_id: str,
        user_id: str | None = None,
        tenant_id: str | None = None,
    ) -> bool:
        """吊销 API Key（设置 enabled=FALSE）"""
        public_key_id_expr = self._public_key_id_sql()
        conditions = [f"{public_key_id_expr} = $1"]
        params = [key_id]

        if user_id:
            conditions.append(f"user_id = ${len(params) + 1}")
            params.append(str(user_id))
        if tenant_id:
            conditions.append(
                f"COALESCE(NULLIF(tenant_id, ''), 'default') = ${len(params) + 1}"
            )
            params.append(tenant_id)

        query = f"""
            UPDATE api_keys
            SET enabled = FALSE, updated_at = NOW()
            WHERE {" AND ".join(conditions)}
            RETURNING {public_key_id_expr} AS key_id
        """

        row = await self.db.fetchrow(query, *params)
        if row:
            logger.info(f"Revoked API key: {key_id}")
            return True
        return False

    async def delete_api_key(
        self,
        key_id: str,
        user_id: str | None = None,
        tenant_id: str | None = None,
    ) -> bool:
        """删除 API Key"""
        public_key_id_expr = self._public_key_id_sql()
        conditions = [f"{public_key_id_expr} = $1"]
        params = [key_id]

        if user_id:
            conditions.append(f"user_id = ${len(params) + 1}")
            params.append(str(user_id))
        if tenant_id:
            conditions.append(
                f"COALESCE(NULLIF(tenant_id, ''), 'default') = ${len(params) + 1}"
            )
            params.append(tenant_id)

        query = f"""
            DELETE FROM api_keys
            WHERE {" AND ".join(conditions)}
            RETURNING {public_key_id_expr} AS key_id
        """

        row = await self.db.fetchrow(query, *params)
        if row:
            logger.info(f"Deleted API key: {key_id}")
            return True
        return False
