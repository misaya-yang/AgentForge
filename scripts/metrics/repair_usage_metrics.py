#!/usr/bin/env python3
"""
Repair usage metrics data integrity.

This script performs two operations in one transaction:
1) Remove exact duplicate rows in usage_records.
2) Rebuild usage_daily_aggregates and usage_hourly_aggregates from usage_records.

Usage:
  python scripts/metrics/repair_usage_metrics.py            # dry-run
  python scripts/metrics/repair_usage_metrics.py --apply    # execute
"""

from __future__ import annotations

import argparse
import asyncio
import os
from dataclasses import dataclass

import asyncpg

DEFAULT_DSN = os.getenv("GATEWAY_DATABASE__DSN", "")


@dataclass
class Snapshot:
    usage_records: int
    usage_daily_rows: int
    usage_hourly_rows: int
    daily_request_sum: int
    hourly_request_sum: int
    duplicated_request_ids: int
    duplicate_extra_rows: int


SNAPSHOT_SQL = """
WITH dup AS (
    SELECT request_id, COUNT(*) AS cnt
    FROM usage_records
    WHERE request_id IS NOT NULL
    GROUP BY request_id
    HAVING COUNT(*) > 1
),
agg AS (
    SELECT
        (SELECT COUNT(*)::bigint FROM usage_records) AS usage_records,
        (SELECT COUNT(*)::bigint FROM usage_daily_aggregates) AS usage_daily_rows,
        (SELECT COUNT(*)::bigint FROM usage_hourly_aggregates) AS usage_hourly_rows,
        (SELECT COALESCE(SUM(request_count), 0)::bigint FROM usage_daily_aggregates) AS daily_request_sum,
        (SELECT COALESCE(SUM(request_count), 0)::bigint FROM usage_hourly_aggregates) AS hourly_request_sum
)
SELECT
    agg.usage_records,
    agg.usage_daily_rows,
    agg.usage_hourly_rows,
    agg.daily_request_sum,
    agg.hourly_request_sum,
    COALESCE((SELECT COUNT(*)::bigint FROM dup), 0) AS duplicated_request_ids,
    COALESCE((SELECT SUM(cnt - 1)::bigint FROM dup), 0) AS duplicate_extra_rows
FROM agg;
"""


DEDUPE_DELETE_SQL = """
WITH ranked AS (
    SELECT
        id,
        ROW_NUMBER() OVER (
            PARTITION BY
                tenant_id,
                COALESCE(user_id, ''),
                COALESCE(request_id, ''),
                COALESCE(service_id, ''),
                COALESCE(assistant_id, ''),
                model,
                COALESCE(provider, ''),
                input_tokens,
                output_tokens,
                input_cost_cents,
                output_cost_cents,
                COALESCE(latency_ms, 0),
                COALESCE(first_token_ms, 0),
                COALESCE(status, ''),
                COALESCE(request_type, ''),
                COALESCE(metadata::text, '{}'),
                created_at
            ORDER BY id
        ) AS rn
    FROM usage_records
),
deleted AS (
    DELETE FROM usage_records u
    USING ranked r
    WHERE u.id = r.id
      AND r.rn > 1
    RETURNING 1
)
SELECT COUNT(*)::bigint FROM deleted;
"""


REBUILD_DAILY_SQL = """
INSERT INTO usage_daily_aggregates (
    tenant_id,
    user_id,
    model,
    assistant_id,
    service_id,
    date,
    request_count,
    success_count,
    error_count,
    total_input_tokens,
    total_output_tokens,
    total_cost_cents,
    avg_latency_ms,
    avg_first_token_ms
)
SELECT
    tenant_id,
    COALESCE(user_id, '') AS user_id,
    COALESCE(model, '') AS model,
    COALESCE(assistant_id, '') AS assistant_id,
    COALESCE(service_id, '') AS service_id,
    created_at::date AS date,
    COUNT(*)::int AS request_count,
    SUM(CASE WHEN COALESCE(status, 'success') = 'success' THEN 1 ELSE 0 END)::int AS success_count,
    SUM(CASE WHEN COALESCE(status, 'success') = 'success' THEN 0 ELSE 1 END)::int AS error_count,
    COALESCE(SUM(input_tokens), 0)::bigint AS total_input_tokens,
    COALESCE(SUM(output_tokens), 0)::bigint AS total_output_tokens,
    COALESCE(SUM(input_cost_cents + output_cost_cents), 0)::bigint AS total_cost_cents,
    COALESCE(AVG(COALESCE(latency_ms, 0))::int, 0) AS avg_latency_ms,
    COALESCE(AVG(COALESCE(first_token_ms, 0))::int, 0) AS avg_first_token_ms
FROM usage_records
GROUP BY
    tenant_id,
    COALESCE(user_id, ''),
    COALESCE(model, ''),
    COALESCE(assistant_id, ''),
    COALESCE(service_id, ''),
    created_at::date;
"""


