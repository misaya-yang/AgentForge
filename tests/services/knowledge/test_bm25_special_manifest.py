"""Connection-bound special-publication phase and proof rules (no live DB)."""

from __future__ import annotations

import json
from typing import Any

import pytest
from knowledge_service.persistence.bm25_v2_lifecycle import (
    Bm25V2LifecycleStore,
    point_ids_sha256,
    source_text_sha256,
)
from knowledge_service.persistence.datasets import DatasetPersistenceMixin

SOURCE_HASH = "a" * 64
PLAN_HASH = "b" * 64


class RecordingConnection:
    def __init__(self) -> None:
        self.dataset_revision = -1007
        self.document: dict[str, Any] = {
            "status": "indexing",
            "metadata": {"_document_pipeline_execution_id": "execution-a"},
        }
        self.execution: dict[str, Any] = {
            "execution_id": "execution-a",
            "document_id": "document-a",
            "dataset_id": "dataset-a",
            "status": "running",
            "manifest": {},
        }

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        if "SELECT content_revision FROM datasets" in query:
            return {"content_revision": self.dataset_revision}
        if "SELECT status, metadata FROM documents" in query:
            return self.document
        if "SELECT status, manifest FROM document_pipeline_executions" in query:
            return self.execution
        if "UPDATE document_pipeline_executions" in query and "SET status = $2" in query:
            if self.execution["status"] != "running":
                return None
            self.execution["status"] = args[1]
            self.execution["error"] = args[2]
            return {"execution_id": args[0]}
        raise AssertionError(f"unexpected fetchrow: {query}")

    async def execute(self, query: str, *args: Any) -> None:
        if "SET manifest = jsonb_set" in query:
            self.execution["manifest"]["special_publication"] = json.loads(args[1])
        elif "SET metadata = jsonb_set" in query:
            self.document["metadata"]["_special_publication_generation_id"] = args[2]
        elif "- '_special_publication_generation_id'" in query:
            self.document["metadata"].pop("_special_publication_generation_id", None)
        else:
            raise AssertionError(f"unexpected execute: {query}")


class BindingConnection(RecordingConnection):
    def __init__(self) -> None:
        super().__init__()
        self.revision = -1007
        self.other_execution = {
            "execution_id": "execution-b",
            "document_id": "document-b",
            "dataset_id": "dataset-a",
            "status": "running",
            "manifest": {"special_publication": {
                "generation_id": "generation-b", "source_hash": SOURCE_HASH,
                "plan_hash": None, "planned_object_keys": [],
                "phase": "preparing", "collections": {}, "objects": {},
            }},
        }

    def is_in_transaction(self) -> bool:
        return True

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        if "SELECT content_revision FROM datasets" in query:
            return {"content_revision": self.revision}
        if "SELECT document_id FROM document_pipeline_executions" in query:
            return {"document_id": "document-a"}
        if "SELECT metadata FROM documents" in query:
            return self.document
        return await super().fetchrow(query, *args)

    async def fetchval(self, query: str, *_args: Any) -> str | None:
        assert "publication_revision" in query
        other = self.other_execution["manifest"]["special_publication"]
        return (
            "execution-b"
            if self.other_execution["status"] == "running"
            and other.get("phase") in {
                "preparing", "prepared", "points_written", "committed"
            }
            and other.get("publication_revision") == 1007
            else None
        )

    async def fetch(self, query: str, *_args: Any) -> list[dict[str, Any]]:
        assert "? 'publication_revision'" in query
        rows = []
        for execution, metadata in (
            (self.execution, self.document["metadata"]),
            (self.other_execution, {
                "_document_pipeline_execution_id": "execution-b",
                "_special_publication_generation_id": "generation-b",
            }),
        ):
            special = execution["manifest"]["special_publication"]
            if (
                execution["status"] == "running"
                and special.get("phase") in {
                    "preparing", "prepared", "points_written", "committed"
                }
                and "publication_revision" in special
            ):
                rows.append({
                    **execution, "document_metadata": metadata,
                    "content_revision": self.revision, "tenant_id": "tenant-a",
                })
        return rows


def _store() -> DatasetPersistenceMixin:
    store = DatasetPersistenceMixin()
    store._pool = object()  # connection-bound test; no pool or database is used
    return store


