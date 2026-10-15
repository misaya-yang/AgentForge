from __future__ import annotations

import uuid
from typing import Any

import pytest
from ai_gateway_core.persistence.repositories.agent_trace_repository import (
    AgentTraceRepository,
)


class CapturingScoreRepository(AgentTraceRepository):
    def __init__(self) -> None:
        self.insert_calls: list[tuple[str, tuple[Any, ...]]] = []

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        if "SELECT trace_id FROM agent_traces" in query:
            return {"trace_id": args[0]}
        self.insert_calls.append((query, args))
        return {
            "score_id": args[0] or uuid.uuid4(),
            "trace_id": args[1],
            "span_id": args[2],
            "label": "pass",
            "metadata": args[-1],
        }


class CapturingManifestRepository(AgentTraceRepository):
    def __init__(self) -> None:
        self.fetch_calls: list[tuple[str, tuple[Any, ...]]] = []

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        self.fetch_calls.append((query, args))
        return [
            {
                "example_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "dataset_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "metadata": {},
            }
        ]


def _experiment_score_payload() -> dict[str, Any]:
    return {
        "score_name": "trajectory",
        "score_type": "numeric",
        "numeric_value": 0.9,
        "label": "pass",
        "scorer_type": "rule",
        "evaluator_version": "v1",
        "target_type": "example",
        "target_id": "example-1",
        "evaluator_id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
        "evaluator_name": "trajectory",
        "score_source": "rule",
        "metadata": {"experiment_run_id": "run-1", "case_id": "case-1"},
    }


@pytest.mark.asyncio
async def test_experiment_score_insert_is_idempotent_across_retries() -> None:
    repository = CapturingScoreRepository()
    trace_id = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"

    for _attempt in range(2):
        created = await repository.create_score(
            tenant_id="tenant-a",
            trace_id=trace_id,
            created_by="eval-worker",
            payload=_experiment_score_payload(),
        )
        assert created is not None

    first_query, first_args = repository.insert_calls[0]
    second_query, second_args = repository.insert_calls[1]
    first_score_id = str(first_args[0])
    second_score_id = str(second_args[0])

    assert uuid.UUID(first_score_id)
    assert first_score_id != trace_id
    assert first_score_id == second_score_id
    assert "ON CONFLICT (score_id)" in first_query
    assert "ON CONFLICT (score_id)" in second_query


@pytest.mark.asyncio
async def test_manual_score_insert_keeps_database_generated_identity() -> None:
    repository = CapturingScoreRepository()

    await repository.create_score(
        tenant_id="tenant-a",
        trace_id="dddddddd-dddd-4ddd-8ddd-dddddddddddd",
        created_by="user-a",
        payload={"score_name": "quality", "numeric_value": 0.8, "metadata": {}},
    )

    query, args = repository.insert_calls[0]
    assert args[0] is None
    assert "COALESCE($1::uuid, gen_random_uuid())" in query


@pytest.mark.asyncio
async def test_example_manifest_uses_one_stably_ordered_query() -> None:
    repository = CapturingManifestRepository()

    examples = await repository.list_example_manifest(
        tenant_id="tenant-a",
        dataset_id="bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    )

    assert len(examples) == 1
    assert len(repository.fetch_calls) == 1
    query, args = repository.fetch_calls[0]
    assert args == ("tenant-a", "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")
    assert "ORDER BY created_at DESC, example_id DESC" in " ".join(query.split())
    assert "LIMIT" not in query
    assert "OFFSET" not in query
