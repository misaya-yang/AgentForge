"""Live serialization proof for the single PostgreSQL migration authority."""

from __future__ import annotations

import asyncio
import os
import signal
import uuid
from pathlib import Path
from typing import Any
from urllib.parse import quote

import asyncpg
import pytest
from dotenv import dotenv_values

from database.authority.commands import command_migrate, default_paths
from database.authority.constants import (
    MIGRATION_ADVISORY_LOCK_ID,
    MIGRATION_ADVISORY_LOCK_NAMESPACE,
)
from database.authority.discovery import (
    LEGACY_MANIFEST_NAME,
    discover_legacy_migrations,
    last_legacy_change,
)
from database.authority.manifest import load_legacy_manifest
from database.authority.runner import MigrationAuthority

ROOT = Path(__file__).resolve().parents[2]


def _postgres_config() -> dict[str, Any]:
    file_values = dotenv_values(os.environ.get("ENV_FILE") or ROOT / ".env")
    required = ("POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB", "POSTGRES_PORT")
    values = {key: os.environ.get(key) or file_values.get(key) for key in required}
    missing = [key for key, value in values.items() if not value]
    if missing:
        pytest.fail(f"local PostgreSQL test configuration missing keys: {', '.join(missing)}")
    return {
        "host": os.environ.get("POSTGRES_HOST") or file_values.get("POSTGRES_HOST") or "127.0.0.1",
        "port": int(str(values["POSTGRES_PORT"])),
        "user": str(values["POSTGRES_USER"]),
        "password": str(values["POSTGRES_PASSWORD"]),
        "database": str(values["POSTGRES_DB"]),
    }


def _dsn(config: dict[str, Any], database: str) -> str:
    user = quote(str(config["user"]), safe="")
    password = quote(str(config["password"]), safe="")
    return f"postgresql://{user}:{password}@{config['host']}:{config['port']}/{database}"


def _authority_environment(dsn: str) -> dict[str, str]:
    return {**os.environ, "AI_GATEWAY_DATABASE_MIGRATOR_DSN": dsn}


async def _start_authority_cli(dsn: str) -> asyncio.subprocess.Process:
    return await asyncio.create_subprocess_exec(
        "uv",
        "run",
        "--extra",
        "database",
        "python",
        "-m",
        "database.authority",
        "migrate",
        "--no-adoption",
        cwd=ROOT,
        env=_authority_environment(dsn),
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        start_new_session=True,
    )


async def _run_authority_cli(dsn: str) -> None:
    process = await _start_authority_cli(dsn)
    _stdout, _stderr = await process.communicate()
    assert process.returncode == 0, "authority CLI failed; output intentionally redacted"


async def _run_authority_in_process(dsn: str) -> None:
    result = await command_migrate(
        MigrationAuthority(dsn, default_paths()),
        allow_adoption=False,
        log=lambda *_args: None,
    )
    assert result.exit_code == 0


async def _prepare_real_pre_freeze_state(dsn: str) -> str:
    """Run the real post-bootstrap chain, then rewind its final ledger identity."""
    paths = default_paths()
    migrations = discover_legacy_migrations(paths.migrations_root)
    manifest = load_legacy_manifest(paths.migrations_root / LEGACY_MANIFEST_NAME)
    assert last_legacy_change(migrations) == manifest.freeze_point

    conn = await asyncpg.connect(dsn)
    try:
        await conn.execute((ROOT / "database/schema.sql").read_text(encoding="utf-8"))
        await conn.execute(
            """
            CREATE TABLE public.schema_migrations (
                filename TEXT PRIMARY KEY,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            """
        )
        await conn.executemany(
            "INSERT INTO public.schema_migrations (filename) VALUES ($1)",
            [
                (migration.path.name,)
                for migration in migrations
                if int(migration.version) < 100
            ],
        )
    finally:
        await conn.close()

    await _run_authority_in_process(dsn)

    conn = await asyncpg.connect(dsn)
    try:
        applied = await conn.fetchval(
            "SELECT count(*) FROM public.schema_migrations WHERE filename = $1",
            manifest.freeze_point,
        )
        assert applied == 1
        await conn.execute(
            "DELETE FROM public.schema_migrations WHERE filename = $1",
            manifest.freeze_point,
        )
    finally:
        await conn.close()
    return manifest.freeze_point


