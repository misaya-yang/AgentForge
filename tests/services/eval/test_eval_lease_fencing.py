import asyncio
from contextlib import asynccontextmanager

import pytest
from ai_gateway_core.persistence.repositories.agent_trace_repository import (
    AgentTraceRepository,
    EvalLeaseLost,
)


class FencedDatabase:
    enabled = True

    def __init__(self):
        self._pool = self
        self.owners = {"job-a": "token-a", "job-b": "token-b"}
        self.domain_queries = []
        self.claims = []

    @asynccontextmanager
    async def acquire(self):
        yield self

    @asynccontextmanager
    async def transaction(self):
        yield

    async def fetchrow(self, query, *args):
        if "FOR UPDATE" in query and "agent_trace_outbox" in query:
            self.claims.append(args)
            await asyncio.sleep(0)
            return {"job_id": args[0]} if self.owners.get(args[0]) == args[3] else None
        self.domain_queries.append((query, args))
        return {"run_case_id": args[1]} if "eval_experiment_run_cases" in query else {}

    async def execute(self, query, *args):
        self.domain_queries.append((query, args))
        return "UPDATE 1"


def claim(suffix="a"):
    return {"job_id": f"job-{suffix}", "tenant_id": "tenant", "owner_id": f"owner-{suffix}",
            "claim_token": f"token-{suffix}"}


@pytest.mark.asyncio
@pytest.mark.parametrize("operation", ["case", "run", "score", "trace"])
async def test_stale_owner_cannot_write_any_eval_domain(operation):
    db = FencedDatabase()
    repo = AgentTraceRepository(db)
    db.owners["job-a"] = "new-owner-token"
    with repo.bind_outbox_claim(claim()), pytest.raises(EvalLeaseLost):
        if operation == "case":
            await repo.update_experiment_run_case(tenant_id="tenant", run_case_id="case", status="succeeded")
        elif operation == "run":
            await repo.update_experiment_run(tenant_id="tenant", run_id="run", status="succeeded")
        elif operation == "score":
            await repo.create_eval_score(tenant_id="tenant", trace_id="trace", payload={"score_name": "x"}, created_by="worker")
        else:
            await repo.ingest_trace(tenant_id="tenant", payload={"trace": {"trace_id": "trace"}}, created_by="worker")
    assert db.domain_queries == []


@pytest.mark.asyncio
async def test_two_worker_tasks_do_not_share_current_claim():
    db = FencedDatabase()
    repo = AgentTraceRepository(db)

    async def write(suffix):
        with repo.bind_outbox_claim(claim(suffix)):
            await asyncio.sleep(0)
            await repo.update_experiment_run_case(tenant_id="tenant", run_case_id=f"case-{suffix}", status="running")

    await asyncio.gather(write("a"), write("b"))
    assert {(row[0], row[3]) for row in db.claims} == {("job-a", "token-a"), ("job-b", "token-b")}
    assert len(db.domain_queries) == 2


def test_worker_cannot_bind_unfenced_job():
    with (pytest.raises(EvalLeaseLost, match="claim_missing"),
          AgentTraceRepository(FencedDatabase()).bind_outbox_claim({"job_id": "job"})):
        pass
