"""Real-PostgreSQL tests for the knowledge-base migrations 065, 066, and 082.

Closes PRD debt A4: the KB ragas/identity migrations had no tier-b coverage.
Pattern follows tests/database/test_agent_studio_migrations.py: throwaway
schema, minimal inline prerequisites, every migration applied twice, and
behavioral assertions against a live developer PostgreSQL.
"""
from __future__ import annotations

import json
import os
import uuid
from collections.abc import AsyncIterator
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

import asyncpg
import pytest
import pytest_asyncio
from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository
from dotenv import dotenv_values

ROOT = Path(__file__).resolve().parents[2]
MIGRATION_065 = ROOT / "database" / "migrations" / "065_kb_ragas_evaluator.sql"
MIGRATION_066 = ROOT / "database" / "migrations" / "066_kb_ragas_score_source.sql"
MIGRATION_082 = ROOT / "database" / "migrations" / "082_kb_dataset_collection_identity.sql"

_TENANT = "t-kb"
_E1 = uuid.UUID("11111111-1111-4111-8111-111111111111")
_E2 = uuid.UUID("22222222-2222-4222-8222-222222222222")
_E3 = uuid.UUID("33333333-3333-4333-8333-333333333333")
# Fixed pair so the score_id DESC tie-breaker (after identical created_at) is
# deterministic: the max uuid must win the dedup CTE.
_SCORE_LOSER = uuid.UUID("00000000-0000-4000-8000-000000000001")
_SCORE_WINNER = uuid.UUID("ffffffff-ffff-4fff-8fff-ffffffffffff")


def _postgres_config() -> dict[str, Any]:
    file_values = dotenv_values(os.environ.get("ENV_FILE") or ROOT / ".env")
    required = ("POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB", "POSTGRES_PORT")
    values = {key: os.environ.get(key) or file_values.get(key) for key in required}
    missing = [key for key, value in values.items() if not value]
    if missing:
        pytest.fail(f"local PostgreSQL test configuration missing keys: {', '.join(missing)}")
    return {
        "host": "127.0.0.1",
        "port": int(str(values["POSTGRES_PORT"])),
        "user": str(values["POSTGRES_USER"]),
        "password": str(values["POSTGRES_PASSWORD"]),
        "database": str(values["POSTGRES_DB"]),
    }


def _ago(hours: int) -> datetime:
    return datetime.now(timezone.utc) - timedelta(hours=hours)


@pytest_asyncio.fixture
async def kb_pool() -> AsyncIterator[asyncpg.Pool]:
    config = _postgres_config()
    schema_name = f"kb_migration_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE SCHEMA "{schema_name}"')
    await admin.close()

    pool = await asyncpg.create_pool(
        **config,
        min_size=1,
        max_size=4,
        server_settings={"search_path": f'"{schema_name}",public'},
    )
    try:
        async with pool.acquire() as conn:
            # Minimal prerequisites: the columns 065/066/082 touch plus the
            # columns get_kb_ragas_summary reads, with the PRE-migration
            # constraint definitions so the 065/066 swaps are load-bearing.
            await conn.execute(
                """
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL DEFAULT '',
                    name VARCHAR(255) NOT NULL DEFAULT '',
                    collection_name VARCHAR(255),
                    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
                    deleted_at TIMESTAMPTZ,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE eval_evaluators (
                    evaluator_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    tenant_id VARCHAR(64) NOT NULL,
                    name VARCHAR(128) NOT NULL,
                    evaluator_type VARCHAR(32) NOT NULL DEFAULT 'human',
                    version VARCHAR(64) NOT NULL DEFAULT 'v1',
                    created_by VARCHAR(64) NOT NULL DEFAULT 'tester',
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    CONSTRAINT chk_eval_evaluators_type
                        CHECK (evaluator_type IN ('human', 'rule', 'llm', 'composite'))
                );
                CREATE TABLE agent_traces (
                    trace_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    trace_family VARCHAR(32) NOT NULL DEFAULT 'assistant',
                    tenant_id VARCHAR(64) NOT NULL,
                    user_id VARCHAR(64) NOT NULL DEFAULT '',
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE agent_trace_scores (
                    score_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    trace_id UUID NOT NULL REFERENCES agent_traces(trace_id) ON DELETE CASCADE,
                    evaluator_id UUID,
                    evaluator_version VARCHAR(64),
                    score_name VARCHAR(96) NOT NULL,
                    label VARCHAR(96),
                    numeric_value DOUBLE PRECISION,
                    score_source VARCHAR(32) NOT NULL DEFAULT 'human',
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    CONSTRAINT chk_agent_trace_scores_source
                        CHECK (score_source IN ('human', 'llm', 'rule', 'system', 'imported'))
                );
                """
            )
            for migration in (MIGRATION_082, MIGRATION_065, MIGRATION_066):
                sql = migration.read_text(encoding="utf-8")
                await conn.execute(sql)
                # Apply a second time: idempotency assertion — any error fails
                # the fixture (and every dependent test) loudly.
                await conn.execute(sql)
        yield pool
    finally:
        await pool.close()
        admin = await asyncpg.connect(**config)
        await admin.execute(f'DROP SCHEMA "{schema_name}" CASCADE')
        await admin.close()


