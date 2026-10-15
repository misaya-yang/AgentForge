"""
数据库存储层单元测试

测试 DatabaseStorage 类的核心功能，特别是 JSON 字段处理。
"""

from __future__ import annotations

import json
from datetime import datetime
from unittest.mock import AsyncMock, MagicMock

import ai_gateway_core.persistence.database as database_module
import pytest

from src.persistence.database import DatabaseStorage


class TestRowToDict:
    """测试 _row_to_dict 方法的 JSON 字段处理"""

    def setup_method(self):
        """每个测试前创建 DatabaseStorage 实例"""
        self.db = DatabaseStorage.__new__(DatabaseStorage)
        self.db._pool = None

    def test_history_as_list_from_string(self):
        """测试 history 字段从 JSON 字符串正确解析为列表"""
        history_data = [
            {"role": "user", "content": "Hello", "timestamp": "2024-01-01T10:00:00"},
            {"role": "assistant", "content": "Hi there!", "timestamp": "2024-01-01T10:00:05"},
        ]
        row = {"session_id": "test-123", "history": json.dumps(history_data)}

        result = self.db._row_to_dict(row)

        assert isinstance(result["history"], list)
        assert len(result["history"]) == 2
        assert result["history"][0]["role"] == "user"
        assert result["history"][1]["content"] == "Hi there!"

    def test_history_as_list_from_list(self):
        """测试 history 字段当已经是列表时保持不变"""
        history_data = [
            {"role": "user", "content": "Test message"},
        ]
        row = {"session_id": "test-456", "history": history_data}

        result = self.db._row_to_dict(row)

        assert isinstance(result["history"], list)
        assert result["history"] == history_data

    def test_history_empty_list_on_parse_error(self):
        """测试 history 字段解析失败时返回空列表（而非空字典）"""
        row = {"session_id": "test-789", "history": "invalid json {"}

        result = self.db._row_to_dict(row)

        assert isinstance(result["history"], list)
        assert result["history"] == []

    def test_history_empty_list_when_not_list(self):
        """测试 history 字段为非列表类型时返回空列表"""
        row = {"session_id": "test-abc", "history": {"wrong": "type"}}

        result = self.db._row_to_dict(row)

        assert isinstance(result["history"], list)
        assert result["history"] == []

    def test_history_none_preserved(self):
        """测试 history 字段为 None 时保持 None"""
        row = {"session_id": "test-def", "history": None}

        result = self.db._row_to_dict(row)

        assert result["history"] is None

    def test_metadata_as_dict(self):
        """测试 metadata 字段正确解析为字典"""
        metadata = {"title": "Test Session", "tags": ["test"]}
        row = {"session_id": "test-ghi", "metadata": json.dumps(metadata)}

        result = self.db._row_to_dict(row)

        assert isinstance(result["metadata"], dict)
        assert result["metadata"]["title"] == "Test Session"

    def test_include_patterns_as_list(self):
        """测试 include_patterns 字段正确解析为列表"""
        patterns = ["page:123", "page:456"]
        row = {"binding_id": "bind-123", "include_patterns": json.dumps(patterns)}

        result = self.db._row_to_dict(row)

        assert isinstance(result["include_patterns"], list)
        assert result["include_patterns"] == patterns

    def test_labels_as_list(self):
        """测试 labels 字段正确解析为列表"""
        labels = ["important", "reviewed"]
        row = {"page_id": "page-123", "labels": json.dumps(labels)}

        result = self.db._row_to_dict(row)

        assert isinstance(result["labels"], list)
        assert result["labels"] == labels

    def test_datetime_to_isoformat(self):
        """测试 datetime 字段转换为 ISO 格式字符串"""
        now = datetime(2024, 1, 15, 10, 30, 0)
        row = {"id": "123", "created_at": now}

        result = self.db._row_to_dict(row)

        assert result["created_at"] == "2024-01-15T10:30:00"

    def test_empty_row_returns_empty_dict(self):
        """测试空行返回空字典"""
        result = self.db._row_to_dict(None)
        assert result == {}

        result = self.db._row_to_dict({})
        assert result == {}


