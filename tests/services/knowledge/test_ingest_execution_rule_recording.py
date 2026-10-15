"""Unit tests for the route-side execution + process-rule recorder.

PRD T1 item 7 / addendum §1: route-submitted verbs record their replay
snapshot AND their immutable rule snapshot at submission time, from the same
config the operator saw:

* non-reembed verbs record a canonical-dialect rule row
  ({"chunking", "processing_mode"}) and pin it on the document;
* reembed stays exempt — vector repair runs no chunking dialect;
* reprocess/recover/retry snapshot persistence failures reject submission with
  503;
* the recorded execution row carries the rule id.
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from fastapi import HTTPException
from knowledge_service.api.routes.knowledge import _record_ingest_execution


class RecordingDatabase:
    def __init__(self, *, rule_failure: Exception | None = None) -> None:
        self.dataset: dict[str, Any] = {
            "dataset_id": "dataset-a",
            "index_config": {"chunking": {"mode": "custom", "chunk_size": 512}},
        }
        self.document: dict[str, Any] = {
            "document_id": "doc-a",
            "metadata": {"processing_mode": "text_only"},
        }
        self.executions: list[dict[str, Any]] = []
        self.rules: list[dict[str, Any]] = []
        self.pins: list[tuple[str, str]] = []
        self.rule_failure = rule_failure

    async def get_dataset(self, dataset_id: str) -> dict[str, Any] | None:
        return dict(self.dataset) if dataset_id == "dataset-a" else None

    async def get_document(self, document_id: str) -> dict[str, Any] | None:
        return dict(self.document) if document_id == "doc-a" else None

    async def record_pipeline_execution(
        self,
        document_id: str,
        dataset_id: str,
        *,
        action: str,
        trigger_source: str = "api",
        triggered_by: str | None = None,
        process_rule_id: str | None = None,
        input_snapshot: dict[str, Any] | None = None,
    ) -> str:
        del triggered_by
        execution_id = f"exec-{len(self.executions) + 1}"
        self.executions.append(
            {
                "execution_id": execution_id,
                "document_id": document_id,
                "dataset_id": dataset_id,
                "action": action,
                "trigger_source": trigger_source,
                "process_rule_id": process_rule_id,
                "input_snapshot": input_snapshot or {},
            }
        )
        return execution_id

    async def record_process_rule(
        self,
        dataset_id: str,
        *,
        mode: str,
        rules: dict[str, Any],
        created_by: str | None = None,
    ) -> str:
        if self.rule_failure is not None:
            raise self.rule_failure
        rule_id = f"rule-{len(self.rules) + 1}"
        self.rules.append(
            {
                "id": rule_id,
                "dataset_id": dataset_id,
                "mode": mode,
                "rules": rules,
                "created_by": created_by,
            }
        )
        return rule_id

    async def pin_document_process_rule(
        self, document_id: str, process_rule_id: str, *, idle_only: bool = False,
    ) -> bool:
        del idle_only
        self.pins.append((document_id, process_rule_id))
        return True


def _svc(database: RecordingDatabase) -> SimpleNamespace:
    return SimpleNamespace(db=database)


@pytest.mark.asyncio
async def test_route_submission_records_and_pins_rule_snapshot() -> None:
    database = RecordingDatabase()

    execution_id = await _record_ingest_execution(
        _svc(database),
        dataset_id="dataset-a",
        document_id="doc-a",
        action="reprocess",
    )

    assert execution_id == "exec-1"
    assert database.rules == [
        {
            "id": "rule-1",
            "dataset_id": "dataset-a",
            "mode": "custom",
            "rules": {
                "index_config": {
                    "chunking": {"mode": "custom", "chunk_size": 512}
                },
                "chunking": {"mode": "custom", "chunk_size": 512},
                "processing_mode": "text_only",
            },
            "created_by": None,
        }
    ]
    assert database.pins == [("doc-a", "rule-1")]
    assert database.executions[0]["process_rule_id"] == "rule-1"
    # The input snapshot keeps its own (already tested) contract untouched.
    assert database.executions[0]["input_snapshot"]["chunking"] == {
        "mode": "custom",
        "chunk_size": 512,
    }
    assert database.executions[0]["input_snapshot"]["index_config"] == {
        "chunking": {"mode": "custom", "chunk_size": 512}
    }


@pytest.mark.asyncio
async def test_route_reembed_submission_records_no_rule() -> None:
    database = RecordingDatabase()

    execution_id = await _record_ingest_execution(
        _svc(database),
        dataset_id="dataset-a",
        document_id="doc-a",
        action="reembed",
    )

    assert execution_id == "exec-1"
    assert database.rules == []
    assert database.pins == []
    assert database.executions[0]["process_rule_id"] is None


@pytest.mark.asyncio
async def test_route_submission_defaults_mode_when_chunking_absent() -> None:
    database = RecordingDatabase()
    database.dataset["index_config"] = {}

    await _record_ingest_execution(
        _svc(database),
        dataset_id="dataset-a",
        document_id="doc-a",
        action="ingest",
    )

    assert database.rules[0]["mode"] == "automatic"
    assert database.rules[0]["rules"] == {
        "index_config": {},
        "chunking": {},
        "processing_mode": "text_only",
    }


@pytest.mark.asyncio
@pytest.mark.parametrize("action", ["reprocess", "recover", "retry"])
async def test_route_rule_failure_rejects_replay_submission(action: str) -> None:
    database = RecordingDatabase(rule_failure=RuntimeError("rule store offline"))

    with pytest.raises(HTTPException) as exc_info:
        await _record_ingest_execution(
            _svc(database),
            dataset_id="dataset-a",
            document_id="doc-a",
            action=action,
        )

    assert exc_info.value.status_code == 503
    assert database.executions == []
    assert database.pins == []