async def _authority_lock_count(conn: asyncpg.Connection, *, granted: bool) -> int:
    return int(
        await conn.fetchval(
            """
            SELECT count(*)
            FROM pg_locks AS lock
            JOIN pg_stat_activity AS activity ON activity.pid = lock.pid
            WHERE lock.locktype = 'advisory'
              AND lock.classid::bigint = $1
              AND lock.objid::bigint = $2
              AND lock.objsubid = 2
              AND lock.granted = $3
              AND activity.datname = current_database()
              AND activity.application_name LIKE 'ai_gateway_authority_%'
            """,
            MIGRATION_ADVISORY_LOCK_NAMESPACE,
            MIGRATION_ADVISORY_LOCK_ID,
            granted,
        )
    )


async def _wait_for_authority_lock_count(
    conn: asyncpg.Connection,
    *,
    granted: bool,
    expected: int,
) -> None:
    for _attempt in range(200):
        if await _authority_lock_count(conn, granted=granted) == expected:
            return
        await asyncio.sleep(0.025)
    actual = await _authority_lock_count(conn, granted=granted)
    pytest.fail(
        f"expected {expected} authority advisory locks with granted={granted}, got {actual}"
    )


async def _wait_for_blocked_final_change(conn: asyncpg.Connection) -> None:
    for _attempt in range(200):
        blocked = await conn.fetchval(
            """
            SELECT EXISTS (
                SELECT 1
                FROM pg_locks AS lock
                JOIN pg_stat_activity AS activity ON activity.pid = lock.pid
                WHERE lock.locktype = 'relation'
                  AND lock.relation =
                      'knowledge.kb_document_progress_events'::regclass
                  AND NOT lock.granted
                  AND activity.datname = current_database()
                  AND activity.application_name LIKE 'ai_gateway_authority_%'
            )
            """
        )
        if blocked:
            return
        await asyncio.sleep(0.025)
    pytest.fail("authority never entered the final migration's blocked table operation")


async def _assert_final_change_state(
    conn: asyncpg.Connection,
    freeze_point: str,
    *,
    applied: bool,
) -> None:
    expected = 1 if applied else 0
    assert (
        await conn.fetchval(
            "SELECT count(*) FROM public.schema_migrations WHERE filename = $1",
            freeze_point,
        )
        == expected
    )
    assert (
        await conn.fetchval(
            "SELECT to_regclass('knowledge.idx_kb_document_progress_events_dataset_created') "
            "IS NOT NULL"
        )
        is applied
    )
    assert (
        await conn.fetchval(
            "SELECT to_regprocedure('knowledge.prune_kb_document_progress_events()') IS NOT NULL"
        )
        is applied
    )
    assert (
        await conn.fetchval(
            """
            SELECT EXISTS (
                SELECT 1 FROM pg_trigger
                WHERE tgrelid = 'knowledge.kb_document_progress_events'::regclass
                  AND tgname = 'trg_kb_document_progress_retention'
                  AND NOT tgisinternal
            )
            """
        )
        is applied
    )