@pytest.mark.asyncio
async def test_bm25_authority_uses_supplied_uncommitted_connection() -> None:
    class NoAcquirePool:
        def acquire(self) -> None:
            raise AssertionError("publisher authority opened a second connection")

    class CandidateConnection:
        async def fetchrow(self, _query: str, *_args: Any) -> dict[str, Any]:
            return {
                "dataset_id": "dataset-a", "tenant_id": "tenant-a",
                "collection_name": "collection-a", "content_revision": -7,
                "index_config": {},
            }

        async def fetch(self, _query: str, *_args: Any) -> list[dict[str, Any]]:
            return [{"point_id": "candidate-a", "text": "uncommitted candidate"}]

    store = Bm25V2LifecycleStore(NoAcquirePool())
    authority = await store.authority_snapshot(
        collection_name="collection-a", tenant_id="tenant-a",
        dataset_id="dataset-a", connection=CandidateConnection(),
    )
    assert authority.content_revision == -7
    assert authority.point_ids_sha256 == point_ids_sha256(["candidate-a"])
    assert authority.source_text_sha256 == source_text_sha256(
        [("candidate-a", "uncommitted candidate")]
    )


@pytest.mark.asyncio
async def test_negative_revision_selects_bound_owner_among_two_preparing() -> None:
    store = _store()
    connection = BindingConnection()
    await store.record_special_publication_manifest(
        "execution-a", "document-a", "dataset-a", "generation-a",
        source_hash=SOURCE_HASH, planned_object_keys=[], connection=connection,
    )
    with pytest.raises(RuntimeError, match="no unique running owner"):
        await store.get_active_special_publication_for_dataset(
            "dataset-a", connection=connection,
        )
    bound = await store.bind_special_publication_revision(
        "execution-a", "dataset-a", -1007, connection=connection,
    )
    assert bound["publication_revision"] == 1007
    assert await store.bind_special_publication_revision(
        "execution-a", "dataset-a", -1007, connection=connection,
    ) == bound
    owner = await store.get_active_special_publication_for_dataset(
        "dataset-a", connection=connection,
    )
    assert owner["execution_id"] == "execution-a"
    assert owner["tenant_id"] == "tenant-a"
    connection.revision = -1004  # unrelated document trigger advanced the fence
    assert (await store.get_active_special_publication_for_dataset(
        "dataset-a", connection=connection,
    ))["execution_id"] == "execution-a"
    connection.revision = -1008  # a newer negative fence cannot use an old binding
    with pytest.raises(RuntimeError, match="owner manifest is invalid"):
        await store.get_active_special_publication_for_dataset(
            "dataset-a", connection=connection,
        )
    connection.revision = -1004
    connection.other_execution["manifest"]["special_publication"]["publication_revision"] = 1007
    connection.other_execution["status"] = "completed"
    assert (await store.get_active_special_publication_for_dataset(
        "dataset-a", connection=connection,
    ))["execution_id"] == "execution-a"
    connection.other_execution["status"] = "running"
    with pytest.raises(RuntimeError, match="no unique running owner"):
        await store.get_active_special_publication_for_dataset(
            "dataset-a", connection=connection,
        )
    connection.revision = -1007
    with pytest.raises(RuntimeError, match="another special publication owns"):
        await store.bind_special_publication_revision(
            "execution-a", "dataset-a", -1007, connection=connection,
        )


