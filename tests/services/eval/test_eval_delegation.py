from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from ai_gateway_core.persistence.repositories.agent_trace_repository import (
    AgentTraceRepository,
    EvalLeaseLost,
)


def repository(row):
    repo = AgentTraceRepository(SimpleNamespace())
    repo.fetchrow = AsyncMock(return_value=row)
    return repo


def claim():
    return {"job_id": "job", "tenant_id": "tenant", "owner_id": "owner", "claim_token": "claim"}


@pytest.mark.asyncio
async def test_delegation_denies_when_active_tenant_user_join_has_no_row():
    repo = repository(None)
    with repo.bind_outbox_claim(claim()), pytest.raises(RuntimeError, match="DELEGATION_SUBJECT_UNAVAILABLE"):
        await repo.resolve_eval_job_actor(tenant_id="tenant")
    query, job_id, tenant_id = repo.fetchrow.call_args.args
    assert "u.user_id = r.created_by" in query
    assert "u.tenant_id = r.tenant_id" in query and "u.status = 'active'" in query
    assert (job_id, tenant_id) == ("job", "tenant")


@pytest.mark.asyncio
async def test_delegation_requires_claim_and_never_accepts_an_actor_parameter():
    repo = repository({"user_id": "verified", "tenant_id": "tenant", "run_id": "run"})
    with pytest.raises(EvalLeaseLost):
        await repo.resolve_eval_job_actor(tenant_id="tenant")
    with repo.bind_outbox_claim(claim()):
        with pytest.raises(EvalLeaseLost):
            await repo.resolve_eval_job_actor(tenant_id="foreign")
        actor = await repo.resolve_eval_job_actor(tenant_id="tenant")
    assert actor == {"actor": "eval-worker", "subject": "verified", "tenant_id": "tenant",
                     "run_id": "run", "job_id": "job"}
