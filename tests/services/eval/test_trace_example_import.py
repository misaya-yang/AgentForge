"""Trace auto-imports deduplicate and cannot overwrite human review."""

import json
from types import SimpleNamespace
from unittest.mock import AsyncMock
from uuid import uuid4

from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository


def repository():
    repo = AgentTraceRepository(SimpleNamespace(enabled=False, _pool=None))
    repo.get_trace_detail = AsyncMock(return_value={
        "trace": {"input_preview": "synthetic question", "output_preview": "answer", "status": "succeeded"},
        "spans": [],
    })
    return repo


async def test_new_auto_import_is_pending_even_if_payload_claims_approval():
    repo = repository()
    repo.fetchrow = AsyncMock(side_effect=[None, {"metadata": {"review_status": "pending"}}])
    await repo.create_example_from_trace(
        tenant_id="tenant-a", dataset_id=str(uuid4()), created_by="user-a", user_id="user-a",
        payload={"source_trace_id": str(uuid4()), "metadata": {"review_status": "approved", "behavior_confirmed": True}},
    )
    query, *args = repo.fetchrow.await_args.args
    assert "ON CONFLICT (example_id) DO NOTHING" in query
    assert json.loads(args[5])["review_status"] == "pending"
    assert json.loads(args[5])["behavior_confirmed"] is False


async def test_reimport_returns_existing_review_without_writing():
    repo = repository()
    reviewed = {"example_id": str(uuid4()), "metadata": {"review_status": "approved"},
                "expected_output": {"contains": "manually reviewed"}}
    repo.fetchrow = AsyncMock(return_value=reviewed)
    result = await repo.create_example_from_trace(
        tenant_id="tenant-a", dataset_id=str(uuid4()), created_by="user-a",
        payload={"source_trace_id": str(uuid4()), "expected_output": {"contains": "replacement"}},
    )
    assert result == reviewed
    repo.fetchrow.assert_awaited_once()
    assert repo.fetchrow.await_args.args[0].lstrip().startswith("SELECT")


async def test_insert_conflict_reads_same_scoped_identity_without_overwrite():
    repo = repository()
    repo.fetchrow = AsyncMock(side_effect=[None, None, {"example_id": "existing"}])
    dataset_id, trace_id = str(uuid4()), str(uuid4())
    result = await repo.create_example_from_trace(
        tenant_id="tenant-a", dataset_id=dataset_id, created_by="user-a",
        payload={"source_trace_id": trace_id},
    )
    assert result["example_id"] == "existing"
    inserted_id = repo.fetchrow.await_args_list[1].args[-1]
    assert repo.fetchrow.await_args.args[1:] == (inserted_id, "tenant-a", dataset_id, trace_id, None)


async def test_foreign_span_is_rejected_before_example_lookup():
    repo = repository()
    repo.fetchrow = AsyncMock()
    assert await repo.create_example_from_trace(
        tenant_id="tenant-a", dataset_id=str(uuid4()), created_by="user-a",
        payload={"source_trace_id": str(uuid4()), "source_span_id": str(uuid4())},
    ) is None
    repo.fetchrow.assert_not_awaited()