@pytest.mark.asyncio
async def test_special_plan_is_pinned_before_write_and_terminal_proof_is_required() -> None:
    store = _store()
    connection = RecordingConnection()
    preparing = await store.record_special_publication_manifest(
        "execution-a", "document-a", "dataset-a", "generation-a",
        source_hash=SOURCE_HASH,
        planned_object_keys=["images/generation-a"],
        connection=connection,
    )
    assert preparing["phase"] == "preparing"
    assert preparing["collections"] == {}
    assert preparing["objects"] == {}
    assert preparing["planned_object_keys"] == ["images/generation-a"]
    with pytest.raises(RuntimeError, match="planned object keys changed"):
        await store.record_special_publication_manifest(
            "execution-a", "document-a", "dataset-a", "generation-a",
            source_hash=SOURCE_HASH,
            planned_object_keys=["images/another-generation"],
            connection=connection,
        )
    assert await store.reconcile_special_publication_execution(
        "execution-a", "document-a", "dataset-a", "generation-a",
        connection=connection,
    ) == "unproven"

    plan = {"collection-a": {
        "candidate_point_ids": ["new-2", "new-1"],
        "old_point_ids": ["old-1"],
    }}
    with pytest.raises(RuntimeError, match="receipts do not cover planned keys"):
        await store.advance_special_publication_manifest(
            "execution-a", "document-a", "dataset-a", "generation-a",
            source_hash=SOURCE_HASH, plan_hash=PLAN_HASH,
            expected_phase="preparing", next_phase="prepared",
            collections=plan, connection=connection,
        )
    prepared = await store.advance_special_publication_manifest(
        "execution-a", "document-a", "dataset-a", "generation-a",
        source_hash=SOURCE_HASH, plan_hash=PLAN_HASH,
        expected_phase="preparing", next_phase="prepared",
        collections=plan, objects={"images/generation-a": "d" * 64},
        planned_object_keys=["images/generation-a"],
        connection=connection,
    )
    assert prepared["collections"]["collection-a"]["candidate_point_ids"] == [
        "new-1", "new-2"
    ]
    connection.execution["manifest"]["special_publication"]["publication_revision"] = 1007
    with pytest.raises(RuntimeError, match="source hash changed"):
        await store.advance_special_publication_manifest(
            "execution-a", "document-a", "dataset-a", "generation-a",
            source_hash="c" * 64, plan_hash=PLAN_HASH,
            expected_phase="prepared", next_phase="points_written", connection=connection,
        )
    with pytest.raises(RuntimeError, match="plan hash changed"):
        await store.advance_special_publication_manifest(
            "execution-a", "document-a", "dataset-a", "generation-a",
            source_hash=SOURCE_HASH, plan_hash="c" * 64,
            expected_phase="prepared", next_phase="points_written", connection=connection,
        )
    with pytest.raises(RuntimeError, match="immutable"):
        await store.advance_special_publication_manifest(
            "execution-a", "document-a", "dataset-a", "generation-a",
            source_hash=SOURCE_HASH, plan_hash=PLAN_HASH,
            expected_phase="prepared", next_phase="points_written",
            collections={"collection-a": {
                "candidate_point_ids": ["other"], "old_point_ids": ["old-1"],
            }},
            connection=connection,
        )
    await store.advance_special_publication_manifest(
        "execution-a", "document-a", "dataset-a", "generation-a",
        source_hash=SOURCE_HASH, plan_hash=PLAN_HASH,
        expected_phase="prepared", next_phase="points_written", connection=connection,
    )
    with pytest.raises(RuntimeError, match="not completed"):
        await store.advance_special_publication_manifest(
            "execution-a", "document-a", "dataset-a", "generation-a",
            source_hash=SOURCE_HASH, plan_hash=PLAN_HASH,
            expected_phase="points_written", next_phase="committed", connection=connection,
        )
    connection.document["status"] = "completed"
    connection.dataset_revision = -1004  # statement triggers advanced the negative fence
    await store.advance_special_publication_manifest(
        "execution-a", "document-a", "dataset-a", "generation-a",
        source_hash=SOURCE_HASH, plan_hash=PLAN_HASH,
        expected_phase="points_written", next_phase="committed", connection=connection,
    )
    assert connection.execution["manifest"]["special_publication"]["publication_revision"] == 1004
    assert await store.reconcile_special_publication_execution(
        "execution-a", "document-a", "dataset-a", "generation-a",
        connection=connection,
    ) == "reconciled"
    assert connection.execution["status"] == "completed"
    assert "_special_publication_generation_id" not in connection.document["metadata"]


@pytest.mark.asyncio
async def test_aborted_cleanup_is_not_inferred_until_document_errors() -> None:
    store = _store()
    connection = RecordingConnection()
    await store.record_special_publication_manifest(
        "execution-a", "document-a", "dataset-a", "generation-a",
        source_hash=SOURCE_HASH,
        connection=connection,
    )
    await store.advance_special_publication_manifest(
        "execution-a", "document-a", "dataset-a", "generation-a",
        source_hash=SOURCE_HASH,
        expected_phase="preparing", next_phase="aborted", connection=connection,
    )
    assert await store.reconcile_special_publication_execution(
        "execution-a", "document-a", "dataset-a", "generation-a",
        connection=connection,
    ) == "unproven"
    connection.document["status"] = "error"
    assert await store.reconcile_special_publication_execution(
        "execution-a", "document-a", "dataset-a", "generation-a",
        connection=connection,
    ) == "reconciled"
    assert connection.execution["status"] == "error"
