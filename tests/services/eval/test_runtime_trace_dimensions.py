"""Real SQL attribution tests use connection-private tables only."""
import json
import os

import asyncpg
import pytest
from ai_gateway_core.persistence.repositories.runtime_trace_dimensions import (
    bind_runtime_trace_dimensions,
)

from tests.database.test_agent_studio_migrations import _postgres_config

pytestmark = pytest.mark.skipif(
    os.environ.get("AGENT_TRACE_DIMENSIONS_DB_LIVE") != "1",
    reason="Set AGENT_TRACE_DIMENSIONS_DB_LIVE=1 for the private-table SQL gate.",
)
RUN = "11111111-1111-4111-8111-111111111111"
AGENT = "22222222-2222-4222-8222-222222222222"
VERSION = "33333333-3333-4333-8333-333333333333"
OTHER = "44444444-4444-4444-8444-444444444444"


class PrivateTables:
    def __init__(self, connection):
        self.connection = connection

    async def execute(self, sql, *args):
        return await self.connection.execute(
            sql.replace("assistant.sessions", "pg_temp.test_sessions"), *args,
        )


@pytest.mark.parametrize("mismatch", [None, "tenant", "user", "session", "agent", "version", "engine", "existing"])
async def test_dimensions_use_original_scoped_pin_and_preserve_existing_identity(mismatch):
    conn = await asyncpg.connect(**_postgres_config())
    try:
        await conn.execute("""
            CREATE TEMP TABLE assistant_runs (run_id uuid, tenant_id text, user_id text,
                session_id text, runtime_snapshot_id uuid, engine text);
            CREATE TEMP TABLE assistant_runtime_snapshots (snapshot_id uuid, run_id uuid,
                tenant_id text, user_id text, session_id text, snapshot jsonb);
            CREATE TEMP TABLE test_sessions (session_id text, tenant_id text, user_id text,
                agent_id uuid, agent_version_id uuid, agent_draft_revision int,
                publication_id uuid, channel text, runtime_fingerprint text, agent_spec_hash text);
            CREATE TEMP TABLE agent_traces (trace_id uuid, tenant_id text, user_id text,
                session_id text, trace_family text, run_id text, agent_id uuid,
                agent_version_id uuid, agent_draft_revision int, publication_id uuid,
                channel text, runtime_fingerprint text, agent_spec_hash text);
        """)
        await conn.execute("INSERT INTO assistant_runs VALUES ($1, 'tenant-a', 'owner-a', 'session-a', NULL, $2)",
                           RUN, "other" if mismatch == "engine" else "agent_runtime")
        snapshot = {"agent_spec": {"agentId": OTHER if mismatch == "agent" else AGENT,
                                   "agentVersionId": OTHER if mismatch == "version" else VERSION}}
        await conn.execute("INSERT INTO assistant_runtime_snapshots VALUES ($1, $1, 'tenant-a', 'owner-a', 'session-a', $2::jsonb)",
                           RUN, json.dumps(snapshot))
        await conn.execute("INSERT INTO test_sessions VALUES ('session-a','tenant-a','owner-a',$1,$2,NULL,$3,'hosted','runtime-a','spec-a')",
                           AGENT, VERSION, OTHER)
        await conn.execute("INSERT INTO agent_traces (trace_id,tenant_id,user_id,session_id,trace_family,run_id,agent_id) VALUES ($1,'tenant-a','owner-a','session-a','assistant',$2,$3)",
                           RUN, RUN, OTHER if mismatch == "existing" else None)
        scope = {"trace_id": RUN, "tenant_id": "tenant-a", "user_id": "owner-a", "session_id": "session-a"}
        if mismatch in {"tenant", "user", "session"}:
            scope[{"tenant": "tenant_id", "user": "user_id", "session": "session_id"}[mismatch]] = "wrong"
        for _ in range(2):
            await bind_runtime_trace_dimensions(PrivateTables(conn), **scope)
        row = await conn.fetchrow("SELECT agent_id,agent_version_id,channel FROM agent_traces")
        if mismatch is None:
            assert str(row["agent_id"]) == AGENT
            assert str(row["agent_version_id"]) == VERSION
            assert row["channel"] == "hosted"
        else:
            assert (str(row["agent_id"]) if row["agent_id"] else None) == (OTHER if mismatch == "existing" else None)
            assert row["agent_version_id"] is None and row["channel"] is None
    finally:
        await conn.close()
