"""Worker routing tests for the T1 dual-verb contract (PRD T1 items 3/4).

The verb is pinned on the queued row's metadata at claim time; the worker
must route strictly by it:

* only explicit reembed goes to in-place vector repair; recover replays the
  pinned incremental pipeline because an indexing crash can predate publication;
* retry and reprocess rebuild through atomic incremental publication from the
  submission-time snapshot;
* attachment bindings are never deleted before a replacement succeeds;
* the execution ledger (snapshot + manifest) survives all of it.
"""

from __future__ import annotations

import copy
import json
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import pytest
from knowledge_service.services.knowledge.worker import (
    KnowledgeIngestTask,
    KnowledgeWorker,
)

DATASET = {
    "dataset_id": "dataset-a",
    "tenant_id": "tenant-a",
    "index_config": {"chunking": {"mode": "automatic"}},
}


class DualVerbDatabase:
    def __init__(self, *, with_binding_cleanup: bool = False) -> None:
        self.dataset = copy.deepcopy(DATASET)
        self.documents: dict[str, dict[str, Any]] = {}
        self.executions: dict[str, dict[str, Any]] = {}
        self.status_writes: list[tuple[str, str, float | None]] = []
        self.binding_cleanups: list[tuple[str, str]] = []
        self.links: list[tuple[str, str]] = []
        self.records: list[dict[str, Any]] = []
        self.completions: list[dict[str, Any]] = []
        self.process_rules: dict[str, dict[str, Any]] = {}
        self.rule_pins: list[tuple[str, str]] = []
        if with_binding_cleanup:

            async def delete_document_attachment_bindings(
                document_id: str,
                dataset_id: str,
                *,
                connection: Any | None = None,
            ) -> None:
                del connection
                self.binding_cleanups.append((document_id, dataset_id))

            self.delete_document_attachment_bindings = (  # type: ignore[attr-defined]
                delete_document_attachment_bindings
            )

    def seed_document(
        self,
        document_id: str = "doc-a",
        *,
        metadata: dict[str, Any] | None = None,
        status: str = "waiting",
    ) -> None:
        self.documents[document_id] = {
            "document_id": document_id,
            "dataset_id": "dataset-a",
            "status": status,
            "size_bytes": 10,
            "content": "hello world",
            "metadata": dict(metadata or {}),
        }

    async def get_dataset(
        self, dataset_id: str, *, connection: Any | None = None
    ) -> dict[str, Any] | None:
        del connection
        return copy.deepcopy(self.dataset) if dataset_id == "dataset-a" else None

    async def get_document(
        self, document_id: str, *, connection: Any | None = None
    ) -> dict[str, Any] | None:
        del connection
        doc = self.documents.get(document_id)
        return dict(doc) if doc else None

    async def update_document_status(
        self,
        document_id: str,
        status: str,
        progress: float | None = None,
        error: str | None = None,
        *,
        connection: Any | None = None,
    ) -> None:
        del error, connection
        self.status_writes.append((document_id, status, progress))

    async def get_image_segments_by_document(
        self,
        document_id: str,
        *,
        connection: Any | None = None,
    ) -> list[dict[str, Any]]:
        del document_id, connection
        return []

    async def document_has_completed_segments(
        self, dataset_id: str, tenant_id: str, document_id: str,
    ) -> bool:
        assert (dataset_id, tenant_id, document_id) == ("dataset-a", "tenant-a", "doc-a")
        return False

    async def document_has_specialized_segments(
        self, dataset_id: str, tenant_id: str, document_id: str,
        *, connection: Any | None = None,
    ) -> bool:
        del connection
        assert (dataset_id, tenant_id, document_id) == ("dataset-a", "tenant-a", "doc-a")
        return False

    async def get_pipeline_execution(
        self, execution_id: str, *, connection: Any | None = None
    ) -> dict[str, Any] | None:
        del connection
        row = self.executions.get(execution_id)
        return dict(row) if row else None

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
        connection: Any | None = None,
    ) -> str:
        del connection, triggered_by
        execution_id = f"exec-{len(self.records) + 1}"
        self.records.append(
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
        self.executions[execution_id] = {
            "execution_id": execution_id,
            "document_id": document_id,
            "dataset_id": dataset_id,
            "action": action,
            "status": "running",
            "process_rule_id": process_rule_id,
            "input_snapshot": input_snapshot or {},
        }
        return execution_id

    async def record_process_rule(
        self,
        dataset_id: str,
        *,
        mode: str,
        rules: dict[str, Any],
        created_by: str | None = None,
        connection: Any | None = None,
    ) -> str:
        del connection, created_by
        for rule_id, row in self.process_rules.items():
            if (
                row["dataset_id"] == dataset_id
                and row["mode"] == mode
                and row["rules"] == rules
            ):
                return rule_id
        rule_id = f"rule-{len(self.process_rules) + 1}"
        self.process_rules[rule_id] = {
            "id": rule_id,
            "dataset_id": dataset_id,
            "mode": mode,
            "rules": rules,
        }
        return rule_id

    async def get_process_rule(
        self, process_rule_id: str, *, connection: Any | None = None
    ) -> dict[str, Any] | None:
        del connection
        row = self.process_rules.get(process_rule_id)
        return dict(row) if row else None

    async def pin_document_process_rule(
        self,
        document_id: str,
        process_rule_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        del connection
        if document_id not in self.documents:
            return False
        self.documents[document_id]["process_rule_id"] = process_rule_id
        self.rule_pins.append((document_id, process_rule_id))
        return True

    async def link_pipeline_execution(
        self,
        document_id: str,
        execution_id: str,
        *,
        connection: Any | None = None,
    ) -> bool:
        del connection
        self.links.append((document_id, execution_id))
        self.documents[document_id].setdefault("metadata", {})[
            "_document_pipeline_execution_id"
        ] = execution_id
        return True

    async def complete_pipeline_execution(
        self,
        execution_id: str,
        *,
        status: str,
        error: str | None = None,
        manifest: dict[str, Any] | list[Any] | None = None,
        connection: Any | None = None,
    ) -> bool:
        del connection
        self.completions.append(
            {
                "execution_id": execution_id,
                "status": status,
                "error": error,
                "manifest": manifest,
            }
        )
        return True


class DualVerbService:
    def __init__(self, database: DualVerbDatabase) -> None:
        self.db = database
        self.vector_store = SimpleNamespace(
            bm25_v2_enabled=True,
            document_has_points=AsyncMock(return_value=False),
            document_has_specialized_points=AsyncMock(return_value=False),
        )
        self.settings = SimpleNamespace(
            knowledge=SimpleNamespace(
                large_file_threshold=1024 * 1024 * 1024,
                pdf_split_enabled=True,
                pdf_split_max_size_bytes=1024,
                pdf_split_min_pages_per_part=1,
                ocr_strategy="hybrid",
                document_recovery_interval_seconds=1,
                document_stuck_threshold_minutes=1,
                document_worker_concurrency=1,
            )
        )
        self.reembed_calls: list[tuple[str, str]] = []
        self.ingest_calls: list[dict[str, Any]] = []

    async def reembed_document(self, dataset_id: str, document_id: str) -> list[str]:
        self.reembed_calls.append((dataset_id, document_id))
        return ["seg-repaired"]

    async def ingest_document(
        self,
        dataset_id: str,
        document_id: str,
        *,
        chunking_config_override: dict[str, Any] | None = None,
        index_config_override: dict[str, Any] | None = None,
    ) -> list[str]:
        del index_config_override
        self.ingest_calls.append(
            {
                "dataset_id": dataset_id,
                "document_id": document_id,
                "chunking_config_override": chunking_config_override,
            }
        )
        return ["seg-rebuilt"]


def make_worker(
    *,
    metadata: dict[str, Any] | None = None,
    with_binding_cleanup: bool = False,
) -> tuple[KnowledgeWorker, DualVerbDatabase, DualVerbService]:
    database = DualVerbDatabase(with_binding_cleanup=with_binding_cleanup)
    database.seed_document(metadata=metadata)
    service = DualVerbService(database)
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]
    return worker, database, service


def make_task() -> KnowledgeIngestTask:
    return KnowledgeIngestTask(dataset_id="dataset-a", document_id="doc-a")


def _snapshot_payload(
    *,
    index_config: dict[str, Any] | None = None,
    processing_mode: str = "text_only",
) -> dict[str, Any]:
    pinned_index_config = copy.deepcopy(
        index_config
        if index_config is not None
        else {"chunking": {"mode": "automatic"}}
    )
    return {
        "index_config": pinned_index_config,
        "chunking": copy.deepcopy(pinned_index_config.get("chunking", {})),
        "processing_mode": processing_mode,
    }


def _pin_process_rule(
    database: DualVerbDatabase,
    *,
    payload: dict[str, Any],
    rule_id: str = "rule-pinned",
) -> str:
    database.process_rules[rule_id] = {
        "id": rule_id,
        "dataset_id": "dataset-a",
        "mode": str(payload["chunking"].get("mode") or "automatic"),
        "rules": copy.deepcopy(payload),
    }
    database.documents["doc-a"]["process_rule_id"] = rule_id
    return rule_id


def _seed_replay_execution(
    database: DualVerbDatabase,
    *,
    action: str,
    index_config: dict[str, Any] | None = None,
    processing_mode: str = "text_only",
    execution_id: str = "exec-pinned",
) -> dict[str, Any]:
    payload = _snapshot_payload(
        index_config=index_config,
        processing_mode=processing_mode,
    )
    rule_id = _pin_process_rule(database, payload=payload)
    database.documents["doc-a"].setdefault("metadata", {}).update(
        {
            "_document_ingest_action": action,
            "_document_pipeline_execution_id": execution_id,
        }
    )
    database.executions[execution_id] = {
        "execution_id": execution_id,
        "document_id": "doc-a",
        "dataset_id": "dataset-a",
        "action": action,
        "status": "running",
        "process_rule_id": rule_id,
        "input_snapshot": copy.deepcopy(payload),
    }
    return payload


@pytest.mark.asyncio
async def test_reembed_verb_routes_to_vector_repair_only() -> None:
    worker, database, service = make_worker(
        metadata={"_document_ingest_action": "reembed"}
    )

    manifest = await worker._process_task(make_task(), connection=object())

    assert manifest == ["seg-repaired"]
    assert service.reembed_calls == [("dataset-a", "doc-a")]
    assert service.ingest_calls == []
    # A pure vector repair never re-enters the parsing stage.
    assert all(status != "parsing" for _, status, _ in database.status_writes)


@pytest.mark.asyncio
async def test_restore_generation_with_pending_lifecycle_marker_routes_to_reembed() -> None:
    """PRD T1 item 6: a disabled/archived document mid-restore carries the
    pending lifecycle marker plus the reembed verb; the worker must rebuild
    its vectors row-by-row, never reject the hidden state or re-ingest."""

    worker, database, service = make_worker(
        metadata={
            "_document_ingest_action": "reembed",
            "_document_lifecycle_reindex": {
                "status": "pending",
                "desired_enabled": True,
                "desired_archived": False,
            },
        }
    )
    database.documents["doc-a"]["enabled"] = False
    database.documents["doc-a"]["archived"] = True

    manifest = await worker._process_task(make_task(), connection=object())

    assert manifest == ["seg-repaired"]
    assert service.reembed_calls == [("dataset-a", "doc-a")]
    assert service.ingest_calls == []


@pytest.mark.asyncio
async def test_recover_from_indexing_replays_pinned_incremental_pipeline() -> None:
    worker, database, service = make_worker(
        metadata={
            "_document_ingest_action": "recover",
            "_document_recover_stage": "indexing",
        }
    )
    _seed_replay_execution(database, action="recover")

    manifest = await worker._process_task(make_task(), connection=object())

    assert manifest == ["seg-rebuilt"]
    assert service.reembed_calls == []
    assert service.ingest_calls == [{
        "dataset_id": "dataset-a",
        "document_id": "doc-a",
        "chunking_config_override": {"mode": "automatic"},
    }]
    assert ("doc-a", "parsing", 5) in database.status_writes


@pytest.mark.asyncio
async def test_recover_from_parsing_redoes_full_pipeline() -> None:
    worker, database, service = make_worker(
        metadata={
            "_document_ingest_action": "recover",
            "_document_recover_stage": "parsing",
        }
    )
    _seed_replay_execution(database, action="recover")

    manifest = await worker._process_task(make_task(), connection=object())

    assert manifest == ["seg-rebuilt"]
    assert service.reembed_calls == []
    assert service.ingest_calls == [
        {
            "dataset_id": "dataset-a",
            "document_id": "doc-a",
            "chunking_config_override": {"mode": "automatic"},
        }
    ]
    assert ("doc-a", "parsing", 5) in database.status_writes


@pytest.mark.asyncio
async def test_pending_restore_uses_pinned_text_candidate_even_for_scanned_source() -> None:
    worker, database, service = make_worker(
        metadata={
            "_document_pending_restore_version": 3,
            "processing_mode": "scanned",
        }
    )
    _seed_replay_execution(database, action="reprocess", processing_mode="text_only")
    database.documents["doc-a"]["size_bytes"] = 100 * 1024 * 1024

    manifest = await worker._process_task(make_task(), connection=object())

    assert manifest == ["seg-rebuilt"]
    assert service.reembed_calls == []
    assert service.ingest_calls == [{
        "dataset_id": "dataset-a",
        "document_id": "doc-a",
        "chunking_config_override": {"mode": "automatic"},
    }]


@pytest.mark.asyncio
async def test_replay_snapshot_pins_chunking_config_into_standard_engine() -> None:
    worker, database, service = make_worker(
        metadata={
            "_document_ingest_action": "reprocess",
            "_document_pipeline_execution_id": "exec-pinned",
        }
    )
    _seed_replay_execution(
        database,
        action="reprocess",
        index_config={
            "chunking": {"mode": "automatic", "chunk_size": 300},
            "snapshot_marker": "submitted",
        },
    )
    database.dataset["index_config"] = {
        "chunking": {"mode": "automatic", "chunk_size": 999},
        "snapshot_marker": "live-drift",
        "retrieval": {
            "lexical": {
                "active_version": "bm25_v2",
                "bm25_v2": {"shadow_write_enabled": True},
            }
        },
    }

    manifest = await worker._process_task(make_task(), connection=object())

    assert manifest == ["seg-rebuilt"]
    assert service.ingest_calls == [
        {
            "dataset_id": "dataset-a",
            "document_id": "doc-a",
            "chunking_config_override": {"mode": "automatic", "chunk_size": 300},
        }
    ]
    # The replay enters the parsing stage before chunking from the snapshot.
    assert ("doc-a", "parsing", 5) in database.status_writes


@pytest.mark.asyncio
async def test_replayed_preparing_ingest_keeps_original_execution_config() -> None:
    worker, database, service = make_worker(
        metadata={"_document_pipeline_execution_id": "exec-pinned"},
    )
    _seed_replay_execution(
        database,
        action="ingest",
        index_config={"chunking": {"mode": "automatic", "chunk_size": 300}},
    )
    database.executions["exec-pinned"]["manifest"] = {
        "preparing_replayed_after_restart": True,
    }
    database.dataset["index_config"] = {
        "chunking": {"mode": "automatic", "chunk_size": 999},
    }

    manifest = await worker._process_task(make_task(), connection=object())

    assert manifest == ["seg-rebuilt"]
    assert service.ingest_calls == [{
        "dataset_id": "dataset-a",
        "document_id": "doc-a",
        "chunking_config_override": {"mode": "automatic", "chunk_size": 300},
    }]


@pytest.mark.asyncio
async def test_replayed_auto_scanned_ingest_reuses_durable_detected_mode() -> None:
    worker, database, _service = make_worker(metadata={
        "_document_pipeline_execution_id": "exec-pinned",
        "processing_mode": "scanned",
    })
    _seed_replay_execution(database, action="ingest", processing_mode="auto")
    database.executions["exec-pinned"]["manifest"] = {
        "preparing_replayed_after_restart": True,
    }
    worker._process_scanned = AsyncMock(return_value=None)
    worker._process_with_auto_detection = AsyncMock(return_value=None)

    await worker._process_task(make_task(), connection=object())

    worker._process_scanned.assert_awaited_once()
    worker._process_with_auto_detection.assert_not_awaited()


@pytest.mark.asyncio
async def test_prepare_replayed_ingest_uses_pinned_config_before_dispatch() -> None:
    worker, database, _service = make_worker(metadata={
        "_document_pipeline_execution_id": "exec-pinned",
    })
    _seed_replay_execution(database, action="ingest")
    database.executions["exec-pinned"]["manifest"] = {
        "preparing_replayed_after_restart": True,
    }
    database.documents["doc-a"]["status"] = "parsing"
    database.dataset["index_config"] = "corrupt-live-config"

    await worker._prepare_document_generation(make_task(), connection=object())


@pytest.mark.asyncio
async def test_prepare_replay_ignores_malformed_live_index_config() -> None:
    worker, database, _service = make_worker(
        metadata={"_document_ingest_action": "reprocess"}
    )
    _seed_replay_execution(database, action="reprocess")
    database.dataset["index_config"] = "corrupt-live-config"
    database.documents["doc-a"]["status"] = "parsing"

    await worker._prepare_document_generation(make_task(), connection=object())


@pytest.mark.asyncio
async def test_replay_snapshot_mode_beats_document_metadata_mode() -> None:
    worker, database, service = make_worker(
        metadata={
            "_document_ingest_action": "reprocess",
            "_document_pipeline_execution_id": "exec-pinned",
            "processing_mode": "multimodal",
        }
    )
    _seed_replay_execution(
        database,
        action="reprocess",
        processing_mode="text_only",
    )

    await worker._process_task(make_task(), connection=object())

    assert service.ingest_calls == [
        {
            "dataset_id": "dataset-a",
            "document_id": "doc-a",
            "chunking_config_override": {"mode": "automatic"},
        }
    ]


@pytest.mark.asyncio
async def test_retry_verb_rebuilds_from_snapshot_without_pre_sweep() -> None:
    worker, database, service = make_worker(
        metadata={"_document_ingest_action": "retry"}
    )
    _seed_replay_execution(
        database,
        action="retry",
        index_config={
            "chunking": {"mode": "automatic", "chunk_size": 300},
            "snapshot_marker": "submitted",
        },
    )
    database.dataset["index_config"] = {
        "chunking": {"mode": "automatic", "chunk_size": 999},
        "snapshot_marker": "live-drift",
    }

    manifest = await worker._process_task(make_task(), connection=object())

    assert manifest == ["seg-rebuilt"]
    assert service.ingest_calls == [
        {
            "dataset_id": "dataset-a",
            "document_id": "doc-a",
            "chunking_config_override": {"mode": "automatic", "chunk_size": 300},
        }
    ]
    assert service.reembed_calls == []


@pytest.mark.asyncio
async def test_retry_verb_without_persisted_snapshot_fails_closed() -> None:
    worker, _database, service = make_worker(
        metadata={"_document_ingest_action": "retry"}
    )

    with pytest.raises(RuntimeError, match="execution id is missing"):
        await worker._process_task(make_task(), connection=object())

    assert service.ingest_calls == []


@pytest.mark.asyncio
async def test_reprocess_and_retry_never_delete_serving_bindings_before_publish() -> None:
    for verb in ("reprocess", "retry"):
        worker, database, _service = make_worker(
            metadata={"_document_ingest_action": verb},
            with_binding_cleanup=True,
        )
        _seed_replay_execution(database, action=verb)

        await worker._process_task(make_task(), connection=object())

        assert database.binding_cleanups == [], verb


@pytest.mark.asyncio
async def test_ingest_verb_never_touches_binding_cleanup() -> None:
    worker, database, service = make_worker(
        metadata={}, with_binding_cleanup=True
    )

    await worker._process_task(make_task(), connection=object())

    assert database.binding_cleanups == []
    assert service.ingest_calls and service.reembed_calls == []


@pytest.mark.asyncio
async def test_unknown_verb_normalizes_to_default_ingest() -> None:
    worker, _database, service = make_worker(
        metadata={"_document_ingest_action": "explode"}
    )

    await worker._process_task(make_task(), connection=object())

    assert service.ingest_calls == [
        {
            "dataset_id": "dataset-a",
            "document_id": "doc-a",
            "chunking_config_override": None,
        }
    ]
    assert service.reembed_calls == []


@pytest.mark.asyncio
async def test_plain_ingest_with_linked_execution_runs_live_dispatch() -> None:
    """Addendum §1-T1.3: replay semantics belong to the VERB, not to the
    presence of a ledger row. A first-generation ingest carries a manifest
    ledger row but must run live mode dispatch — never the pinned-snapshot
    shortcut (which would pre-empt auto-detect/scanned/large/hierarchical)."""

    worker, database, service = make_worker(
        metadata={
            "_document_ingest_action": "ingest",
            "_document_pipeline_execution_id": "exec-live",
        }
    )
    database.executions["exec-live"] = {
        "execution_id": "exec-live",
        "action": "ingest",
        "status": "running",
        "input_snapshot": {
            "chunking": {"mode": "automatic", "chunk_size": 999},
            "processing_mode": "text_only",
        },
    }

    await worker._process_task(make_task(), connection=object())

    # No snapshot override reaches the engine: live dispatch owns the run.
    assert service.ingest_calls == [
        {
            "dataset_id": "dataset-a",
            "document_id": "doc-a",
            "chunking_config_override": None,
        }
    ]


@pytest.mark.asyncio
async def test_retry_with_linked_execution_replays_pinned_dispatch() -> None:
    """Retry must replay its durable snapshot rather than live configuration."""

    worker, database, service = make_worker(
        metadata={
            "_document_ingest_action": "retry",
            "_document_pipeline_execution_id": "exec-live",
        }
    )
    _seed_replay_execution(
        database,
        action="retry",
        execution_id="exec-live",
        index_config={"chunking": {"mode": "automatic", "chunk_size": 321}},
    )
    database.dataset["index_config"] = {
        "chunking": {"mode": "automatic", "chunk_size": 999}
    }

    await worker._process_task(make_task(), connection=object())

    assert service.ingest_calls == [
        {
            "dataset_id": "dataset-a",
            "document_id": "doc-a",
            "chunking_config_override": {"mode": "automatic", "chunk_size": 321},
        }
    ]


@pytest.mark.asyncio
async def test_reprocess_scanned_keeps_parse_stage_processor() -> None:
    """A scanned document replayed via reprocess must keep its parse-stage
    processor (VLM OCR), never route to the incremental engine, even though a
    chunking snapshot is pinned — the snapshot stays in the ledger for audit."""

    worker, database, service = make_worker(
        metadata={
            "_document_ingest_action": "reprocess",
            "_document_pipeline_execution_id": "exec-pinned",
        }
    )
    pinned_index_config = {
        "chunking": {"mode": "automatic", "chunk_size": 300},
        "snapshot_marker": "submitted",
    }
    _seed_replay_execution(
        database,
        action="reprocess",
        index_config=pinned_index_config,
        processing_mode="scanned",
    )
    database.dataset["index_config"] = {
        "chunking": {"mode": "custom", "chunk_size": 999},
        "snapshot_marker": "live-drift",
    }
    scanned_calls: list[tuple[Any, dict[str, Any], dict[str, Any] | None]] = []

    async def fake_scanned(
        task,
        doc,
        *,
        index_config_override=None,
        stage_receipt=None,
    ):
        del stage_receipt  # PRD T9-1 attribution channel; not under test here
        scanned_calls.append((task, doc, index_config_override))

    worker._process_scanned = fake_scanned  # type: ignore[method-assign]

    await worker._process_task(make_task(), connection=object())

    assert len(scanned_calls) == 1
    assert scanned_calls[0][2] == pinned_index_config
    assert service.ingest_calls == []
    assert service.reembed_calls == []


@pytest.mark.asyncio
async def test_reprocess_large_file_keeps_streaming_processor() -> None:
    """A large replay keeps streaming but receives the pinned config."""

    worker, database, service = make_worker(
        metadata={
            "_document_ingest_action": "reprocess",
            "_document_pipeline_execution_id": "exec-pinned",
        }
    )
    database.documents["doc-a"]["size_bytes"] = 2 * 1024 * 1024 * 1024
    pinned_index_config = {
        "chunking": {"mode": "automatic", "chunk_size": 300},
        "snapshot_marker": "submitted",
    }
    _seed_replay_execution(
        database,
        action="reprocess",
        index_config=pinned_index_config,
    )
    database.dataset["index_config"] = {
        "chunking": {"mode": "custom", "chunk_size": 999},
        "snapshot_marker": "live-drift",
    }
    large_calls: list[
        tuple[Any, dict[str, Any], Any, dict[str, Any] | None]
    ] = []

    async def fake_large(
        task,
        doc,
        mode,
        *,
        index_config_override=None,
        stage_receipt=None,
    ):
        del stage_receipt  # PRD T9-1 attribution channel; not under test here
        large_calls.append((task, doc, mode, index_config_override))

    worker._process_large_file = fake_large  # type: ignore[method-assign]

    await worker._process_task(make_task(), connection=object())

    assert len(large_calls) == 1
    assert large_calls[0][3] == pinned_index_config
    assert service.ingest_calls == []
    assert service.reembed_calls == []


@pytest.mark.asyncio
async def test_reprocess_hierarchical_dispatch_uses_pinned_config() -> None:
    worker, database, service = make_worker(
        metadata={"_document_ingest_action": "reprocess"}
    )
    pinned_index_config = {
        "chunking": {
            "mode": "hierarchical",
            "parent_chunk_size": 6000,
            "child_chunk_size": 1600,
        },
        "snapshot_marker": "submitted",
    }
    _seed_replay_execution(
        database,
        action="reprocess",
        index_config=pinned_index_config,
    )
    database.dataset["index_config"] = {
        "chunking": {"mode": "custom", "chunk_size": 999},
        "snapshot_marker": "live-drift",
    }
    worker.hierarchical_indexer = object()  # type: ignore[assignment]
    hierarchical_calls: list[dict[str, Any] | None] = []

    async def fake_hierarchical(
        task,
        doc,
        mode,
        *,
        index_config_override=None,
        stage_receipt=None,
    ):
        del task, doc, mode, stage_receipt
        hierarchical_calls.append(index_config_override)

    worker._process_with_hierarchical_indexer = (  # type: ignore[method-assign]
        fake_hierarchical
    )

    await worker._process_task(make_task(), connection=object())

    assert hierarchical_calls == [pinned_index_config]
    assert service.ingest_calls == []


@pytest.mark.asyncio
async def test_ensure_pipeline_execution_keeps_route_linked_row() -> None:
    worker, database, _service = make_worker()
    database.executions["exec-route"] = {
        "execution_id": "exec-route",
        "action": "reprocess",
        "status": "running",
        "input_snapshot": {"chunking": {"mode": "automatic"}},
    }

    execution_id = await worker._ensure_pipeline_execution(
        make_task(), "reprocess", "exec-route", connection=None
    )

    assert execution_id == "exec-route"
    assert database.records == []


@pytest.mark.asyncio
async def test_ensure_pipeline_execution_opens_ledger_row_for_crash_recovery() -> None:
    worker, database, _service = make_worker()
    payload = _snapshot_payload()
    rule_id = _pin_process_rule(database, payload=payload)

    execution_id = await worker._ensure_pipeline_execution(
        make_task(), "recover", "", connection=None
    )

    assert execution_id == "exec-1"
    assert database.records == [
        {
            "execution_id": "exec-1",
            "document_id": "doc-a",
            "dataset_id": "dataset-a",
            "action": "recover",
            "trigger_source": "recover",
            "process_rule_id": rule_id,
            "input_snapshot": payload,
        }
    ]
    assert database.links == [("doc-a", "exec-1")]


@pytest.mark.asyncio
async def test_ensure_pipeline_execution_closed_replay_row_fails_closed() -> None:
    """A closed replay row must never be replaced from live config."""

    worker, database, _service = make_worker()
    database.executions["exec-closed"] = {
        "execution_id": "exec-closed",
        "action": "reprocess",
        "status": "completed",
        "input_snapshot": {"chunking": {"mode": "automatic"}},
    }

    with pytest.raises(RuntimeError, match="missing or no longer running"):
        await worker._ensure_pipeline_execution(
            make_task(), "reprocess", "exec-closed", connection=None
        )

    assert database.records == []
    assert database.links == []


@pytest.mark.asyncio
async def test_ensure_pipeline_execution_never_rebuilds_retry_from_live_config() -> None:
    worker, database, _service = make_worker()

    with pytest.raises(RuntimeError, match="retry generation is missing"):
        await worker._ensure_pipeline_execution(
            make_task(), "retry", "", connection=object()
        )

    assert database.records == []
    assert database.links == []


@pytest.mark.asyncio
async def test_load_replay_snapshot_fails_when_execution_missing() -> None:
    worker, database, _service = make_worker()
    task = make_task()
    document = database.documents["doc-a"]

    with pytest.raises(RuntimeError, match="execution id is missing"):
        await worker._load_replay_snapshot(
            task,
            "",
            document,
            expected_action="reprocess",
        )
    with pytest.raises(RuntimeError, match="execution snapshot is missing"):
        await worker._load_replay_snapshot(
            task,
            "exec-gone",
            document,
            expected_action="reprocess",
        )


@pytest.mark.asyncio
async def test_generation_open_records_and_pins_process_rule() -> None:
    """PRD T1 item 7: a generation the worker opens records the rule
    snapshot of the config that actually builds it (canonical dialect), and
    the document is pinned to that immutable row."""

    worker, database, _service = make_worker()

    execution_id = await worker._ensure_pipeline_execution(
        make_task(), "ingest", "", connection=None
    )

    assert execution_id == "exec-1"
    assert database.records[0]["process_rule_id"] == "rule-1"
    assert database.process_rules == {
        "rule-1": {
            "id": "rule-1",
            "dataset_id": "dataset-a",
            "mode": "automatic",
            "rules": {
                "index_config": {"chunking": {"mode": "automatic"}},
                "chunking": {"mode": "automatic"},
                "processing_mode": "text_only",
            },
        }
    }
    assert database.rule_pins == [("doc-a", "rule-1")]
    assert database.documents["doc-a"]["process_rule_id"] == "rule-1"


@pytest.mark.asyncio
@pytest.mark.parametrize("special_path", ["hierarchical", "multimodal", "auto"])
async def test_special_generation_refuses_missing_rule_before_publication(
    special_path: str,
) -> None:
    worker, database, _service = make_worker()
    if special_path == "hierarchical":
        database.dataset["index_config"] = {"chunking": {"mode": "hierarchical"}}
    else:
        database.documents["doc-a"]["metadata"]["processing_mode"] = special_path
    database.record_process_rule = AsyncMock(side_effect=RuntimeError("rule store offline"))

    with pytest.raises(RuntimeError, match="durable process-rule snapshot"):
        await worker._ensure_pipeline_execution(
            make_task(), "ingest", "", connection=None,
        )

    assert database.records == []
    assert database.rule_pins == []


@pytest.mark.asyncio
async def test_reembed_generation_records_no_process_rule() -> None:
    """reembed repairs vectors at existing segment identity and never runs
    the chunking dialect, so no rule row is recorded or pinned."""

    worker, database, _service = make_worker()

    execution_id = await worker._ensure_pipeline_execution(
        make_task(), "reembed", "", connection=None
    )

    assert execution_id == "exec-1"
    assert database.records[0]["process_rule_id"] is None
    assert database.process_rules == {}
    assert database.rule_pins == []


@pytest.mark.asyncio
async def test_reprocess_without_execution_snapshot_fails_closed() -> None:

    worker, database, service = make_worker(
        metadata={
            "_document_ingest_action": "reprocess",
            "_document_pipeline_execution_id": "exec-gone",
        }
    )
    database.process_rules["rule-pinned"] = {
        "id": "rule-pinned",
        "dataset_id": "dataset-a",
        "mode": "custom",
        "rules": {
            "chunking": {"mode": "custom", "chunk_size": 123},
            "processing_mode": "text_only",
        },
    }
    database.documents["doc-a"]["process_rule_id"] = "rule-pinned"

    with pytest.raises(RuntimeError, match="execution snapshot is missing"):
        await worker._process_task(make_task(), connection=object())

    assert service.ingest_calls == []


@pytest.mark.asyncio
async def test_reprocess_fallback_decodes_raw_json_rule_payload() -> None:
    """A valid raw-json process-rule row still cross-checks successfully."""

    worker, database, service = make_worker(
        metadata={
            "_document_ingest_action": "reprocess",
            "_document_pipeline_execution_id": "exec-gone",
        }
    )
    payload = _seed_replay_execution(
        database,
        action="reprocess",
        index_config={"chunking": {"mode": "custom", "chunk_size": 77}},
        execution_id="exec-gone",
    )
    database.process_rules["rule-pinned"]["rules"] = json.dumps(payload)

    await worker._process_task(make_task(), connection=object())

    assert service.ingest_calls == [
        {
            "dataset_id": "dataset-a",
            "document_id": "doc-a",
            "chunking_config_override": {"mode": "custom", "chunk_size": 77},
        }
    ]


@pytest.mark.asyncio
async def test_reprocess_corrupt_execution_snapshot_fails_closed() -> None:
    worker, database, service = make_worker(
        metadata={"_document_ingest_action": "reprocess"}
    )
    _seed_replay_execution(database, action="reprocess")
    database.executions["exec-pinned"]["input_snapshot"] = "{not-json"

    with pytest.raises(RuntimeError, match="invalid JSON"):
        await worker._process_task(make_task(), connection=object())

    assert service.ingest_calls == []


@pytest.mark.asyncio
async def test_reprocess_corrupt_process_rule_snapshot_fails_closed() -> None:
    worker, database, service = make_worker(
        metadata={"_document_ingest_action": "reprocess"}
    )
    _seed_replay_execution(database, action="reprocess")
    database.process_rules["rule-pinned"]["rules"] = "{not-json"

    with pytest.raises(RuntimeError, match="invalid JSON"):
        await worker._process_task(make_task(), connection=object())

    assert service.ingest_calls == []


@pytest.mark.asyncio
async def test_reprocess_disagreeing_snapshots_fail_closed() -> None:
    worker, database, service = make_worker(
        metadata={"_document_ingest_action": "reprocess"}
    )
    _seed_replay_execution(database, action="reprocess")
    database.process_rules["rule-pinned"]["rules"]["index_config"]["chunking"][
        "chunk_size"
    ] = 999
    database.process_rules["rule-pinned"]["rules"]["chunking"]["chunk_size"] = 999

    with pytest.raises(RuntimeError, match="snapshots disagree"):
        await worker._process_task(make_task(), connection=object())

    assert service.ingest_calls == []


@pytest.mark.asyncio
async def test_finish_pipeline_execution_writes_manifest_ledger() -> None:
    worker, database, _service = make_worker()

    await worker._finish_pipeline_execution(
        "exec-1", status="completed", manifest=["s1", "s2"]
    )
    await worker._finish_pipeline_execution("", status="completed", manifest=None)

    assert database.completions == [
        {
            "execution_id": "exec-1",
            "status": "completed",
            "error": None,
            "manifest": {"segment_ids": ["s1", "s2"]},
        }
    ]