# ---------------------------------------------------------------------------
# 082 — dataset collection identity
# ---------------------------------------------------------------------------


async def _insert_dataset(
    conn: asyncpg.Connection,
    dataset_id: str,
    *,
    collection_name: str | None,
    is_deleted: bool = False,
) -> None:
    await conn.execute(
        """
        INSERT INTO datasets (dataset_id, tenant_id, name, collection_name, is_deleted)
        VALUES ($1, $2, $1, $3, $4)
        """,
        dataset_id,
        _TENANT,
        collection_name,
        is_deleted,
    )


async def test_082_collection_identity_index_shape(kb_pool: asyncpg.Pool) -> None:
    async with kb_pool.acquire() as conn:
        index_def = await conn.fetchval(
            """
            SELECT indexdef
            FROM pg_indexes
            WHERE schemaname = current_schema()
              AND indexname = 'idx_datasets_collection_name_unique_nonempty'
            """
        )
        assert index_def is not None
        upper = index_def.upper()
        assert "UNIQUE" in upper
        assert "COLLECTION_NAME" in upper
        # The predicate is the whole point: NULL and blank names are outside
        # the reservation set.
        assert "WHERE" in upper
        assert "IS NOT NULL" in upper
        assert "BTRIM" in upper
        # No is_deleted filter — soft-deleted rows keep reserving the name.
        assert "IS_DELETED" not in upper


async def test_082_rejects_duplicate_nonblank_collection_names(kb_pool: asyncpg.Pool) -> None:
    async with kb_pool.acquire() as conn:
        await _insert_dataset(conn, "ds-alpha", collection_name="coll-alpha")
        with pytest.raises(asyncpg.UniqueViolationError):
            await _insert_dataset(conn, "ds-alpha-two", collection_name="coll-alpha")


async def test_082_allows_multiple_null_and_blank_collection_names(kb_pool: asyncpg.Pool) -> None:
    async with kb_pool.acquire() as conn:
        await _insert_dataset(conn, "ds-null-1", collection_name=None)
        await _insert_dataset(conn, "ds-null-2", collection_name=None)
        await _insert_dataset(conn, "ds-empty-1", collection_name="")
        await _insert_dataset(conn, "ds-empty-2", collection_name="   ")
        await _insert_dataset(conn, "ds-empty-3", collection_name="")
        count = await conn.fetchval(
            """
            SELECT COUNT(*) FROM datasets
            WHERE collection_name IS NULL OR BTRIM(collection_name) = ''
            """
        )
        assert count == 5


async def test_082_soft_deleted_row_still_reserves_collection_name(kb_pool: asyncpg.Pool) -> None:
    async with kb_pool.acquire() as conn:
        await _insert_dataset(
            conn, "ds-soft-deleted", collection_name="coll-soft", is_deleted=True
        )
        # The partial index has no is_deleted filter, so a soft-deleted
        # dataset must keep blocking silent collection reassignment.
        with pytest.raises(asyncpg.UniqueViolationError):
            await _insert_dataset(conn, "ds-soft-reuse", collection_name="coll-soft")
        # A blank name on a live row is still fine alongside the reservation.
        await _insert_dataset(conn, "ds-soft-blank", collection_name="  ")


