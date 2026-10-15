"""Missing Runtime usage is unknown; only complete scoped receipts set totals."""
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from src.services.eval.assistant_trace_capture import enrich_runtime_trace_usage


@pytest.mark.parametrize("complete", [True, False])
async def test_background_trace_usage_requires_all_dispatched_call_receipts(complete):
    trace = {"run_id": "run-a", "tenant_id": "tenant-a", "user_id": "owner-a",
             "session_id": "session-a", "metadata": {}, "metrics": {"total_tokens": 0}}
    repo = SimpleNamespace(
        fetchrow=AsyncMock(return_value={"harness_thread_id": "thread-a"}),
        get_assistant_runtime_trace_model_usage=AsyncMock(return_value={
            "dispatched_calls": 2, "measured_calls": 2 if complete else 1,
            "input_tokens": 100, "output_tokens": 20, "cost_microusd": 17,
            "cost_measured_calls": 2 if complete else 1,
        }),
    )
    await enrich_runtime_trace_usage(repo, trace)
    assert repo.fetchrow.await_args.args[1:] == ("run-a", "tenant-a", "owner-a", "session-a")
    assert repo.get_assistant_runtime_trace_model_usage.await_args.kwargs == {
        "run_id": "run-a", "tenant_id": "tenant-a", "user_id": "owner-a",
        "session_id": "session-a", "runtime_thread_id": "thread-a",
    }
    usage = trace["metadata"]["runtime_model_usage"]
    assert usage["tokens_complete"] is complete
    assert trace["metrics"]["total_tokens"] == (120 if complete else 0)
    assert ("cost_microusd" in usage) is complete


async def test_background_trace_does_not_read_calls_without_scoped_terminal_run():
    repo = SimpleNamespace(fetchrow=AsyncMock(return_value=None),
                           get_assistant_runtime_trace_model_usage=AsyncMock())
    trace = {"run_id": "run-a", "tenant_id": "tenant-a", "user_id": "owner-a",
             "session_id": "session-a", "metadata": {}, "metrics": {}}
    await enrich_runtime_trace_usage(repo, trace)
    repo.get_assistant_runtime_trace_model_usage.assert_not_awaited()
    assert trace["metrics"] == {}
