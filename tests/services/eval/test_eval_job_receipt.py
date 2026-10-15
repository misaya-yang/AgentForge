"""An accepted evaluator job must have a serializable API receipt."""

from contextlib import nullcontext
from types import SimpleNamespace
from unittest.mock import AsyncMock
from uuid import uuid4

from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository

from src.api.schemas.eval import EvalAsyncJobResponse
from tests.services.eval.test_assistant_trace_reconciler import _Holder


async def test_evaluator_enqueue_returns_string_ids_after_single_transaction():
    run_id, job_id = uuid4(), uuid4()
    conn = SimpleNamespace(
        fetchrow=AsyncMock(side_effect=[{"run_id": run_id}, {"job_id": job_id}]),
        transaction=lambda: nullcontext(),
    )
    repo = AgentTraceRepository(_Holder(conn))
    evaluator = {"evaluator_id": str(uuid4()), "evaluator_type": "rule", "metadata": {}}
    repo.get_evaluator = AsyncMock(return_value=evaluator)
    repo.freeze_eval_judge = AsyncMock(return_value=evaluator)
    receipt = await repo.enqueue_evaluator_run(
        tenant_id="tenant-a", evaluator_id=evaluator["evaluator_id"],
        created_by="user-a", payload={"trace_id": str(uuid4())},
    )
    result = EvalAsyncJobResponse.model_validate(receipt)
    assert result.job_id == str(job_id) and result.run_id == str(run_id)
    assert conn.fetchrow.await_count == 2