@pytest.mark.asyncio
async def test_two_authority_entrypoints_serialize_one_legacy_change() -> None:
    config = _postgres_config()
    database_name = f"migration_authority_lock_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database_name}"')
    dsn = _dsn(config, database_name)

    try:
        freeze_point = await _prepare_real_pre_freeze_state(dsn)
        holder = await asyncpg.connect(
            dsn,
            server_settings={"application_name": "test_migration_lock_holder"},
        )
        observer = await asyncpg.connect(dsn)
        tasks: list[asyncio.Task[None]] = []
        try:
            await holder.execute(
                "SELECT pg_advisory_lock($1::integer, $2::integer)",
                MIGRATION_ADVISORY_LOCK_NAMESPACE,
                MIGRATION_ADVISORY_LOCK_ID,
            )
            tasks = [
                asyncio.create_task(_run_authority_in_process(dsn)),
                asyncio.create_task(_run_authority_cli(dsn)),
            ]
            await _wait_for_authority_lock_count(
                observer,
                granted=False,
                expected=2,
            )
            await holder.execute(
                "SELECT pg_advisory_unlock($1::integer, $2::integer)",
                MIGRATION_ADVISORY_LOCK_NAMESPACE,
                MIGRATION_ADVISORY_LOCK_ID,
            )
            await asyncio.gather(*tasks)
            await _wait_for_authority_lock_count(observer, granted=True, expected=0)
            await _assert_final_change_state(observer, freeze_point, applied=True)
        finally:
            await holder.execute(
                "SELECT pg_advisory_unlock($1::integer, $2::integer)",
                MIGRATION_ADVISORY_LOCK_NAMESPACE,
                MIGRATION_ADVISORY_LOCK_ID,
            )
            for task in tasks:
                if not task.done():
                    task.cancel()
            if tasks:
                await asyncio.gather(*tasks, return_exceptions=True)
            await observer.close()
            await holder.close()
    finally:
        await admin.execute(f'DROP DATABASE "{database_name}" WITH (FORCE)')
        await admin.close()


@pytest.mark.parametrize("signal_number", (signal.SIGTERM, signal.SIGKILL))
@pytest.mark.asyncio
async def test_authority_cli_crash_releases_lock_without_ghost_change(
    signal_number: signal.Signals,
) -> None:
    config = _postgres_config()
    database_name = f"migration_authority_crash_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database_name}"')
    dsn = _dsn(config, database_name)

    try:
        freeze_point = await _prepare_real_pre_freeze_state(dsn)
        blocker = await asyncpg.connect(dsn)
        observer = await asyncpg.connect(dsn)
        process: asyncio.subprocess.Process | None = None
        communicate_task: asyncio.Task[tuple[bytes, bytes]] | None = None
        try:
            await observer.execute(
                "DROP TRIGGER IF EXISTS trg_kb_document_progress_retention "
                "ON knowledge.kb_document_progress_events"
            )
            await observer.execute(
                "DROP FUNCTION IF EXISTS knowledge.prune_kb_document_progress_events()"
            )
            await observer.execute(
                "DROP INDEX IF EXISTS knowledge.idx_kb_document_progress_events_dataset_created"
            )
            await _assert_final_change_state(observer, freeze_point, applied=False)

            await blocker.execute("BEGIN")
            await blocker.execute(
                "LOCK TABLE knowledge.kb_document_progress_events IN ACCESS EXCLUSIVE MODE"
            )
            process = await _start_authority_cli(dsn)
            communicate_task = asyncio.create_task(process.communicate())
            await _wait_for_authority_lock_count(observer, granted=True, expected=1)
            await _wait_for_blocked_final_change(observer)

            os.killpg(process.pid, signal_number)
            await asyncio.wait_for(communicate_task, timeout=10)
            await _wait_for_authority_lock_count(observer, granted=True, expected=0)
            await _assert_final_change_state(observer, freeze_point, applied=False)

            await blocker.execute("ROLLBACK")
            await _run_authority_in_process(dsn)
            await _assert_final_change_state(observer, freeze_point, applied=True)
        finally:
            if process is not None and process.returncode is None:
                os.killpg(process.pid, signal.SIGKILL)
            if communicate_task is not None and not communicate_task.done():
                await asyncio.gather(communicate_task, return_exceptions=True)
            await blocker.execute("ROLLBACK")
            await observer.close()
            await blocker.close()
    finally:
        await admin.execute(f'DROP DATABASE "{database_name}" WITH (FORCE)')
        await admin.close()
