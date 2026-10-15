"""Real Gateway startup fails before shared storage bootstrap on schema rejection."""

from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from types import SimpleNamespace
from unittest.mock import AsyncMock

import ai_gateway_core.persistence.database as core_database
import pytest
from ai_gateway_contracts.database_revision import DATABASE_BASELINE_ID

from database.authority.runner import AuthorityError, MigrationAuthority
from src.container import Container
from src.persistence import schema_compatibility
from src.services.health_contract import gateway_database_is_ready


class SchemaConnection:
    def __init__(self, events: list[str], *, sequence: int = 10) -> None:
        self.events = events
        self.sequence = sequence
        self.baseline = DATABASE_BASELINE_ID
        self.missing_objects = False
        self.query_error: BaseException | None = None
        self.close = AsyncMock(side_effect=lambda: events.append("check-close"))

    async def fetchval(self, query: str, *args: object) -> object:
        assert query.lstrip().startswith("SELECT ")
        if self.query_error is not None:
            raise self.query_error
        if "max(sequence)" in query:
            assert args == (self.baseline,)
            return self.sequence
        assert "to_regclass" in query
        return not self.missing_objects or "platform_schema_baselines" in str(args)

    async def fetch(self, query: str) -> list[dict[str, str]]:
        assert "SELECT baseline_id FROM public.platform_schema_baselines" in query
        return [{"baseline_id": self.baseline}]


@pytest.fixture
def startup(monkeypatch: pytest.MonkeyPatch) -> SimpleNamespace:
    events: list[str] = []
    schema = SchemaConnection(events)
    state = SimpleNamespace(connect_error=None, pool_error=None, bootstrap_error=None)
    pool_connection = SimpleNamespace(fetchval=AsyncMock(return_value=1))

    async def bootstrap(query: str, *args: object) -> str:
        assert query.lstrip().startswith("UPDATE users")
        assert args == ("test-password-hash",)
        events.append("password-bootstrap")
        if state.bootstrap_error is not None:
            raise state.bootstrap_error
        return "UPDATE 1"

    pool_connection.execute = AsyncMock(side_effect=bootstrap)

    @asynccontextmanager
    async def acquire():
        yield pool_connection

    pool = SimpleNamespace(
        acquire=acquire,
        close=AsyncMock(side_effect=lambda: events.append("pool-close")),
    )

    async def connect(dsn: str, *, server_settings: dict[str, str]):
        assert dsn == "unused-test-dsn"
        assert server_settings["default_transaction_read_only"] == "on"
        events.append("check-connect")
        if state.connect_error is not None:
            raise state.connect_error
        return schema

    async def create_pool(dsn: str, **kwargs: object):
        assert dsn == "unused-test-dsn"
        assert schema.close.await_count == driver.connect.await_count
        events.append("pool-open")
        if state.pool_error is not None:
            raise state.pool_error
        return pool

    driver = SimpleNamespace(
        connect=AsyncMock(side_effect=connect),
        create_pool=AsyncMock(side_effect=create_pool),
        PostgresError=RuntimeError,
    )
    monkeypatch.setattr(
        schema_compatibility, "MigrationAuthority",
        lambda dsn, paths: MigrationAuthority(dsn, paths, asyncpg_module=driver),
    )
    monkeypatch.setattr(core_database, "asyncpg", driver)
    monkeypatch.setattr(core_database, "HAS_ASYNCPG", True)
    monkeypatch.delenv("DEFAULT_USER_PASSWORD", raising=False)
    settings = SimpleNamespace(
        database=SimpleNamespace(enabled=True, dsn="unused-test-dsn", auto_init=False),
        redis=SimpleNamespace(enabled=True),
        proxy=SimpleNamespace(enabled=True),
    )
    container = Container(settings)
    database = container.database
    database._bootstrap_admin_password_hash = "test-password-hash"
    redis = SimpleNamespace(
        connect=AsyncMock(), ping=AsyncMock(return_value=True), close=AsyncMock()
    )
    proxy = SimpleNamespace(refresh_all_service_health=AsyncMock(), close=AsyncMock())
    container.override("redis", redis)
    container.override("transparent_proxy", proxy)
    return SimpleNamespace(
        events=events, schema=schema, state=state, pool=pool,
        pool_connection=pool_connection, driver=driver,
        container=container, database=database, redis=redis, proxy=proxy,
    )


async def assert_no_bootstrap(startup: SimpleNamespace) -> None:
    assert startup.container._initialized is False
    assert startup.database._pool is None
    assert startup.database._api_key_usage_task is None
    assert await gateway_database_is_ready(startup.database) is False
    startup.driver.create_pool.assert_not_awaited()
    startup.pool_connection.execute.assert_not_awaited()
    startup.redis.connect.assert_not_awaited()
    startup.proxy.refresh_all_service_health.assert_not_awaited()


@pytest.mark.parametrize("sequence", [2, 10])
def test_compatible_startup_checks_schema_before_pool_and_bootstrap(startup, sequence):
    startup.schema.sequence = sequence

    async def run():
        await startup.container.initialize()
        assert startup.events == [
            "check-connect", "check-close", "pool-open", "password-bootstrap"
        ]
        assert startup.container._initialized is True
        assert startup.database._api_key_usage_task is not None
        assert await gateway_database_is_ready(startup.database) is True
        startup.redis.connect.assert_awaited_once_with()
        startup.proxy.refresh_all_service_health.assert_awaited_once_with()
        # Repeated initialization must not open another connection or bootstrap.
        await startup.container.initialize()
        startup.driver.connect.assert_awaited_once()
        startup.driver.create_pool.assert_awaited_once()
        await startup.container.shutdown()
        assert startup.database._pool is None
        assert startup.database._api_key_usage_task is None
        startup.schema.close.assert_awaited_once_with()
        startup.pool.close.assert_awaited_once_with()
        assert await gateway_database_is_ready(startup.database) is False

    asyncio.run(run())