class TestAppendSessionMessage:
    """测试 append_session_message 方法"""

    @pytest.mark.asyncio
    async def test_append_message_atomic(self):
        """测试消息原子追加操作"""
        db = DatabaseStorage.__new__(DatabaseStorage)
        mock_pool = AsyncMock()
        mock_conn = AsyncMock()
        mock_conn.execute = AsyncMock(return_value="UPDATE 1")
        mock_pool.acquire = MagicMock(
            return_value=AsyncMock(
                __aenter__=AsyncMock(return_value=mock_conn), __aexit__=AsyncMock()
            )
        )
        db._pool = mock_pool

        message = {
            "role": "user",
            "content": "Hello",
            "timestamp": "2024-01-15T10:00:00",
        }

        result = await db.append_session_message("session-123", message)

        assert result is True
        mock_conn.execute.assert_called_once()
        call_args = mock_conn.execute.call_args
        assert "history = history || $2::jsonb" in call_args[0][0]

    @pytest.mark.asyncio
    async def test_append_message_with_metadata_update(self):
        """测试带 metadata 更新的消息追加"""
        db = DatabaseStorage.__new__(DatabaseStorage)
        mock_pool = AsyncMock()
        mock_conn = AsyncMock()
        mock_conn.execute = AsyncMock(return_value="UPDATE 1")
        mock_pool.acquire = MagicMock(
            return_value=AsyncMock(
                __aenter__=AsyncMock(return_value=mock_conn), __aexit__=AsyncMock()
            )
        )
        db._pool = mock_pool

        message = {"role": "user", "content": "First message"}
        metadata_update = {"title": "First message"}

        result = await db.append_session_message("session-456", message, metadata_update)

        assert result is True
        call_args = mock_conn.execute.call_args
        assert "metadata = metadata || $3::jsonb" in call_args[0][0]

    @pytest.mark.asyncio
    async def test_append_message_no_pool_returns_false(self):
        """测试无连接池时返回 False"""
        db = DatabaseStorage.__new__(DatabaseStorage)
        db._pool = None

        result = await db.append_session_message("session-789", {"role": "user"})

        assert result is False


class TestBootstrapAdminPassword:
    """The generated local admin secret is applied once, never overwritten."""

    @pytest.mark.asyncio
    async def test_sets_only_an_empty_bootstrap_admin_hash(self):
        db = DatabaseStorage.__new__(DatabaseStorage)
        mock_conn = AsyncMock()
        mock_pool = AsyncMock()
        mock_pool.acquire = MagicMock(
            return_value=AsyncMock(
                __aenter__=AsyncMock(return_value=mock_conn), __aexit__=AsyncMock()
            )
        )
        db._pool = mock_pool
        db._bootstrap_admin_password_hash = "$2b$12$generated-local-hash"

        await db._ensure_bootstrap_admin_password_hash()

        mock_conn.execute.assert_awaited_once()
        query, password_hash = mock_conn.execute.await_args.args
        assert "WHERE user_id = 'admin'" in query
        assert "password_hash IS NULL OR password_hash = ''" in query
        assert password_hash == db._bootstrap_admin_password_hash

    @pytest.mark.asyncio
    async def test_skips_when_no_generated_hash_is_configured(self):
        db = DatabaseStorage.__new__(DatabaseStorage)
        db._pool = AsyncMock()
        db._bootstrap_admin_password_hash = None

        await db._ensure_bootstrap_admin_password_hash()

        db._pool.acquire.assert_not_called()

    @pytest.mark.asyncio
    async def test_account_migration_detects_missing_seed_rows(self):
        db = DatabaseStorage.__new__(DatabaseStorage)
        mock_conn = AsyncMock()
        mock_conn.fetchval = AsyncMock(side_effect=[1, "permissions", None, None])
        mock_pool = AsyncMock()
        mock_pool.acquire = MagicMock(
            return_value=AsyncMock(
                __aenter__=AsyncMock(return_value=mock_conn), __aexit__=AsyncMock()
            )
        )
        db._pool = mock_pool

        assert await db._account_permission_schema_missing() is True

    @pytest.mark.asyncio
    async def test_account_migration_accepts_complete_schema_and_seeds(self):
        db = DatabaseStorage.__new__(DatabaseStorage)
        mock_conn = AsyncMock()
        mock_conn.fetchval = AsyncMock(side_effect=[1, "permissions", 1, 1])
        mock_pool = AsyncMock()
        mock_pool.acquire = MagicMock(
            return_value=AsyncMock(
                __aenter__=AsyncMock(return_value=mock_conn), __aexit__=AsyncMock()
            )
        )
        db._pool = mock_pool

        assert await db._account_permission_schema_missing() is False

    @pytest.mark.asyncio
    async def test_platform_admin_check_requires_role_and_strict_bootstrap_assignment(self):
        db = DatabaseStorage.__new__(DatabaseStorage)
        mock_conn = AsyncMock()
        mock_conn.fetchval = AsyncMock(side_effect=[1, None])
        mock_pool = AsyncMock()
        mock_pool.acquire = MagicMock(
            return_value=AsyncMock(
                __aenter__=AsyncMock(return_value=mock_conn), __aexit__=AsyncMock()
            )
        )
        db._pool = mock_pool

        assert await db._platform_admin_schema_missing() is True
        assignment_query = mock_conn.fetchval.await_args_list[1].args[0]
        assert "u.user_id = 'admin'" in assignment_query
        assert "u.created_by = 'system'" in assignment_query
        assert "ur.role_name = 'platform_admin'" in assignment_query