# ---------------------------------------------------------------------------
# 065 + 066 — kb_ragas evaluator type and score source
# ---------------------------------------------------------------------------

_CONSTRAINT_TABLES = {
    "chk_eval_evaluators_type": "eval_evaluators",
    "chk_agent_trace_scores_source": "agent_trace_scores",
}


async def test_065_and_066_replace_check_constraints(kb_pool: asyncpg.Pool) -> None:
    async with kb_pool.acquire() as conn:
        rows = await conn.fetch(
            """
            SELECT c.conname, r.relname AS table_name,
                   pg_get_constraintdef(c.oid) AS def
            FROM pg_constraint c
            JOIN pg_namespace n ON n.oid = c.connamespace
            JOIN pg_class r ON r.oid = c.conrelid
            WHERE n.nspname = current_schema()
              AND c.conname = ANY($1::text[])
            """,
            list(_CONSTRAINT_TABLES),
        )
        found = {row["conname"]: row["def"].lower() for row in rows}
        assert set(found) == set(_CONSTRAINT_TABLES)
        assert "'ragas'" in found["chk_eval_evaluators_type"]
        assert "'kb_ragas'" in found["chk_agent_trace_scores_source"]
        for row in rows:
            assert row["table_name"] == _CONSTRAINT_TABLES[row["conname"]]


