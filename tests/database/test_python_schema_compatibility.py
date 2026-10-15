"""Python Knowledge accepts the native schema without expanding Gateway policy."""

from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

import pytest
from ai_gateway_contracts.database_revision import (
    DATABASE_BASELINE_ID,
    KNOWLEDGE_MAX_SUPPORTED_EPOCH_SEQUENCE,
    KNOWLEDGE_MIN_REQUIRED_EPOCH_SEQUENCE,
    MAX_SUPPORTED_EPOCH_SEQUENCE,
    MIN_REQUIRED_EPOCH_SEQUENCE,
)
from knowledge_service.persistence import database as knowledge_database
from knowledge_service.persistence.schema_compatibility import require_compatible_schema

from database.authority import cli, commands
from database.authority.manifest import load_epoch_manifest
from database.authority.runner import AuthorityError

ROOT = Path(__file__).resolve().parents[2]


class SchemaConnection:
    """Exercise the real read-only guards, without a PostgreSQL dependency."""

    def __init__(self, sequence: int, *, baseline: str = DATABASE_BASELINE_ID) -> None:
        self.sequence = sequence
        self.baseline = baseline
        self.close = AsyncMock()

    async def fetchval(self, query: str, *args: object) -> object:
        assert query.lstrip().startswith("SELECT ")
        if "max(sequence)" in query:
            assert args == (self.baseline,)
            return self.sequence
        assert "to_regclass" in query
        return True

    async def fetch(self, query: str) -> list[dict[str, str]]:
        assert "SELECT baseline_id FROM public.platform_schema_baselines" in query
        return [{"baseline_id": self.baseline}]


@pytest.mark.parametrize("sequence", range(5, 18))
def test_knowledge_accepts_every_reviewed_epoch(sequence: int) -> None:
    asyncio.run(require_compatible_schema(SchemaConnection(sequence)))


def test_knowledge_ceiling_matches_shipped_manifest() -> None:
    manifest = load_epoch_manifest(
        ROOT / "database" / "migrations" / DATABASE_BASELINE_ID / "manifest.yml"
    )
    assert KNOWLEDGE_MAX_SUPPORTED_EPOCH_SEQUENCE == max(c.sequence for c in manifest.changes)
    assert KNOWLEDGE_MIN_REQUIRED_EPOCH_SEQUENCE == 5
    assert (MIN_REQUIRED_EPOCH_SEQUENCE, MAX_SUPPORTED_EPOCH_SEQUENCE) == (2, 10)


@pytest.mark.parametrize("sequence", [0, 2, 4, 18])
def test_knowledge_rejects_unreviewed_epochs(sequence: int) -> None:
    with pytest.raises(RuntimeError, match="epoch is incompatible"):
        asyncio.run(require_compatible_schema(SchemaConnection(sequence)))


def test_knowledge_rejects_other_baseline() -> None:
    with pytest.raises(RuntimeError, match="baseline is incompatible"):
        asyncio.run(require_compatible_schema(SchemaConnection(17, baseline="unreviewed")))


@pytest.mark.parametrize("principal", ["knowledge_api", "knowledge_worker"])
@pytest.mark.parametrize("sequence", [5, 10, 11, 12, 13, 14, 15, 16, 17])
def test_authority_knowledge_startup_uses_reviewed_window(principal: str, sequence: int) -> None:
    conn = SchemaConnection(sequence)
    authority = SimpleNamespace(connect=AsyncMock(return_value=conn))
    assert asyncio.run(commands.command_startup_check(authority, principal=principal, log=lambda _: None)) == 0
    authority.connect.assert_awaited_once_with(read_only=True)
    conn.close.assert_awaited_once_with()


@pytest.mark.parametrize("principal", ["knowledge_api", "knowledge_worker"])
@pytest.mark.parametrize("sequence", [4, 18])
def test_authority_knowledge_startup_rejects_outside_window(principal: str, sequence: int) -> None:
    conn = SchemaConnection(sequence)
    authority = SimpleNamespace(connect=AsyncMock(return_value=conn))
    with pytest.raises(AuthorityError, match=r"application window \(5\.\.17\)"):
        asyncio.run(commands.command_startup_check(authority, principal=principal))
    conn.close.assert_awaited_once_with()


@pytest.mark.parametrize("sequence", [2, 10])
def test_authority_default_gateway_window_is_unchanged(sequence: int) -> None:
    authority = SimpleNamespace(connect=AsyncMock(return_value=SchemaConnection(sequence)))
    assert asyncio.run(commands.command_startup_check(authority, log=lambda _: None)) == 0


@pytest.mark.parametrize("principal", ["gateway", "runtime", "capability_worker"])
@pytest.mark.parametrize("sequence", [1, 11, 12, 17])
def test_authority_does_not_expand_other_principals(principal: str, sequence: int) -> None:
    conn = SchemaConnection(sequence)
    authority = SimpleNamespace(connect=AsyncMock(return_value=conn))
    with pytest.raises(AuthorityError, match=r"application window \(2\.\.10\)"):
        asyncio.run(commands.command_startup_check(authority, principal=principal))
    conn.close.assert_awaited_once_with()


def test_authority_rejects_unknown_principal_before_connect() -> None:
    authority = SimpleNamespace(connect=AsyncMock())
    with pytest.raises(AuthorityError, match="unknown application principal"):
        asyncio.run(commands.command_startup_check(authority, principal="knowledge"))
    authority.connect.assert_not_awaited()


@pytest.mark.parametrize("principal", ["gateway", "knowledge_api", "knowledge_worker"])
def test_cli_binds_role_and_window_to_same_principal(principal: str) -> None:
    authority = object()
    startup = AsyncMock(return_value=0)
    with (
        patch.object(cli, "role_prefix_from_env", return_value="test_"),
        patch.object(cli, "get_role_dsn", return_value="unused-test-dsn") as get_dsn,
        patch.object(cli, "MigrationAuthority", return_value=authority),
        patch.object(cli, "command_startup_check", startup),
    ):
        assert cli.main(["startup-check", "--principal", principal]) == 0
    get_dsn.assert_called_once_with(principal)
    startup.assert_awaited_once_with(authority, principal=principal)


@pytest.mark.parametrize("sequence,accepted", [(5, True), (17, True), (4, False), (18, False)])
def test_knowledge_pool_startup_and_failed_cleanup(sequence: int, accepted: bool) -> None:
    conn = SchemaConnection(sequence)

    @asynccontextmanager
    async def acquire():
        yield conn

    pool = SimpleNamespace(acquire=acquire, close=AsyncMock())
    driver = SimpleNamespace(create_pool=AsyncMock(return_value=pool))
    with (
        patch.object(knowledge_database, "HAS_ASYNCPG", True),
        patch.object(knowledge_database, "asyncpg", driver),
    ):
        storage = knowledge_database.DatabaseStorage(
            dsn="unused-test-dsn", enabled=True, api_key_usage_flush_interval_seconds=0
        )
        if accepted:
            asyncio.run(storage.connect())
            assert storage._pool is pool
            pool.close.assert_not_awaited()
        else:
            with pytest.raises(RuntimeError, match="epoch is incompatible"):
                asyncio.run(storage.connect())
            assert storage._pool is None
            pool.close.assert_awaited_once_with()
        assert storage._api_key_usage_task is None