@pytest.mark.parametrize("sequence", [1, 11, 17])
def test_incompatible_schema_aborts_before_any_application_bootstrap(startup, sequence):
    startup.schema.sequence = sequence

    async def run():
        with pytest.raises(AuthorityError, match=r"application window \(2\.\.10\)"):
            await startup.container.initialize()
        await assert_no_bootstrap(startup)
        startup.schema.close.assert_awaited_once_with()
        assert startup.events == ["check-connect", "check-close"]

    asyncio.run(run())


@pytest.mark.parametrize("failure", ["missing_objects", "wrong_baseline", "query_error"])
def test_unverifiable_schema_closes_readonly_connection_without_bootstrap(startup, failure):
    if failure == "missing_objects":
        startup.schema.missing_objects = True
    elif failure == "wrong_baseline":
        startup.schema.baseline = "unknown-baseline"
    else:
        startup.schema.query_error = OSError("schema query unavailable")

    async def run():
        with pytest.raises((AuthorityError, OSError)):
            await startup.container.initialize()
        await assert_no_bootstrap(startup)
        startup.schema.close.assert_awaited_once_with()

    asyncio.run(run())


def test_unavailable_database_aborts_without_any_pool_or_bootstrap(startup):
    startup.state.connect_error = OSError("database unavailable")

    async def run():
        with pytest.raises(OSError, match="database unavailable"):
            await startup.container.initialize()
        await assert_no_bootstrap(startup)
        startup.schema.close.assert_not_awaited()

    asyncio.run(run())


def test_pool_failure_aborts_after_successful_readonly_gate(startup):
    startup.state.pool_error = OSError("pool unavailable")

    async def run():
        with pytest.raises(OSError, match="pool unavailable"):
            await startup.container.initialize()
        assert startup.database._pool is None
        assert startup.database._api_key_usage_task is None
        assert startup.container._initialized is False
        assert await gateway_database_is_ready(startup.database) is False
        startup.pool_connection.execute.assert_not_awaited()
        startup.redis.connect.assert_not_awaited()
        startup.schema.close.assert_awaited_once_with()

    asyncio.run(run())


@pytest.mark.parametrize("cancelled", [False, True])
def test_failed_bootstrap_closes_partial_pool_and_cancels_usage_flusher(startup, cancelled):
    error = asyncio.CancelledError() if cancelled else OSError("bootstrap unavailable")
    startup.state.bootstrap_error = error

    async def run():
        with pytest.raises(type(error)):
            await startup.container.initialize()
        assert startup.container._initialized is False
        assert startup.database._pool is None
        assert startup.database._api_key_usage_task is None
        assert await gateway_database_is_ready(startup.database) is False
        startup.schema.close.assert_awaited_once_with()
        startup.pool.close.assert_awaited_once_with()
        startup.redis.connect.assert_not_awaited()
        startup.proxy.refresh_all_service_health.assert_not_awaited()
        # Cleanup already happened even though normal shutdown is skipped on
        # a container that never finished initialization.
        await startup.container.shutdown()
        startup.pool.close.assert_awaited_once_with()

    asyncio.run(run())


def test_disabled_database_does_not_require_schema_connection(startup):
    startup.container.settings.database.enabled = False

    async def run():
        await startup.container.initialize()
        assert startup.container._initialized is True
        startup.driver.connect.assert_not_awaited()
        startup.driver.create_pool.assert_not_awaited()
        startup.pool_connection.execute.assert_not_awaited()
        await startup.container.shutdown()

    asyncio.run(run())


def test_cancelled_schema_check_closes_readonly_connection_before_pool(startup):
    startup.schema.query_error = asyncio.CancelledError()

    async def run():
        with pytest.raises(asyncio.CancelledError):
            await startup.container.initialize()
        await assert_no_bootstrap(startup)
        startup.schema.close.assert_awaited_once_with()

    asyncio.run(run())


def test_retry_after_rejection_rechecks_schema_before_opening_pool(startup):
    async def run():
        startup.schema.sequence = 17
        with pytest.raises(AuthorityError):
            await startup.container.initialize()
        await assert_no_bootstrap(startup)
        startup.schema.sequence = 10
        await startup.container.initialize()
        assert startup.driver.connect.await_count == 2
        assert startup.schema.close.await_count == 2
        startup.driver.create_pool.assert_awaited_once()
        startup.pool_connection.execute.assert_awaited_once()
        assert await gateway_database_is_ready(startup.database) is True
        await startup.container.shutdown()
        assert startup.database._pool is None
        assert startup.database._api_key_usage_task is None

    asyncio.run(run())


def test_failed_readonly_connection_close_still_prevents_pool_bootstrap(startup):
    startup.schema.close.side_effect = OSError("check connection close failed")

    async def run():
        with pytest.raises(OSError, match="check connection close failed"):
            await startup.container.initialize()
        await assert_no_bootstrap(startup)
        startup.schema.close.assert_awaited_once_with()

    asyncio.run(run())
