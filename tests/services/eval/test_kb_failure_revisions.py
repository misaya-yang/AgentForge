from __future__ import annotations

import asyncio
import json
import uuid
from contextlib import asynccontextmanager
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace

import pytest
from ai_gateway_core.eval.evaluator_executor import is_runnable_dataset_example
from ai_gateway_core.persistence.repositories.agent_trace_repository import (
    AgentTraceRepository,
    EvalCaseRevisionConflict,
)


class _Connection:
    def __init__(self) -> None:
        self.rows: list[dict] = []
        self.lock = asyncio.Lock()
        self.advisory_calls = 0

    @asynccontextmanager
    async def transaction(self):
        async with self.lock:
            yield

    async def execute(self, sql: str, *_args):
        assert "pg_advisory_xact_lock" in sql
        self.advisory_calls += 1

    async def fetch(self, _sql: str, tenant_id: str, dataset_id: str, case_id: str):
        return [
            row for row in reversed(self.rows)
            if row["tenant_id"] == tenant_id and row["dataset_id"] == dataset_id
            and row["metadata"]["case_id"] == case_id
        ]

    async def fetchrow(self, sql: str, *args):
        if "FROM eval_datasets" in sql:
            return {"dataset_id": args[1]}
        if "FROM eval_examples" in sql:
            tenant_id, dataset_id, example_id = args
            return next((row for row in self.rows if row["tenant_id"] == tenant_id
                         and row["dataset_id"] == dataset_id
                         and row["example_id"] == example_id), None)
        if "INSERT INTO eval_examples" in sql:
            dataset_id, tenant_id, input_json, expected_json, metadata_json, trace_id, created_by = args
            row = {
                "example_id": str(uuid.uuid4()),
                "dataset_id": dataset_id,
                "tenant_id": tenant_id,
                "split": "review",
                "input": json.loads(input_json),
                "expected_output": json.loads(expected_json),
                "metadata": json.loads(metadata_json),
                "source_trace_id": trace_id,
                "source_span_id": None,
                "created_by": created_by,
                "created_at": datetime.now(timezone.utc) + timedelta(microseconds=len(self.rows)),
            }
            self.rows.append(row)
            return row
        if "UPDATE eval_examples" in sql:
            tenant_id, dataset_id, example_id, split, metadata_json = args
            row = next(row for row in self.rows if row["tenant_id"] == tenant_id
                       and row["dataset_id"] == dataset_id
                       and row["example_id"] == example_id)
            row["split"] = split
            row["metadata"] = {**row["metadata"], **json.loads(metadata_json)}
            return row
        raise AssertionError(sql)


class _Pool:
    def __init__(self) -> None:
        self.conn = _Connection()

    @asynccontextmanager
    async def acquire(self):
        yield self.conn


def _repository() -> tuple[AgentTraceRepository, _Connection]:
    pool = _Pool()
    return AgentTraceRepository(SimpleNamespace(_pool=pool, enabled=True)), pool.conn


def _payload(*, expected: str, revision: int) -> dict:
    return {
        "case_id": "kb-failure-case-1",
        "kb_dataset_id": "kb-1",
        "source": "kb-qa",
        "query": "What is the rule?",
        "expected_answer": expected,
        "observed_segment_ids": ["seg-1"],
        "source_versions": [],
        "source_versions_verified": False,
        "source_trace_id": "11111111-1111-4111-8111-111111111111",
        "kb_trace_id": "11111111-1111-4111-8111-111111111111",
        "query_fingerprint": "fingerprint",
        "observed_answer": "old answer",
        "failure_reason": "outdated",
        "expected_revision": revision,
    }


@pytest.mark.asyncio
async def test_kb_failure_revisions_are_immutable_idempotent_and_cas_guarded() -> None:
    repo, conn = _repository()
    first, created = await repo.save_kb_failure_revision(
        tenant_id="tenant-a", dataset_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        created_by="user-a", payload=_payload(expected="new answer", revision=0),
    )
    assert created is True
    assert first["metadata"]["case_revision"] == 1
    repeated, created = await repo.save_kb_failure_revision(
        tenant_id="tenant-a", dataset_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        created_by="user-a", payload=_payload(expected="new answer", revision=0),
    )
    assert created is False
    assert repeated["example_id"] == first["example_id"]
    assert len(conn.rows) == 1

    revised, created = await repo.save_kb_failure_revision(
        tenant_id="tenant-a", dataset_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        created_by="user-a", payload=_payload(expected="corrected answer", revision=1),
    )
    assert created is True
    assert revised["metadata"]["case_revision"] == 2
    assert revised["metadata"]["supersedes_example_id"] == first["example_id"]
    assert first["expected_output"] == {"answer": "new answer"}
    assert revised["expected_output"] == {"answer": "corrected answer"}
    with pytest.raises(EvalCaseRevisionConflict) as error:
        await repo.save_kb_failure_revision(
            tenant_id="tenant-a", dataset_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            created_by="user-a", payload=_payload(expected="stale answer", revision=1),
        )
    assert error.value.current_revision == 2
    assert len(conn.rows) == 2
    assert conn.advisory_calls >= 4


@pytest.mark.asyncio
async def test_review_only_latest_revision_keeps_kb_history_outside_eval_runs() -> None:
    repo, conn = _repository()
    dataset_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    first, _ = await repo.save_kb_failure_revision(
        tenant_id="tenant-a", dataset_id=dataset_id, created_by="user-a",
        payload=_payload(expected="old approved", revision=0),
    )
    await repo.review_kb_failure_example(
        tenant_id="tenant-a", dataset_id=dataset_id, example_id=first["example_id"],
        review_status="approved", reviewed_from="eval_console",
    )
    assert not any(is_runnable_dataset_example(row) for row in conn.rows)

    revised, _ = await repo.save_kb_failure_revision(
        tenant_id="tenant-a", dataset_id=dataset_id, created_by="user-a",
        payload=_payload(expected="new pending", revision=1),
    )
    assert not any(is_runnable_dataset_example(row) for row in conn.rows)
    with pytest.raises(EvalCaseRevisionConflict):
        await repo.review_kb_failure_example(
            tenant_id="tenant-a", dataset_id=dataset_id, example_id=first["example_id"],
            review_status="approved", reviewed_from="eval_console",
        )
    await repo.review_kb_failure_example(
        tenant_id="tenant-a", dataset_id=dataset_id, example_id=revised["example_id"],
        review_status="approved", reviewed_from="eval_console",
    )
    assert revised["metadata"]["case_revision"] == 2
    assert all(row["split"] == "review" for row in conn.rows)
    assert not any(is_runnable_dataset_example(row) for row in conn.rows)


@pytest.mark.asyncio
async def test_two_concurrent_revisions_from_same_base_cannot_both_commit() -> None:
    repo, conn = _repository()
    dataset_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    await repo.save_kb_failure_revision(
        tenant_id="tenant-a", dataset_id=dataset_id, created_by="user-a",
        payload=_payload(expected="base", revision=0),
    )
    results = await asyncio.gather(
        repo.save_kb_failure_revision(
            tenant_id="tenant-a", dataset_id=dataset_id, created_by="user-a",
            payload=_payload(expected="edit A", revision=1),
        ),
        repo.save_kb_failure_revision(
            tenant_id="tenant-a", dataset_id=dataset_id, created_by="user-b",
            payload=_payload(expected="edit B", revision=1),
        ),
        return_exceptions=True,
    )
    assert sum(isinstance(item, EvalCaseRevisionConflict) for item in results) == 1
    assert len(conn.rows) == 2
