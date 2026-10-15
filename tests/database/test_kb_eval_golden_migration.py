"""Real-PostgreSQL behavior tests for the T0-#2 golden QA store.

Contract under test (PRD T0-#2: 版本钉进 Postgres, 分冻结回归集与增长集):

* migration 104 is idempotent (re-applying a deployed migration must never
  break a re-run) and self-contained — no foreign keys, so golden ground
  truth survives unrelated dataset churn;
* import is an idempotent (version, case_id) upsert: a re-import refreshes
  the row in place, preserving created_at and bumping updated_at;
* the split column is the review gate: imports default to ``growth``,
  promotion to ``frozen`` is an explicit store write, unknown ids or splits
  are refused, and the CHECK constraint holds even against raw SQL;
* kb_eval_golden_release pins which version list_cases cites when no
  version is passed, re-pinning replaces the pointer, and pinning a version
  with no rows fails closed;
* answer_aware reference_answer and JSONB relevance/metadata round-trip
  exactly through the store mapping.

Tier-b pattern: throwaway schema + migration applied VERBATIM, exercised
through KbEvalGoldenStore over a live developer PostgreSQL (located via
ENV_FILE, never printed).
"""

from __future__ import annotations

import os
import uuid
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any

import asyncpg
import pytest
import pytest_asyncio
from dotenv import dotenv_values
from knowledge_service.persistence.kb_eval_golden_store import (
    GoldenStoreError,
    KbEvalGoldenStore,
)

ROOT = Path(__file__).resolve().parents[2]
MIGRATION_104 = ROOT / "database" / "migrations" / "104_kb_eval_golden.sql"


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


def _cases(version: str = "v-golden-1") -> list[dict[str, Any]]:
    return [
        {
            "case_id": "kb.t.retrieval.en",
            "track": "retrieval_only",
            "query": "how do refunds work",
            "relevance": {"seg-a": 3, "seg-b": 1},
            "metadata": {"version": version, "provenance": "tier-b fixture"},
        },
        {
            "case_id": "kb.t.retrieval.zh",
            "track": "retrieval_only",
            "query": "退款规则是什么",
            "relevance": {"seg-a": 3, "seg-c": 1},
            "metadata": {"version": version, "language": "zh"},
        },
        {
            "case_id": "kb.t.answer.xl",
            "track": "answer_aware",
            "query": "退款规则是什么",
            "relevance": {"seg-a": 3},
            "reference_answer": "Refunds follow the published window.",
            "metadata": {"version": version, "answer_language": "en"},
        },
    ]


@pytest_asyncio.fixture
async def golden_store() -> AsyncIterator[tuple[KbEvalGoldenStore, asyncpg.Pool]]:
    config = _postgres_config()
    database_name = f"kb_eval_golden_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database_name}"')

    pool = await asyncpg.create_pool(
        **{**config, "database": database_name},
        min_size=1,
        max_size=2,
        server_settings={"search_path": "knowledge,gateway,assistant,public"},
    )
    try:
        async with pool.acquire() as conn:
            await conn.execute("CREATE SCHEMA knowledge")
            # The migration itself is under test: apply it verbatim.
            await conn.execute(MIGRATION_104.read_text())
        yield KbEvalGoldenStore(pool), pool
    finally:
        await pool.close()
        try:
            await admin.execute(f'DROP DATABASE "{database_name}"')
        finally:
            await admin.close()


@pytest.mark.asyncio
async def test_migration_104_is_idempotent(
    golden_store: tuple[KbEvalGoldenStore, asyncpg.Pool],
) -> None:
    _store, pool = golden_store
    async with pool.acquire() as conn:
        await conn.execute(MIGRATION_104.read_text())  # second application must not raise
        tables = {
            row["tablename"]
            for row in await conn.fetch(
                "SELECT tablename FROM pg_tables WHERE schemaname = current_schema()"
            )
        }
    assert {"kb_eval_golden", "kb_eval_golden_release"} <= tables


@pytest.mark.asyncio
async def test_import_round_trips_cases(
    golden_store: tuple[KbEvalGoldenStore, asyncpg.Pool],
) -> None:
    store, _pool = golden_store

    counts = await store.import_cases(_cases(), version="v-golden-1")
    assert counts == {"imported": 3, "frozen": 0, "growth": 3}
    assert await store.count_cases("v-golden-1") == 3

    listed = await store.list_cases("v-golden-1")
    assert [case["case_id"] for case in listed] == [
        "kb.t.answer.xl",
        "kb.t.retrieval.en",
        "kb.t.retrieval.zh",
    ]
    first = listed[1]
    assert first["query"] == "how do refunds work"
    assert first["relevance"] == {"seg-a": 3, "seg-b": 1}
    assert first["metadata"]["version"] == "v-golden-1"
    # The split rides back in metadata; nothing is frozen without promotion.
    assert first["metadata"]["split"] == "growth"
    answer_case = listed[0]
    assert answer_case["reference_answer"] == "Refunds follow the published window."

    # Track filtering matches the gate's track semantics.
    retrieval_only = await store.list_cases("v-golden-1", track="retrieval_only")
    assert {case["case_id"] for case in retrieval_only} == {
        "kb.t.retrieval.en",
        "kb.t.retrieval.zh",
    }