async def test_065_ragas_evaluator_roundtrip(kb_pool: asyncpg.Pool) -> None:
    evaluator_id = uuid.uuid4()
    async with kb_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO eval_evaluators (evaluator_id, tenant_id, name, evaluator_type)
            VALUES ($1, $2, 'kb-ragas-faithfulness', 'ragas')
            """,
            evaluator_id,
            _TENANT,
        )
        stored = await conn.fetchval(
            "SELECT evaluator_type FROM eval_evaluators WHERE evaluator_id = $1",
            evaluator_id,
        )
        assert stored == "ragas"
        # The replacement constraint must still reject unknown types.
        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(
                """
                INSERT INTO eval_evaluators (tenant_id, name, evaluator_type)
                VALUES ($1, 'bogus', 'definitely-not-a-type')
                """,
                _TENANT,
            )


async def test_066_kb_ragas_score_roundtrip(kb_pool: asyncpg.Pool) -> None:
    trace_id = uuid.uuid4()
    score_id = uuid.uuid4()
    async with kb_pool.acquire() as conn:
        await conn.execute(
            "INSERT INTO agent_traces (trace_id, trace_family, tenant_id) VALUES ($1, 'rag', $2)",
            trace_id,
            _TENANT,
        )
        await conn.execute(
            """
            INSERT INTO agent_trace_scores (score_id, trace_id, score_name, score_source)
            VALUES ($1, $2, 'faithfulness', 'kb_ragas')
            """,
            score_id,
            trace_id,
        )
        stored = await conn.fetchval(
            "SELECT score_source FROM agent_trace_scores WHERE score_id = $1", score_id
        )
        assert stored == "kb_ragas"
        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(
                """
                INSERT INTO agent_trace_scores (trace_id, score_name, score_source)
                VALUES ($1, 'faithfulness', 'not-a-source')
                """,
                trace_id,
            )


# ---------------------------------------------------------------------------
# get_kb_ragas_summary — real repository call against the throwaway schema
# ---------------------------------------------------------------------------


@dataclass
class _Holder:
    _pool: Any
    enabled: bool = True


async def _seed_ragas_fixture(kb_pool: asyncpg.Pool) -> None:
    """Seed traces/scores exercising window, dedup, source, and tenant filters.

    Layout (all kb_ragas unless noted):
    - tr1 (rag, ds-1, 2h ago): faithfulness v1 rev A (0.2 fail, 4h) superseded
      by rev B (1.0 pass, 2h, judge_model=judge-old); faithfulness v2
      (0.5 pass) must NOT dedup with v1; harmfulness review (0.3); a
      score_source='human' precision row that must be invisible.
    - tr2 (rag, ds-2, 3d ago): answer_relevancy NULL-version (0.75 pass, 1h,
      judge_model=judge-new) dedups with an older empty-version row (0.9
      fail, 2h) via COALESCE(version, '').
    - tr3 (rag, ds-1, 30h ago): groundedness tie-breaker pair with identical
      created_at — only the max score_id (1.0 pass) survives.
    - tr-out (rag, ds-1, 30d ago): faithfulness pass — excluded by window.
    - tr-assistant (assistant family) and an other-tenant rag trace.
    """
    tr1, tr2, tr3, tr_out, tr_asst, tr_other = (uuid.uuid4() for _ in range(6))
    tie_at = _ago(10)
    async with kb_pool.acquire() as conn, conn.transaction():
        for trace_id, family, tenant, hours, dataset in (
            (tr1, "rag", _TENANT, 2, "ds-1"),
            (tr2, "rag", _TENANT, 72, "ds-2"),
            (tr3, "rag", _TENANT, 30, "ds-1"),
            (tr_out, "rag", _TENANT, 30 * 24, "ds-1"),
            (tr_asst, "assistant", _TENANT, 1, "ds-1"),
            (tr_other, "rag", "t-other", 1, "ds-1"),
        ):
            await conn.execute(
                """
                INSERT INTO agent_traces (trace_id, trace_family, tenant_id, metadata, created_at)
                VALUES ($1, $2, $3, $4::jsonb, $5)
                """,
                trace_id,
                family,
                tenant,
                json.dumps({"dataset_id": dataset}),
                _ago(hours),
            )

        async def score(
            trace: uuid.UUID,
            evaluator: uuid.UUID,
            name: str,
            *,
            version: str | None = "v1",
            numeric: float | None = None,
            label: str | None = "pass",
            source: str = "kb_ragas",
            created: datetime | None = None,
            score_id: uuid.UUID | None = None,
            metadata: dict[str, str] | None = None,
        ) -> None:
            await conn.execute(
                """
                INSERT INTO agent_trace_scores (
                    score_id, trace_id, evaluator_id, evaluator_version, score_name,
                    label, numeric_value, score_source, metadata, created_at
                ) VALUES (
                    COALESCE($1::uuid, gen_random_uuid()), $2, $3, $4, $5,
                    $6, $7, $8, $9::jsonb, COALESCE($10, NOW())
                )
                """,
                score_id,
                trace,
                evaluator,
                version,
                name,
                label,
                numeric,
                source,
                json.dumps(metadata or {}),
                created,
            )

        await score(tr1, _E1, "faithfulness", numeric=0.2, label="fail", created=_ago(4))
        await score(
            tr1,
            _E1,
            "faithfulness",
            numeric=1.0,
            created=_ago(2),
            metadata={"judge_model": "judge-old"},
        )
        await score(tr1, _E1, "faithfulness", version="v2", numeric=0.5, created=_ago(2))
        await score(tr1, _E1, "harmfulness", numeric=0.3, label="review", created=_ago(2))
        await score(tr1, _E1, "precision", source="human", numeric=1.0, created=_ago(2))
        await score(
            tr2,
            _E2,
            "answer_relevancy",
            version=None,
            numeric=0.75,
            created=_ago(1),
            metadata={"judge_model": "judge-new"},
        )
        await score(
            tr2, _E2, "answer_relevancy",
            version="", numeric=0.9, label="fail", created=_ago(2),
        )
        await score(
            tr3, _E3, "groundedness",
            numeric=0.0, label="fail", created=tie_at, score_id=_SCORE_LOSER,
        )
        await score(
            tr3, _E3, "groundedness",
            numeric=1.0, label="pass", created=tie_at, score_id=_SCORE_WINNER,
        )
        await score(tr_out, _E1, "faithfulness", numeric=1.0, label="pass")
        await score(tr_other, _E1, "faithfulness", numeric=1.0, label="pass")


def _metric(summary: dict[str, Any], name: str) -> dict[str, Any]:
    for row in summary["metrics"]:
        if row["metric"] == name:
            return row
    raise AssertionError(f"metric {name!r} missing from {summary['metrics']}")


async def test_get_kb_ragas_summary_window_dedup_and_source_filters(
    kb_pool: asyncpg.Pool,
) -> None:
    await _seed_ragas_fixture(kb_pool)
    repository = AgentTraceRepository(_Holder(kb_pool))

    summary = await repository.get_kb_ragas_summary(tenant_id=_TENANT, days=7)
    assert summary["window_days"] == 7
    assert summary["dataset_id"] is None
    # Window keeps tr1/tr2/tr3; drops tr_out, the assistant-family trace,
    # and the other-tenant trace.
    assert summary["rag_traces"] == 3
    assert summary["ragas_scored_traces"] == 3
    assert {row["metric"] for row in summary["metrics"]} == {
        "faithfulness",
        "answer_relevancy",
        "groundedness",
        "harmfulness",
    }
    # 'precision' exists only as score_source='human' → must be invisible.
    assert "precision" not in {row["metric"] for row in summary["metrics"]}

    faithfulness = _metric(summary, "faithfulness")
    # Dedup keeps rev B (1.0 pass) over A (0.2 fail); v2 (0.5 pass) is a
    # different partition and survives independently.
    assert faithfulness["scored_count"] == 2
    assert faithfulness["pass_count"] == 2
    assert faithfulness["fail_count"] == 0
    assert faithfulness["average_score"] == pytest.approx(0.75)

    answer_relevancy = _metric(summary, "answer_relevancy")
    # COALESCE(evaluator_version, '') folds NULL and '' into one partition;
    # the 0.9 fail row must be superseded by the 0.75 pass row.
    assert answer_relevancy["scored_count"] == 1
    assert answer_relevancy["pass_count"] == 1
    assert answer_relevancy["fail_count"] == 0
    assert answer_relevancy["average_score"] == pytest.approx(0.75)

    groundedness = _metric(summary, "groundedness")
    # Identical created_at tie-break: only the max score_id row counts.
    assert groundedness["scored_count"] == 1
    assert groundedness["pass_count"] == 1
    assert groundedness["fail_count"] == 0
    assert groundedness["average_score"] == pytest.approx(1.0)

    harmfulness = _metric(summary, "harmfulness")
    # label='review' counts toward review_count only; COALESCE(AVG…) → 0.
    assert harmfulness["scored_count"] == 0
    assert harmfulness["review_count"] == 1
    assert harmfulness["average_score"] == 0.0

    # Latest latest_scores row with a judge_model wins (tr2 at 1h ago).
    assert summary["latest_judge_model"] == "judge-new"

    # A 1-day window keeps only tr1 (2h ago); tr3 (30h) and tr2 (3d) drop out.
    summary_1d = await repository.get_kb_ragas_summary(tenant_id=_TENANT, days=1)
    assert summary_1d["rag_traces"] == 1
    assert summary_1d["ragas_scored_traces"] == 1
    assert {row["metric"] for row in summary_1d["metrics"]} == {"faithfulness", "harmfulness"}

    summary_2d = await repository.get_kb_ragas_summary(tenant_id=_TENANT, days=2)
    assert summary_2d["rag_traces"] == 2
    assert {row["metric"] for row in summary_2d["metrics"]} == {
        "faithfulness",
        "groundedness",
        "harmfulness",
    }

    # Dataset filter narrows to tr2's metadata->>'dataset_id'.
    summary_ds2 = await repository.get_kb_ragas_summary(
        tenant_id=_TENANT, days=7, dataset_id="ds-2"
    )
    assert summary_ds2["dataset_id"] == "ds-2"
    assert summary_ds2["rag_traces"] == 1
    assert [row["metric"] for row in summary_ds2["metrics"]] == ["answer_relevancy"]
    assert summary_ds2["latest_judge_model"] == "judge-new"

    # days is clamped to >= 1.
    assert (await repository.get_kb_ragas_summary(tenant_id=_TENANT, days=0))["window_days"] == 1

    # Unknown tenant sees nothing.
    empty = await repository.get_kb_ragas_summary(tenant_id="tenant-missing", days=7)
    assert empty["rag_traces"] == 0
    assert empty["metrics"] == []
    assert empty["latest_judge_model"] is None