REBUILD_HOURLY_SQL = """
INSERT INTO usage_hourly_aggregates (
    tenant_id,
    user_id,
    model,
    assistant_id,
    service_id,
    bucket_start,
    date,
    request_count,
    success_count,
    error_count,
    total_input_tokens,
    total_output_tokens,
    total_cost_cents,
    avg_latency_ms,
    avg_first_token_ms
)
SELECT
    tenant_id,
    COALESCE(user_id, '') AS user_id,
    COALESCE(model, '') AS model,
    COALESCE(assistant_id, '') AS assistant_id,
    COALESCE(service_id, '') AS service_id,
    date_trunc('hour', created_at) AS bucket_start,
    created_at::date AS date,
    COUNT(*)::int AS request_count,
    SUM(CASE WHEN COALESCE(status, 'success') = 'success' THEN 1 ELSE 0 END)::int AS success_count,
    SUM(CASE WHEN COALESCE(status, 'success') = 'success' THEN 0 ELSE 1 END)::int AS error_count,
    COALESCE(SUM(input_tokens), 0)::bigint AS total_input_tokens,
    COALESCE(SUM(output_tokens), 0)::bigint AS total_output_tokens,
    COALESCE(SUM(input_cost_cents + output_cost_cents), 0)::bigint AS total_cost_cents,
    COALESCE(AVG(COALESCE(latency_ms, 0))::int, 0) AS avg_latency_ms,
    COALESCE(AVG(COALESCE(first_token_ms, 0))::int, 0) AS avg_first_token_ms
FROM usage_records
GROUP BY
    tenant_id,
    COALESCE(user_id, ''),
    COALESCE(model, ''),
    COALESCE(assistant_id, ''),
    COALESCE(service_id, ''),
    date_trunc('hour', created_at),
    created_at::date;
"""


def _to_snapshot(row: asyncpg.Record) -> Snapshot:
    return Snapshot(
        usage_records=int(row["usage_records"] or 0),
        usage_daily_rows=int(row["usage_daily_rows"] or 0),
        usage_hourly_rows=int(row["usage_hourly_rows"] or 0),
        daily_request_sum=int(row["daily_request_sum"] or 0),
        hourly_request_sum=int(row["hourly_request_sum"] or 0),
        duplicated_request_ids=int(row["duplicated_request_ids"] or 0),
        duplicate_extra_rows=int(row["duplicate_extra_rows"] or 0),
    )


async def snapshot(conn: asyncpg.Connection) -> Snapshot:
    row = await conn.fetchrow(SNAPSHOT_SQL)
    if row is None:
        return Snapshot(0, 0, 0, 0, 0, 0, 0)
    return _to_snapshot(row)


async def repair(conn: asyncpg.Connection) -> int:
    async with conn.transaction():
        removed = int(await conn.fetchval(DEDUPE_DELETE_SQL) or 0)
        await conn.execute("TRUNCATE TABLE usage_daily_aggregates, usage_hourly_aggregates")
        await conn.execute(REBUILD_DAILY_SQL)
        await conn.execute(REBUILD_HOURLY_SQL)
        return removed


def print_snapshot(title: str, s: Snapshot) -> None:
    print(f"\n[{title}]")
    print(f"usage_records:           {s.usage_records}")
    print(f"usage_daily_rows:        {s.usage_daily_rows}")
    print(f"usage_hourly_rows:       {s.usage_hourly_rows}")
    print(f"daily_request_sum:       {s.daily_request_sum}")
    print(f"hourly_request_sum:      {s.hourly_request_sum}")
    print(f"duplicated_request_ids:  {s.duplicated_request_ids}")
    print(f"duplicate_extra_rows:    {s.duplicate_extra_rows}")


async def main() -> int:
    parser = argparse.ArgumentParser(description="Repair usage metrics aggregates from usage_records")
    parser.add_argument("--dsn", default=DEFAULT_DSN, help="PostgreSQL DSN")
    parser.add_argument("--apply", action="store_true", help="Execute repair (default is dry-run)")
    args = parser.parse_args()

    conn = await asyncpg.connect(args.dsn)
    try:
        before = await snapshot(conn)
        print_snapshot("Before", before)

        if not args.apply:
            print("\nDry-run only. Re-run with --apply to execute repair.")
            return 0

        removed = await repair(conn)
        after = await snapshot(conn)
        print(f"\nRemoved duplicate usage_records rows: {removed}")
        print_snapshot("After", after)
        return 0
    finally:
        await conn.close()


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