@pytest.mark.asyncio
async def test_reimport_upserts_in_place(
    golden_store: tuple[KbEvalGoldenStore, asyncpg.Pool],
) -> None:
    store, pool = golden_store

    await store.import_cases(_cases(), version="v-golden-1")
    async with pool.acquire() as conn:
        before = await conn.fetchrow(
            "SELECT created_at, updated_at FROM kb_eval_golden "
            "WHERE version = 'v-golden-1' AND case_id = 'kb.t.retrieval.en'"
        )

    updated = _cases()
    updated[0]["query"] = "how do annual refunds work"
    counts = await store.import_cases(updated, version="v-golden-1")
    assert counts["imported"] == 3
    assert await store.count_cases("v-golden-1") == 3  # refreshed, not duplicated

    relisted = await store.list_cases("v-golden-1")
    queries = {case["case_id"]: case["query"] for case in relisted}
    assert queries["kb.t.retrieval.en"] == "how do annual refunds work"

    async with pool.acquire() as conn:
        after = await conn.fetchrow(
            "SELECT created_at, updated_at FROM kb_eval_golden "
            "WHERE version = 'v-golden-1' AND case_id = 'kb.t.retrieval.en'"
        )
    assert after["created_at"] == before["created_at"]
    assert after["updated_at"] >= before["updated_at"]


@pytest.mark.asyncio
async def test_versions_are_isolated(
    golden_store: tuple[KbEvalGoldenStore, asyncpg.Pool],
) -> None:
    store, _pool = golden_store

    await store.import_cases(_cases("v-old"), version="v-old")
    await store.import_cases(_cases("v-new"), version="v-new")
    assert await store.count_cases("v-old") == 3
    assert await store.count_cases("v-new") == 3
    # An edit under one version never bleeds into the pinned history.
    edited = _cases("v-new")
    edited[0]["query"] = "changed"
    await store.import_cases(edited, version="v-new")
    old = await store.list_cases("v-old", track="retrieval_only")
    assert {case["query"] for case in old} == {"how do refunds work", "退款规则是什么"}


@pytest.mark.asyncio
async def test_frozen_promotion_is_explicit_and_validated(
    golden_store: tuple[KbEvalGoldenStore, asyncpg.Pool],
) -> None:
    store, _pool = golden_store
    await store.import_cases(_cases(), version="v-golden-1")

    promoted = await store.set_split("v-golden-1", ["kb.t.retrieval.en"], "frozen")
    assert promoted == 1
    frozen = await store.list_cases("v-golden-1", split="frozen")
    assert [case["case_id"] for case in frozen] == ["kb.t.retrieval.en"]
    assert frozen[0]["metadata"]["split"] == "frozen"
    assert await store.count_cases("v-golden-1", split="growth") == 2

    # Unknown ids fail closed and change nothing.
    with pytest.raises(GoldenStoreError, match="not in version"):
        await store.set_split("v-golden-1", ["kb.t.answer.xl", "kb.gone"], "frozen")
    assert [case["case_id"] for case in await store.list_cases("v-golden-1", split="frozen")] == [
        "kb.t.retrieval.en"
    ]
    with pytest.raises(ValueError, match="split"):
        await store.set_split("v-golden-1", ["kb.t.answer.xl"], "eternal")
    with pytest.raises(ValueError, match="case_id"):
        await store.set_split("v-golden-1", [], "frozen")


@pytest.mark.asyncio
async def test_split_and_track_checks_hold_against_raw_sql(
    golden_store: tuple[KbEvalGoldenStore, asyncpg.Pool],
) -> None:
    _store, pool = golden_store
    insert = (
        "INSERT INTO kb_eval_golden (case_id, version, track, query, relevance, split) "
        "VALUES ($1, 'v-check', 'retrieval_only', 'q', '{\"seg\": 1}'::jsonb, $2)"
    )
    async with pool.acquire() as conn:
        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(insert, "kb.check.split", "eternal")
        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(
                "INSERT INTO kb_eval_golden (case_id, version, track, query, relevance) "
                "VALUES ('kb.check.track', 'v-check', 'answer', 'q', '{\"seg\": 1}'::jsonb)"
            )
        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(
                "INSERT INTO kb_eval_golden (case_id, version, track, query, relevance) "
                "VALUES ('kb.check.query', 'v-check', 'retrieval_only', '   ', '{\"seg\": 1}'::jsonb)"
            )
        # PK is (version, case_id): same id under another version is legal.
        await conn.execute(insert, "kb.check.ok", "growth")
        with pytest.raises(asyncpg.UniqueViolationError):
            await conn.execute(insert, "kb.check.ok", "growth")


@pytest.mark.asyncio
async def test_release_pointer_drives_default_version_read(
    golden_store: tuple[KbEvalGoldenStore, asyncpg.Pool],
) -> None:
    store, _pool = golden_store
    await store.import_cases(_cases("v-old"), version="v-old")
    await store.import_cases(_cases("v-new"), version="v-new")

    assert await store.get_release("current") is None
    with pytest.raises(GoldenStoreError, match="no golden version pinned"):
        await store.list_cases()

    await store.pin_release("v-old", note="baseline run 1")
    assert await store.get_release("current") == "v-old"
    assert {case["metadata"]["version"] for case in await store.list_cases()} == {"v-old"}

    # Re-pinning replaces the pointer; a version with no rows can never be pinned.
    await store.pin_release("v-new", note="promoted after review")
    assert await store.get_release("current") == "v-new"
    with pytest.raises(GoldenStoreError, match="empty golden version"):
        await store.pin_release("v-never-imported")
    assert await store.get_release("current") == "v-new"


@pytest.mark.asyncio
async def test_import_requires_cases_and_pool(
    golden_store: tuple[KbEvalGoldenStore, asyncpg.Pool],
) -> None:
    store, _pool = golden_store
    with pytest.raises(ValueError, match="at least one case"):
        await store.import_cases([], version="v-golden-1")
    with pytest.raises(ValueError, match="duplicate case_id"):
        await store.import_cases(_cases()[:1] + _cases()[:1], version="v-golden-1")
    with pytest.raises(ValueError, match="asyncpg pool"):
        KbEvalGoldenStore(None)
    # The failed attempts wrote nothing.
    assert await store.count_cases("v-golden-1") == 0
