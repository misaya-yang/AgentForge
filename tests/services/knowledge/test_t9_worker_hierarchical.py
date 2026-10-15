"""PRD T9-1 worker-side follow-up: default-worker indexer injection, safely.

With the hierarchical indexer finally wired into the default KnowledgeWorker
(main.py), two properties must hold:

* grayscale — the hierarchical branches stay opt-in. Only datasets whose
  stored chunking mode is explicitly "hierarchical" take the new dispatch;
  AUTOMATIC and every malformed config keep the pre-lift standard path;
* failure attribution (PRD T9-1) — when a hierarchical attempt fails and the
  run falls back to standard ingestion, the swallowed stage is recorded on
  the pipeline execution receipt (jsonb manifest ``stage_fallbacks``) instead
  of disappearing into a completed run, and an unsuccessful (no-vector) index
  result is recorded even though it raises nothing.

Also pins the dataset-scoped embedder resolution that makes the injection
safe for custom-model datasets (embedding_resolver, not a global embedder).
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.services.knowledge.hierarchical_indexer import HierarchicalIndexer
from knowledge_service.services.knowledge.worker import (
    KnowledgeIngestTask,
    KnowledgeWorker,
)

DATASET_ID = "dataset-a"
DOCUMENT_ID = "doc-a"


def _dataset(*, mode: str | None = "hierarchical") -> dict[str, Any]:
    chunking: dict[str, Any] = {} if mode is None else {"mode": mode}
    return {
        "dataset_id": DATASET_ID,
        "tenant_id": "tenant-a",
        "index_config": {"chunking": chunking},
    }


def _index_result(
    *, success: bool = True, errors: list[str] | None = None, vectors: int = 3
) -> SimpleNamespace:
    return SimpleNamespace(
        document_id=DOCUMENT_ID,
        l1_count=1,
        l2_count=2,
        l3_count=2,
        total_vectors=0 if not success else vectors,
        errors=list(errors or []),
        success=success,
    )


class RecordingIndexer:
    """Stand-in for HierarchicalIndexer.index_document."""

    def __init__(self, *, raises: Exception | None = None, result: SimpleNamespace | None = None):
        self.raises = raises
        self.result = result or _index_result()
        self.calls: list[dict[str, Any]] = []

    async def index_document(self, **kwargs: Any) -> SimpleNamespace:
        self.calls.append(kwargs)
        if self.raises is not None:
            raise self.raises
        return self.result


class T9Database:
    def __init__(self, dataset: dict[str, Any] | None, *, fail_get_dataset: bool = False) -> None:
        self.dataset = dataset
        self.fail_get_dataset = fail_get_dataset
        self.status_writes: list[tuple[str, str]] = []
        self.completions: list[dict[str, Any]] = []

    async def get_dataset(
        self, dataset_id: str, *, connection: Any | None = None
    ) -> dict[str, Any] | None:
        del connection
        if self.fail_get_dataset:
            raise RuntimeError("db is down")
        if dataset_id != DATASET_ID or self.dataset is None:
            return None
        return dict(self.dataset)

    async def get_document(
        self, document_id: str, *, connection: Any | None = None
    ) -> dict[str, Any] | None:
        del connection
        if document_id != DOCUMENT_ID:
            return None
        return {
            "document_id": DOCUMENT_ID,
            "dataset_id": DATASET_ID,
            "status": "waiting",
            "size_bytes": 10,
            "content": "some readable text",
            "metadata": {
                "original_file_key": "orig-key",
                "mime_type": "text/plain",
            },
        }

    async def update_document_status(
        self,
        document_id: str,
        status: str,
        progress: float | None = None,
        error: str | None = None,
        *,
        connection: Any | None = None,
    ) -> None:
        del progress, error, connection
        self.status_writes.append((document_id, status))

    async def complete_pipeline_execution(
        self,
        execution_id: str,
        *,
        status: str,
        error: str | None = None,
        manifest: dict[str, Any] | list[Any] | None = None,
    ) -> bool:
        self.completions.append(
            {"execution_id": execution_id, "status": status, "error": error, "manifest": manifest}
        )
        return True


class T9Service:
    def __init__(self, database: T9Database) -> None:
        self.db = database
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
        self.image_storage_service = SimpleNamespace(
            download_original_file=self._download
        )
        self.ingest_calls: list[tuple[str, str]] = []

    async def _download(self, _storage_key: str) -> bytes:
        return b"page one content here for hierarchical chunking"

    async def ingest_document(
        self,
        dataset_id: str,
        document_id: str,
        *,
        chunking_config_override: dict[str, Any] | None = None,
        index_config_override: dict[str, Any] | None = None,
    ) -> list[str]:
        del chunking_config_override, index_config_override
        self.ingest_calls.append((dataset_id, document_id))
        return ["seg-standard"]


def _make_worker(
    *,
    mode: str | None = "hierarchical",
    indexer: RecordingIndexer | None = None,
    fail_get_dataset: bool = False,
) -> tuple[KnowledgeWorker, T9Database, T9Service, RecordingIndexer | None]:
    database = T9Database(_dataset(mode=mode), fail_get_dataset=fail_get_dataset)
    service = T9Service(database)
    worker = KnowledgeWorker(service, hierarchical_indexer=indexer)  # type: ignore[arg-type]
    return worker, database, service, indexer


def _make_task() -> KnowledgeIngestTask:
    return KnowledgeIngestTask(dataset_id=DATASET_ID, document_id=DOCUMENT_ID)


# ---------------------------------------------------------------------------
# Embedder resolution (dataset-scoped, injected via resolver)
# ---------------------------------------------------------------------------


class _StubEmbedder:
    dimension = 7


@pytest.mark.asyncio
async def test_explicit_embedder_wins_and_resolver_is_untouched() -> None:
    calls: list[str] = []

    async def resolver(dataset_id: str) -> _StubEmbedder:
        calls.append(dataset_id)
        return _StubEmbedder()

    indexer = HierarchicalIndexer(
        vector_store=object(),
        database=object(),
        embedder="explicit",
        embedding_resolver=resolver,
    )
    assert await indexer._embedder_for("kb-1") == "explicit"
    assert calls == []


@pytest.mark.asyncio
async def test_resolver_is_called_once_per_dataset_and_cached() -> None:
    calls: list[str] = []

    async def resolver(dataset_id: str) -> _StubEmbedder:
        calls.append(dataset_id)
        return _StubEmbedder()

    indexer = HierarchicalIndexer(
        vector_store=object(), database=object(), embedding_resolver=resolver
    )
    first = await indexer._embedder_for("kb-1")
    second = await indexer._embedder_for("kb-1 ")
    other = await indexer._embedder_for("kb-2")
    assert first is second
    assert other is not first
    assert calls == ["kb-1", "kb-2"]


@pytest.mark.asyncio
async def test_missing_embedder_and_resolver_raises_runtime_error() -> None:
    indexer = HierarchicalIndexer(vector_store=object(), database=object())
    with pytest.raises(RuntimeError, match="without an embedder or embedding_resolver"):
        await indexer._embedder_for("kb-1")


@pytest.mark.asyncio
async def test_blank_dataset_id_raises_value_error() -> None:
    async def resolver(_dataset_id: str) -> _StubEmbedder:
        return _StubEmbedder()

    indexer = HierarchicalIndexer(
        vector_store=object(), database=object(), embedding_resolver=resolver
    )
    with pytest.raises(ValueError, match="requires a dataset_id"):
        await indexer._embedder_for("   ")


@pytest.mark.asyncio
async def test_resolver_returning_none_raises_value_error() -> None:
    async def resolver(_dataset_id: str) -> None:
        return None

    indexer = HierarchicalIndexer(
        vector_store=object(), database=object(), embedding_resolver=resolver
    )
    with pytest.raises(ValueError, match="embedding resolution failed"):
        await indexer._embedder_for("kb-1")


@pytest.mark.asyncio
async def test_vector_dimension_follows_resolver_and_defaults_to_1024() -> None:
    async def resolver(_dataset_id: str) -> _StubEmbedder:
        return _StubEmbedder()

    indexer = HierarchicalIndexer(
        vector_store=object(), database=object(), embedding_resolver=resolver
    )
    assert await indexer._get_vector_dimension("kb-1") == 7

    async def bare_resolver(_dataset_id: str) -> SimpleNamespace:
        return SimpleNamespace()  # no .dimension

    bare = HierarchicalIndexer(
        vector_store=object(), database=object(), embedding_resolver=bare_resolver
    )
    assert await bare._get_vector_dimension("kb-2") == 1024


# ---------------------------------------------------------------------------
# Grayscale: opt-in gating
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "index_config, expected",
    [
        ({"chunking": {"mode": "hierarchical"}}, True),
        ({"chunking": {"mode": " HIERARCHICAL "}}, True),
        ({"chunking": {"mode": "automatic"}}, False),
        ({"chunking": {"mode": "custom"}}, False),
        ({"chunking": {}}, False),
        ({"chunking": "not-a-dict"}, False),
        ({}, False),
        (None, False),
        ("not-a-dict", False),
    ],
)
def test_hierarchical_opted_in_is_explicit_only(
    index_config: Any, expected: bool
) -> None:
    assert KnowledgeWorker._hierarchical_opted_in(index_config) is expected


@pytest.mark.asyncio
async def test_dataset_opt_in_check_degrades_to_false_on_lookup_failure() -> None:
    worker, _database, _service, _indexer = _make_worker(fail_get_dataset=True)
    assert await worker._dataset_hierarchical_opted_in(DATASET_ID) is False


@pytest.mark.asyncio
async def test_automatic_dataset_stays_on_standard_path() -> None:
    indexer = RecordingIndexer()
    worker, _database, service, _indexer = _make_worker(mode="automatic", indexer=indexer)

    await worker._process_task(_make_task())

    assert service.ingest_calls == [(DATASET_ID, DOCUMENT_ID)]
    assert indexer.calls == []


@pytest.mark.asyncio
async def test_hierarchical_dataset_takes_indexer_path() -> None:
    indexer = RecordingIndexer()
    worker, database, service, _indexer = _make_worker(mode="hierarchical", indexer=indexer)
    stage_receipt: list[dict[str, str]] = []

    manifest = await worker._process_task(_make_task(), stage_receipt=stage_receipt)

    assert manifest is None
    assert service.ingest_calls == []
    assert len(indexer.calls) == 1
    assert indexer.calls[0]["dataset_id"] == DATASET_ID
    assert ("doc-a", "completed") in database.status_writes
    assert stage_receipt == []


# ---------------------------------------------------------------------------
# Failure attribution (PRD T9-1)
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_raising_indexer_falls_back_and_records_the_stage() -> None:
    indexer = RecordingIndexer(raises=RuntimeError("qdrant exploded"))
    worker, _database, service, _indexer = _make_worker(indexer=indexer)
    stage_receipt: list[dict[str, str]] = []

    await worker._process_task(_make_task(), stage_receipt=stage_receipt)

    # The swallow-and-retry behaviour is unchanged...
    assert service.ingest_calls == [(DATASET_ID, DOCUMENT_ID)]
    # ...but the failed stage is now attributable.
    assert len(stage_receipt) == 1
    assert stage_receipt[0]["stage"] == "hierarchical_indexing"
    assert "qdrant exploded" in stage_receipt[0]["error"]


@pytest.mark.asyncio
async def test_unsuccessful_index_result_records_fallback_without_exception() -> None:
    indexer = RecordingIndexer(
        result=_index_result(success=False, errors=["embed failed", "upsert skipped"])
    )
    worker, database, _service, _indexer = _make_worker(indexer=indexer)
    stage_receipt: list[dict[str, str]] = []

    await worker._process_task(_make_task(), stage_receipt=stage_receipt)

    assert len(stage_receipt) == 1
    assert stage_receipt[0]["stage"] == "hierarchical_indexing"
    assert "embed failed" in stage_receipt[0]["error"]
    assert "upsert skipped" in stage_receipt[0]["error"]
    # Document state still goes to error — attribution does not soften it.
    assert ("doc-a", "error") in database.status_writes


def test_note_stage_fallback_truncates_and_tolerates_missing_list() -> None:
    receipt: list[dict[str, str]] = []
    KnowledgeWorker._note_stage_fallback(receipt, stage="s", error=RuntimeError("x" * 600))
    assert receipt[0]["stage"] == "s"
    assert len(receipt[0]["error"]) == 500
    # A None receipt (caller without attribution) must not raise.
    KnowledgeWorker._note_stage_fallback(None, stage="s", error="ignored")


@pytest.mark.asyncio
async def test_finish_pipeline_execution_merges_fallbacks_into_manifest() -> None:
    database = T9Database(_dataset())
    service = T9Service(database)
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]
    fallbacks = [{"stage": "hierarchical_ocr_indexing", "error": "ocr index failed"}]

    await worker._finish_pipeline_execution(
        "exec-1", status="completed", manifest=["seg-1"], stage_receipt=fallbacks
    )
    await worker._finish_pipeline_execution("exec-2", status="completed", manifest=["seg-1"])
    await worker._finish_pipeline_execution("exec-3", status="completed")
    await worker._finish_pipeline_execution("", status="completed", manifest=["seg-1"])

    assert database.completions[0]["manifest"] == {
        "segment_ids": ["seg-1"],
        "stage_fallbacks": fallbacks,
    }
    assert database.completions[1]["manifest"] == {"segment_ids": ["seg-1"]}
    assert database.completions[2]["manifest"] is None
    # Empty execution id short-circuits before any write.
    assert len(database.completions) == 3


@pytest.mark.asyncio
async def test_error_receipt_still_carries_attributed_stages() -> None:
    database = T9Database(_dataset())
    service = T9Service(database)
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]

    await worker._finish_pipeline_execution(
        "exec-9",
        status="error",
        error="document processor returned without a completed generation",
        stage_receipt=[{"stage": "hierarchical_indexing", "error": "boom"}],
    )

    completion = database.completions[0]
    assert completion["status"] == "error"
    assert completion["manifest"]["stage_fallbacks"][0]["stage"] == "hierarchical_indexing"
