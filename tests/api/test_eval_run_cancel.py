from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException

from src.api.schemas.eval import EvalExperimentRetryRequest
from src.api.v1 import eval as routes


@pytest.mark.asyncio
async def test_cancel_fences_job_before_interrupt_and_uses_persisted_actor(monkeypatch):
    order = []

    class Repository:
        async def cancel_experiment_run(self, **kwargs):
            assert kwargs == {"tenant_id": "tenant", "run_id": "run", "cancelled_by": "operator"}
            order.append("fenced")
            return {"run_id": "run", "status": "cancelled", "cases": [{
                "run_case_id": "case", "runtime_handle": {
                    "thread_id": "thread", "turn_id": "turn", "user_id": "candidate-actor",
                },
            }]}

    async def interrupt(**kwargs):
        assert order == ["fenced"]
        assert kwargs["tenant_id"] == "tenant"
        assert kwargs["user_id"] == "candidate-actor"
        assert kwargs["session_id"] == "case"
        order.append("interrupt")

    monkeypatch.setattr(routes, "_get_trace_repository", lambda _request: Repository())
    monkeypatch.setattr(routes, "_require_eval_run_access", lambda *_args: None)
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(
        agent_runtime_control=SimpleNamespace(interrupt_turn=interrupt),
    )))
    result = await routes.cancel_eval_experiment_run("run", request, SimpleNamespace(tenant_id="tenant", user_id="operator"))
    assert result == {"run_id": "run", "status": "cancelled", "runtime_interrupt_pending": 0}


@pytest.mark.asyncio
async def test_cross_tenant_unknown_run_does_not_interrupt(monkeypatch):
    repo = SimpleNamespace(cancel_experiment_run=AsyncMock(return_value=None))
    interrupt = AsyncMock()
    monkeypatch.setattr(routes, "_get_trace_repository", lambda _request: repo)
    monkeypatch.setattr(routes, "_require_eval_run_access", lambda *_args: None)
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(
        agent_runtime_control=SimpleNamespace(interrupt_turn=interrupt),
    )))
    with pytest.raises(HTTPException) as error:
        await routes.cancel_eval_experiment_run("run", request, SimpleNamespace(tenant_id="other", user_id="operator"))
    assert error.value.status_code == 404
    interrupt.assert_not_awaited()


@pytest.mark.asyncio
async def test_unconfirmed_interrupt_is_visible_and_safe_to_retry(monkeypatch):
    async def cancelled(**_kwargs):
        return {"run_id": "run", "status": "cancelled", "cases": [{
            "run_case_id": "case", "runtime_handle": {}, "dispatch_state": "reconcile_required",
        }]}

    monkeypatch.setattr(routes, "_get_trace_repository", lambda _request: SimpleNamespace(cancel_experiment_run=cancelled))
    monkeypatch.setattr(routes, "_require_eval_run_access", lambda *_args: None)
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace()))
    result = await routes.cancel_eval_experiment_run("run", request, SimpleNamespace(tenant_id="tenant", user_id="operator"))
    assert result["runtime_interrupt_pending"] == 1


@pytest.mark.asyncio
async def test_selected_case_retry_requires_confirmation_and_creates_new_run(monkeypatch):
    calls = []

    class Repository:
        async def retry_failed_experiment_cases(self, **kwargs):
            calls.append(kwargs)
            return {"run_id": "new-run", "job_id": "new-job", "status": "queued"}

    monkeypatch.setattr(routes, "_get_trace_repository", lambda _request: Repository())
    monkeypatch.setattr(routes, "_require_eval_run_access", lambda *_args: None)
    auth = SimpleNamespace(tenant_id="tenant", user_id="operator")
    request = SimpleNamespace()
    with pytest.raises(HTTPException) as error:
        await routes.retry_failed_eval_experiment_cases(
            "old-run", EvalExperimentRetryRequest(case_ids=["case-a"], acknowledge_replay=False),
            request, auth, idempotency_key="request-123",
        )
    assert error.value.status_code == 422
    assert calls == []

    result = await routes.retry_failed_eval_experiment_cases(
        "old-run", EvalExperimentRetryRequest(case_ids=["case-a"], acknowledge_replay=True),
        request, auth, idempotency_key="request-123",
    )
    assert result.jobs[0].run_id == "new-run"
    assert calls == [{
        "tenant_id": "tenant", "run_id": "old-run", "case_ids": ["case-a"],
        "created_by": "operator", "idempotency_key": "request-123",
    }]


@pytest.mark.asyncio
async def test_selected_retry_blocks_unconfirmed_side_effects(monkeypatch):
    async def blocked(**_kwargs):
        raise ValueError("eval_retry_side_effect_unconfirmed:case-a")

    monkeypatch.setattr(
        routes, "_get_trace_repository",
        lambda _request: SimpleNamespace(retry_failed_experiment_cases=blocked),
    )
    monkeypatch.setattr(routes, "_require_eval_run_access", lambda *_args: None)
    with pytest.raises(HTTPException) as error:
        await routes.retry_failed_eval_experiment_cases(
            "old-run", EvalExperimentRetryRequest(case_ids=["case-a"], acknowledge_replay=True),
            SimpleNamespace(), SimpleNamespace(tenant_id="tenant", user_id="operator"),
            idempotency_key="request-123",
        )
    assert error.value.status_code == 409
