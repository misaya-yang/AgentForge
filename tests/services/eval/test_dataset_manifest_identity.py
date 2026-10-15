"""Release checks and real Eval jobs bind the same complete dataset identity."""

import copy
import json
from contextlib import nullcontext
from types import SimpleNamespace
from unittest.mock import AsyncMock
from uuid import uuid4

import pytest
from ai_gateway_core.eval.dataset_manifest import build_eval_dataset_manifest
from ai_gateway_core.persistence.repositories.agent_repository import DatabaseAgentRepository
from ai_gateway_core.persistence.repositories.agent_trace_repository import (
    AgentTraceRepository,
    _canonical_hash,
)

from tests.services.eval.test_assistant_trace_reconciler import _Holder


def _fixture():
    dataset = {"dataset_id": uuid4(), "tenant_id": "tenant-a", "name": "Cases",
               "version": "v1", "schema": {}, "metadata": {"owner": "quality"}}
    examples = [{
        "example_id": uuid4(), "split": "regression", "input": {"message": "Fact?"},
        "expected_output": {"contains": "fact"}, "source_trace_id": uuid4(),
        "metadata": {"case_id": "fact", "review_status": "approved",
                     "assertions": [{"type": "output_contains", "value": "fact"}]},
    }]
    return dataset, examples


async def test_release_live_and_rescore_use_identical_content_hash():
    dataset, examples = _fixture()
    release_conn = SimpleNamespace(fetchrow=AsyncMock(return_value=dataset),
                                   fetch=AsyncMock(return_value=examples))
    release = await DatabaseAgentRepository(_Holder(release_conn))._eval_dataset_snapshot_from_conn(
        release_conn, tenant_id="tenant-a", dataset_id=str(dataset["dataset_id"]),
    )
    conn = SimpleNamespace(
        fetchrow=AsyncMock(side_effect=[{"run_id": uuid4()}, {"job_id": uuid4()},
                                       {"run_id": uuid4()}, {"job_id": uuid4()}]),
        executemany=AsyncMock(), transaction=lambda: nullcontext(),
    )
    repo = AgentTraceRepository(_Holder(conn))
    repo.get_dataset = AsyncMock(return_value=dataset)
    repo.list_example_manifest = AsyncMock(return_value=examples)
    evaluator = {"evaluator_id": str(uuid4()), "evaluator_type": "rule"}
    repo.get_evaluator = AsyncMock(return_value=evaluator)
    repo.freeze_eval_judge = AsyncMock(side_effect=lambda **kwargs: kwargs["evaluator"])
    repo.freeze_eval_model_ref = AsyncMock(return_value={"model_id": "model-a", "provider_id": "p"})
    await repo.enqueue_live_experiment_run(
        tenant_id="tenant-a", experiment_id=str(uuid4()), dataset_id=str(dataset["dataset_id"]),
        evaluator_snapshots=[evaluator], examples=examples, repetitions=1, created_by="builder",
        target_snapshot={}, execution_config={"model_id": "model-a"}, candidate_fingerprint={},
    )
    await repo.enqueue_evaluator_run(
        tenant_id="tenant-a", evaluator_id=evaluator["evaluator_id"], created_by="builder",
        payload={"dataset_id": str(dataset["dataset_id"])},
    )
    assert conn.fetchrow.await_args_list[0].args[7] == release["manifest_hash"]
    assert conn.fetchrow.await_args_list[2].args[5] == release["manifest_hash"]
    rescore_job = json.loads(conn.fetchrow.await_args_list[3].args[2])
    assert rescore_job["dataset_manifest"][0]["metadata"]["assertions"] == examples[0]["metadata"]["assertions"]


@pytest.mark.parametrize("field", ["input", "expected_output", "split", "source_trace_id", "metadata"])
def test_manifest_binds_behavior_review_and_provenance(field):
    dataset, examples = _fixture()
    original = _canonical_hash(build_eval_dataset_manifest(dataset, examples))
    changed = copy.deepcopy(examples)
    changed[0][field] = {"changed": True} if field in {"input", "expected_output", "metadata"} else str(uuid4())
    assert _canonical_hash(build_eval_dataset_manifest(dataset, changed)) != original


def test_manifest_normalizes_sql_json_and_order_but_binds_dataset_revision():
    dataset, examples = _fixture()
    original = _canonical_hash(build_eval_dataset_manifest(dataset, examples))
    encoded = copy.deepcopy(examples)
    for key in ("input", "expected_output", "metadata"):
        encoded[0][key] = json.dumps(encoded[0][key])
    assert _canonical_hash(build_eval_dataset_manifest(dataset, encoded)) == original
    assert _canonical_hash(build_eval_dataset_manifest({**dataset, "version": "v2"}, examples)) != original
