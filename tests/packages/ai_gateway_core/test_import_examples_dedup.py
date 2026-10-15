from __future__ import annotations

from unittest.mock import AsyncMock

import pytest
from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository


def _example(case_id: str, *, preview: str) -> dict:
    return {
        "case_id": case_id,
        "split": "regression",
        "input": {"input_preview": preview},
        "expected_output": {"contains": preview},
        "expected_trajectory": {"required_span_kinds": ["model_invocation"]},
        "assertions": [{"type": "output_contains", "value": preview}],
        "metadata": {"review_status": "approved"},
    }


@pytest.fixture
def repo() -> AgentTraceRepository:
    repository = AgentTraceRepository.__new__(AgentTraceRepository)
    repository.fetch = AsyncMock(return_value=[{"case_id": "existing.case"}])
    repository.create_example = AsyncMock(
        side_effect=lambda **kwargs: {
            "example_id": f"example-{kwargs['payload']['metadata']['case_id']}",
            "metadata": kwargs["payload"]["metadata"],
        }
    )
    return repository


@pytest.mark.asyncio
async def test_import_examples_skips_existing_and_request_duplicates(repo: AgentTraceRepository) -> None:
    result = await repo.import_examples(
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        created_by="user-a",
        mode="skip_duplicates",
        examples=[
            _example("existing.case", preview="old"),
            _example("new.case", preview="one"),
            _example("new.case", preview="dup"),
        ],
    )

    assert result["imported"] == 1
    assert result["skipped"] == 2
    assert repo.create_example.await_count == 1


@pytest.mark.asyncio
async def test_import_examples_append_mode_allows_duplicates(repo: AgentTraceRepository) -> None:
    result = await repo.import_examples(
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        created_by="user-a",
        mode="append",
        examples=[
            _example("existing.case", preview="old"),
            _example("new.case", preview="one"),
        ],
    )

    assert result["imported"] == 2
    assert result["skipped"] == 0
    assert repo.create_example.await_count == 2
    repo.fetch.assert_not_called()


@pytest.mark.asyncio
async def test_delete_example_scopes_the_delete_to_tenant_dataset_and_example() -> None:
    repository = AgentTraceRepository.__new__(AgentTraceRepository)
    repository.fetchrow = AsyncMock(
        side_effect=[{"example_id": "example-a"}, None],
    )

    assert await repository.delete_example(
        tenant_id="tenant-a",
        dataset_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        example_id="bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    )
    assert not await repository.delete_example(
        tenant_id="tenant-b",
        dataset_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        example_id="bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    )

    query, tenant_id, dataset_id, example_id = repository.fetchrow.await_args_list[0].args
    assert "tenant_id = $1" in query
    assert "dataset_id = $2::uuid" in query
    assert "example_id = $3::uuid" in query
    assert (tenant_id, dataset_id, example_id) == (
        "tenant-a",
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    )