@pytest.mark.asyncio
async def test_connect_rejects_retired_auto_init_before_opening_pool(monkeypatch):
    db = DatabaseStorage.__new__(DatabaseStorage)
    db.enabled = True
    db.dsn = "postgresql://unused"
    db._pool_min_size = 1
    db._pool_max_size = 1
    db._command_timeout_s = 30
    db._api_key_usage_flush_interval_seconds = 0
    db._api_key_usage_flush_batch_size = 0
    db._api_key_usage_task = None
    db.auto_init = True

    fake_pool = AsyncMock()
    monkeypatch.setattr(database_module.asyncpg, "create_pool", AsyncMock(return_value=fake_pool))

    with pytest.raises(RuntimeError, match="database.authority migrate"):
        await db.connect()

    database_module.asyncpg.create_pool.assert_not_awaited()


@pytest.mark.asyncio
async def test_connect_keeps_update_only_password_bootstrap_separate_from_schema(
    monkeypatch,
):
    db = DatabaseStorage.__new__(DatabaseStorage)
    db.enabled = True
    db.dsn = "postgresql://unused"
    db._pool_min_size = 1
    db._pool_max_size = 1
    db._command_timeout_s = 30
    db._api_key_usage_flush_interval_seconds = 0
    db._api_key_usage_flush_batch_size = 0
    db._api_key_usage_task = None
    db.auto_init = False
    db._bootstrap_admin_password_hash = "already-hashed"
    db._ensure_bootstrap_admin_password_hash = AsyncMock()

    fake_pool = AsyncMock()
    monkeypatch.setattr(
        database_module.asyncpg,
        "create_pool",
        AsyncMock(return_value=fake_pool),
    )

    await db.connect()

    db._ensure_bootstrap_admin_password_hash.assert_awaited_once_with()


@pytest.mark.asyncio
async def test_schema_check_failure_uses_bounded_exception_logger(monkeypatch):
    db = DatabaseStorage.__new__(DatabaseStorage)
    db._pool = object()

    async def fail_schema_check():
        raise OSError("database unavailable")

    db._schema_is_missing = fail_schema_check
    recorded: list[tuple[str, type[BaseException]]] = []
    monkeypatch.setattr(
        database_module,
        "record_internal_exception",
        lambda _logger, event, exc: recorded.append((event, type(exc))),
    )

    await db._auto_initialize_schema()

    assert recorded == [("assistant.database.schema_state_check_failed", OSError)]


class TestUpdateSessionPatchFields:
    """测试 session config/metadata 原子 patch 方法"""

    @pytest.mark.asyncio
    async def test_update_session_metadata_atomic_patch(self):
        db = DatabaseStorage.__new__(DatabaseStorage)
        mock_pool = AsyncMock()
        mock_conn = AsyncMock()
        mock_conn.execute = AsyncMock(return_value="UPDATE 1")
        mock_pool.acquire = MagicMock(
            return_value=AsyncMock(
                __aenter__=AsyncMock(return_value=mock_conn), __aexit__=AsyncMock()
            )
        )
        db._pool = mock_pool

        result = await db.update_session_metadata("session-123", {"title": "new title"})

        assert result is True
        call_args = mock_conn.execute.call_args
        assert "metadata = COALESCE(metadata, '{}'::jsonb) || $2::jsonb" in call_args[0][0]

    @pytest.mark.asyncio
    async def test_update_session_config_atomic_patch(self):
        db = DatabaseStorage.__new__(DatabaseStorage)
        mock_pool = AsyncMock()
        mock_conn = AsyncMock()
        mock_conn.execute = AsyncMock(return_value="UPDATE 1")
        mock_pool.acquire = MagicMock(
            return_value=AsyncMock(
                __aenter__=AsyncMock(return_value=mock_conn), __aexit__=AsyncMock()
            )
        )
        db._pool = mock_pool

        result = await db.update_session_config("session-456", {"selected_datasets": ["kb_1"]})

        assert result is True
        call_args = mock_conn.execute.call_args
        assert "config = COALESCE(config, '{}'::jsonb) || $2::jsonb" in call_args[0][0]
