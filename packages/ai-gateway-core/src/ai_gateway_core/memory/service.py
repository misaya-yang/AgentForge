"""
Memory Service for Agent.

Provides structured long-term memory (User Level) and persistent session memory (Session Level).
Implements the 3-layer memory architecture:
1. Working Memory (in-context, handled by AssistantService)
2. Session Memory (persistent key-value for current task/session)
3. User Memory (long-term preferences and facts)
"""

from __future__ import annotations

import contextlib
import json
from datetime import datetime
from typing import TYPE_CHECKING, Any

from ai_gateway_core.logging import get_logger, record_internal_exception

if TYPE_CHECKING:
    from ai_gateway_core.persistence import DatabaseStorageLike

logger = get_logger(__name__)


class MemoryService:
    def __init__(self, database: DatabaseStorageLike):
        self.database = database

    @staticmethod
    def _extract_extended_memory_metadata(
        key: str,
        metadata: dict[str, Any] | None,
    ) -> tuple[str, datetime | None, str, str, dict[str, Any] | None]:
        data = dict(metadata or {})
        namespace = str(data.get("namespace") or "default")
        if "/" in key:
            namespace = str(key.split("/", 1)[0]) or namespace

        sensitivity = str(data.get("sensitivity") or "normal")
        source = str(data.get("source") or "assistant")

        expires_at = None
        raw_expires_at = data.get("expires_at")
        if isinstance(raw_expires_at, datetime):
            expires_at = raw_expires_at
        elif isinstance(raw_expires_at, str) and raw_expires_at:
            normalized = raw_expires_at.replace("Z", "+00:00")
            with contextlib.suppress(ValueError):
                expires_at = datetime.fromisoformat(normalized)

        data["namespace"] = namespace
        data["sensitivity"] = sensitivity
        data["source"] = source
        if expires_at:
            data["expires_at"] = expires_at.isoformat()

        return namespace, expires_at, sensitivity, source, data or None

    @staticmethod
    def _normalize_memory_value(value: Any) -> str:
        """Normalize memory value to JSON string for JSONB columns."""
        if not isinstance(value, (dict, list, str, int, float, bool, type(None))):
            value = str(value)
        return json.dumps(value)

    # =========================================================================
    # User Memory (Long-term)
    # =========================================================================

    async def set_user_memory(
        self,
        tenant_id: str,
        user_id: str,
        key: str,
        value: Any,
        metadata: dict[str, Any] | None = None,
    ) -> bool:
        """
        Set a user memory key-value pair.
        Upserts if key already exists.

        Args:
            tenant_id: Tenant ID for multi-tenant isolation (required)
            user_id: User ID
            key: Memory key
            value: Value to store
            metadata: Optional metadata
        """
        query = """
            INSERT INTO user_memory (
                tenant_id, user_id, key, value, metadata,
                namespace, expires_at, sensitivity, source, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())
            ON CONFLICT (tenant_id, user_id, key)
            DO UPDATE SET
                value = EXCLUDED.value,
                metadata = COALESCE(EXCLUDED.metadata, user_memory.metadata),
                namespace = EXCLUDED.namespace,
                expires_at = COALESCE(EXCLUDED.expires_at, user_memory.expires_at),
                sensitivity = EXCLUDED.sensitivity,
                source = EXCLUDED.source,
                updated_at = NOW();
        """
        try:
            namespace, expires_at, sensitivity, source, extended_metadata = (
                self._extract_extended_memory_metadata(key, metadata)
            )

            await self.database.execute(
                query,
                tenant_id,
                user_id,
                key,
                self._normalize_memory_value(value),
                json.dumps(extended_metadata) if extended_metadata else None,
                namespace,
                expires_at,
                sensitivity,
                source,
            )
            return True
        except Exception as exc:
            record_internal_exception(logger, "assistant.memory.set_user.internal_failure", exc)
            return False

    async def get_user_memory(
        self,
        tenant_id: str,
        user_id: str,
        key: str,
    ) -> Any | None:
        """
        Get a specific user memory value.
        Updates access count and last_accessed_at.

        Args:
            tenant_id: Tenant ID for multi-tenant isolation (required)
            user_id: User ID
            key: Memory key
        """
        # First get the value with tenant isolation
        query = """
            SELECT value FROM user_memory
            WHERE tenant_id = $1 AND user_id = $2 AND key = $3
              AND (expires_at IS NULL OR expires_at > NOW())
        """
        result = await self.database.fetchrow(query, tenant_id, user_id, key)

        if result:
            # Async update access stats (fire and forget pattern ideally, but here simple await)
            update_query = """
                UPDATE user_memory
                SET access_count = access_count + 1, last_accessed_at = NOW()
                WHERE tenant_id = $1 AND user_id = $2 AND key = $3
            """
            await self.database.execute(update_query, tenant_id, user_id, key)

            value = result.get("value")
            # If the driver returns JSON string, parse it. If it returns dict, use it.
            if isinstance(value, str):
                try:
                    return json.loads(value)
                except (json.JSONDecodeError, ValueError):
                    return value
            return value

        return None

    async def list_user_memories(
        self,
        tenant_id: str,
        user_id: str,
        limit: int = 100,
    ) -> dict[str, Any]:
        """
        List all memories for a user.
        Returns a dictionary of key-value pairs.

        Args:
            tenant_id: Tenant ID for multi-tenant isolation (required)
            user_id: User ID
            limit: Maximum number of memories to return
        """
        query = """
            SELECT key, value FROM user_memory
            WHERE tenant_id = $1 AND user_id = $2
              AND (expires_at IS NULL OR expires_at > NOW())
            ORDER BY last_accessed_at DESC NULLS LAST, created_at DESC
            LIMIT $3
        """
        rows = await self.database.fetch(query, tenant_id, user_id, limit)

        memories = {}
        for row in rows:
            val = row.get("value")
            if isinstance(val, str):
                with contextlib.suppress(json.JSONDecodeError, ValueError):
                    val = json.loads(val)
            memories[row.get("key")] = val

        return memories

    async def delete_user_memory(self, tenant_id: str, user_id: str, key: str) -> bool:
        """
        Delete a user memory item.

        Args:
            tenant_id: Tenant ID for multi-tenant isolation (required)
            user_id: User ID
            key: Memory key

        Returns:
            True if the operation succeeded (regardless of whether item existed),
            False if an error occurred.
        """
        try:
            query = "DELETE FROM user_memory WHERE tenant_id = $1 AND user_id = $2 AND key = $3"
            await self.database.execute(query, tenant_id, user_id, key)
            return True
        except Exception as exc:
            record_internal_exception(logger, "assistant.memory.delete_user.internal_failure", exc)
            return False

    # =========================================================================
    # Session Memory (Context/Task Level)
    # =========================================================================

    async def set_session_memory(
        self,
        tenant_id: str,
        session_id: str,
        key: str,
        value: Any,
        metadata: dict[str, Any] | None = None,
    ) -> bool:
        """
        Set session memory.

        Args:
            tenant_id: Tenant ID for multi-tenant isolation (required)
            session_id: Session ID
            key: Memory key
            value: Value to store
            metadata: Optional metadata
        """
        query = """
            INSERT INTO session_memory (
                tenant_id, session_id, key, value, metadata,
                namespace, expires_at, sensitivity, source, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())
            ON CONFLICT (tenant_id, session_id, key)
            DO UPDATE SET
                value = EXCLUDED.value,
                metadata = COALESCE(EXCLUDED.metadata, session_memory.metadata),
                namespace = EXCLUDED.namespace,
                expires_at = COALESCE(EXCLUDED.expires_at, session_memory.expires_at),
                sensitivity = EXCLUDED.sensitivity,
                source = EXCLUDED.source,
                updated_at = NOW();
        """
        try:
            namespace, expires_at, sensitivity, source, extended_metadata = (
                self._extract_extended_memory_metadata(key, metadata)
            )

            await self.database.execute(
                query,
                tenant_id,
                session_id,
                key,
                self._normalize_memory_value(value),
                json.dumps(extended_metadata) if extended_metadata else None,
                namespace,
                expires_at,
                sensitivity,
                source,
            )
            return True
        except Exception as exc:
            record_internal_exception(logger, "assistant.memory.set_session.internal_failure", exc)
            return False

    async def get_session_memory(self, tenant_id: str, session_id: str, key: str) -> Any | None:
        """
        Get session memory value.

        Args:
            tenant_id: Tenant ID for multi-tenant isolation (required)
            session_id: Session ID
            key: Memory key
        """
        query = (
            "SELECT value FROM session_memory "
            "WHERE tenant_id = $1 AND session_id = $2 AND key = $3 "
            "AND (expires_at IS NULL OR expires_at > NOW())"
        )
        result = await self.database.fetchrow(query, tenant_id, session_id, key)

        if result:
            value = result.get("value")
            if isinstance(value, str):
                try:
                    return json.loads(value)
                except (json.JSONDecodeError, ValueError):
                    return value
            return value
        return None

    async def list_session_memories(self, tenant_id: str, session_id: str) -> dict[str, Any]:
        """
        List all session memories.

        Args:
            tenant_id: Tenant ID for multi-tenant isolation (required)
            session_id: Session ID
        """
        query = (
            "SELECT key, value FROM session_memory "
            "WHERE tenant_id = $1 AND session_id = $2 "
            "AND (expires_at IS NULL OR expires_at > NOW())"
        )
        rows = await self.database.fetch(query, tenant_id, session_id)

        memories = {}
        for row in rows:
            val = row.get("value")
            if isinstance(val, str):
                with contextlib.suppress(json.JSONDecodeError, ValueError):
                    val = json.loads(val)
            memories[row.get("key")] = val

        return memories

    async def delete_session_memory(self, tenant_id: str, session_id: str, key: str) -> bool:
        """
        Delete session memory item.

        Args:
            tenant_id: Tenant ID for multi-tenant isolation (required)
            session_id: Session ID
            key: Memory key

        Returns:
            True if the operation succeeded (regardless of whether item existed),
            False if an error occurred.
        """
        try:
            query = (
                "DELETE FROM session_memory WHERE tenant_id = $1 AND session_id = $2 AND key = $3"
            )
            await self.database.execute(query, tenant_id, session_id, key)
            return True
        except Exception as exc:
            record_internal_exception(logger, "assistant.memory.delete_session.internal_failure", exc)
            return False

    async def delete_all_session_memories(self, tenant_id: str, session_id: str) -> bool:
        """Delete every memory row in one tenant/session scope."""

        try:
            query = "DELETE FROM session_memory WHERE tenant_id = $1 AND session_id = $2"
            await self.database.execute(query, tenant_id, session_id)
            remaining = await self.database.fetchrow(
                "SELECT 1 AS present FROM session_memory "
                "WHERE tenant_id = $1 AND session_id = $2 LIMIT 1",
                tenant_id,
                session_id,
            )
            return remaining is None
        except Exception as exc:
            record_internal_exception(
                logger,
                "assistant.memory.delete_all_sessions.internal_failure",
                exc,
            )
            return False

    async def get_session_context(
        self,
        tenant_id: str,
        session_id: str,
    ) -> dict[str, Any] | None:
        """
        Get the full session context including all session memories.

        Returns a dictionary with:
        - conversation_summary: Previous conversation summary if compressed
        - task_state: Current task state
        - recent_context: Any other relevant session data

        Args:
            tenant_id: Tenant ID for multi-tenant isolation (required)
            session_id: The session ID

        Returns:
            Dictionary of session context, or None if no session data
        """
        # Get all session memories with tenant isolation
        all_memories = await self.list_session_memories(tenant_id, session_id)

        if not all_memories:
            return None

        # Structure the context
        context: dict[str, Any] = {
            "conversation_summary": all_memories.get("conversation_summary"),
            "task_state": all_memories.get("task_state"),
            "working_memory": all_memories.get("working_memory"),
            "last_response": all_memories.get("last_response"),
            "compressed_context": all_memories.get("compressed_context"),
        }

        # Filter out None values
        return {k: v for k, v in context.items() if v is not None} or None

    async def get_user_preferences(
        self,
        tenant_id: str,
        user_id: str,
    ) -> dict[str, Any]:
        """
        Get user preferences from long-term memory.

        Returns default preferences if none are stored.

        Args:
            tenant_id: Tenant ID for multi-tenant isolation (required)
            user_id: The user ID

        Returns:
            Dictionary of user preferences
        """
        DEFAULT_PREFERENCES = {
            "language": "zh-CN",
            "response_style": "professional",
            "preferred_tools": [],
            "default_datasets": [],
        }

        prefs = await self.get_user_memory(tenant_id=tenant_id, user_id=user_id, key="preferences")
        if prefs is None:
            return DEFAULT_PREFERENCES.copy()

        # Merge with defaults to ensure all keys exist
        merged = DEFAULT_PREFERENCES.copy()
        if isinstance(prefs, dict):
            merged.update(prefs)
        return merged

    async def get_long_term_context(
        self,
        tenant_id: str,
        user_id: str,
        limit: int = 20,
    ) -> dict[str, Any]:
        """
        Get user's long-term memory context.

        Includes preferences and frequently accessed memories.

        Args:
            tenant_id: Tenant ID for multi-tenant isolation (required)
            user_id: The user ID
            limit: Maximum number of memories to retrieve

        Returns:
            Dictionary with preferences and learned patterns
        """
        # Get preferences with tenant isolation
        preferences = await self.get_user_preferences(tenant_id, user_id)

        # Get frequently accessed memories with tenant isolation
        query = """
            SELECT key, value, metadata, access_count
            FROM user_memory
            WHERE tenant_id = $1 AND user_id = $2
              AND (expires_at IS NULL OR expires_at > NOW())
              AND key <> '__assistant_memory_control__'
            ORDER BY access_count DESC, last_accessed_at DESC NULLS LAST, updated_at DESC
            LIMIT $3
        """
        rows = await self.database.fetch(query, tenant_id, user_id, limit)

        frequent_memories = []
        for row in rows:
            key = row.get("key")
            if key == "preferences":
                continue  # Already included

            val = row.get("value")
            if isinstance(val, str):
                with contextlib.suppress(json.JSONDecodeError, ValueError):
                    val = json.loads(val)

            frequent_memories.append(
                {
                    "key": key,
                    "value": val,
                    "access_count": row.get("access_count", 0),
                }
            )

        return {
            "preferences": preferences,
            "frequent_memories": frequent_memories[:10],  # Top 10
        }
