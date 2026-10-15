"""Unit tests for the reembed verb: in-place vector repair (PRD T1 item 3).

Contract under test:

* serving rows are re-embedded at their EXISTING segment/point identity — no
  delete-first window, no new ids;
* operator-disabled rows stay dark (their points remain deleted);
* rows a crashed generation left staged (status='indexing') are re-embedded
  and then promoted by the staging flip;
* with nothing persisted the verb degrades to the full pipeline;
* any embedding failure aborts the whole repair fail-closed (no partial
  vector state) and the document lands in the error terminal.
"""

from __future__ import annotations

import contextlib
from copy import deepcopy
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import pytest
from knowledge_service.services.knowledge.ingestion_service import (
    IngestionService,
    _rollback_backup_point_id,
)
from qdrant_client.http import models as qmodels


def _dataset() -> dict:
    return {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 2,
        "embedding_config": {},
        "index_config": {"chunking": {"mode": "automatic"}},
    }


def _document() -> dict:
    return {
        "document_id": "document-a",
        "dataset_id": "dataset-a",
        "source_type": "upload",
        "title": "doc.txt",
        "mime_type": "text/plain",
        "content": "alpha beta",
        "metadata": {"processing_mode": "text_only"},
    }


def _segment(
    *,
    segment_id: str,
    position: int,
    text: str,
    status: str = "completed",
    enabled: bool = True,
    vector_id: str | None = None,
) -> dict:
    return {
        "segment_id": segment_id,
        "dataset_id": "dataset-a",
        "document_id": "document-a",
        "content_type": "text",
        "position": position,
        "text": text,
        "token_count": len(text.split()),
        "status": status,
        "enabled": enabled,
        "vector_id": vector_id or segment_id,
        "metadata": {"page_number": position + 1, "language": "en"},
    }


class ReembedVectorStore:
    def __init__(self) -> None:
        self.points: dict[str, qmodels.PointStruct] = {}
        self.upsert_batches: list[list[str]] = []
        self.delete_batches: list[list[str]] = []
        self.fail_upsert_batch: int | None = None

    async def ensure_collection(self, **_kwargs: Any) -> str:
        return "collection-a"

    async def upsert(self, *, points: list[Any], **_kwargs: Any) -> None:
        ids = [str(point.id) for point in points]
        self.upsert_batches.append(ids)
        if self.fail_upsert_batch == len(self.upsert_batches):
            raise RuntimeError("simulated Qdrant batch failure")
        for point in points:
            self.points[str(point.id)] = point

    async def snapshot_points(
        self, _collection: str, point_ids: list[str], **_kwargs: Any
    ) -> dict[str, qmodels.PointStruct]:
        return {
            point_id: deepcopy(self.points[point_id])
            for point_id in point_ids
            if point_id in self.points
        }

    async def delete_points(
        self, _collection: str, point_ids: list[str], **_kwargs: Any
    ) -> None:
        self.delete_batches.append(list(point_ids))
        for point_id in point_ids:
            self.points.pop(point_id, None)


class ReembedDatabase:
    def __init__(self, segments: list[dict]) -> None:
        self.dataset = _dataset()
        self.document = _document()
        self.segments: dict[str, dict] = {
            row["segment_id"]: dict(row) for row in segments
        }
        self.status_updates: list[tuple[str, float | None]] = []
        self.recovered_publication = False
        self.publication_revision = -100_000

    @contextlib.asynccontextmanager
    async def dataset_index_write_lease(self, *_args: Any, **_kwargs: Any):
        yield

    @contextlib.asynccontextmanager
    async def dataset_index_publication_lease(
        self, *_args: Any, **_kwargs: Any
    ):
        yield SimpleNamespace(
            connection=self,
            revision=self.publication_revision,
            recovered=self.recovered_publication,
        )

    async def abort_index_publication(self, *_args: Any, **_kwargs: Any) -> None:
        return None

    async def get_dataset(self, _dataset_id: str, **_kwargs: Any) -> dict:
        return deepcopy(self.dataset)

    async def get_document(self, _document_id: str, **_kwargs: Any) -> dict:
        return deepcopy(self.document)

    async def list_segments(
        self,
        _dataset_id: str,
        *,
        document_id: str | None = None,
        limit: int = 100,
        offset: int = 0,
        **_kwargs: Any,
    ) -> list[dict]:
        rows = [
            deepcopy(row)
            for row in sorted(
                self.segments.values(), key=lambda item: item["position"]
            )
            if document_id is None or row["document_id"] == document_id
        ]
        return rows[offset : offset + limit]

    async def update_document_status(
        self, _document_id: str, status: str, progress: float | None = None, **_kwargs: Any
    ) -> None:
        self.status_updates.append((status, progress))

    async def activate_staged_segments(
        self, _document_id: str, segment_ids: list[str], **_kwargs: Any
    ) -> int:
        wanted = {sid for sid in segment_ids if sid}
        promoted = 0
        for row in self.segments.values():
            if row["segment_id"] in wanted and row.get("status") == "indexing":
                row["status"] = "completed"
                row["enabled"] = True
                promoted += 1
        return promoted

    async def refresh_document_segment_count(self, _document_id: str) -> int:
        return sum(
            1
            for row in self.segments.values()
            if row.get("status") == "completed" and row.get("enabled")
        )

    async def commit_reembed_publication(
        self,
        *,
        document_id: str,
        staged_segment_ids: list[str],
        **_kwargs: Any,
    ) -> int:
        return await self.activate_staged_segments(document_id, staged_segment_ids)


class RepairEmbedder:
    _dimension = 2
    dimension = 2

    def __init__(self, *, fail_on: str | None = None) -> None:
        self.fail_on = fail_on
        self.embedded_texts: list[str] = []

    async def embed_documents(self, texts: list[str]) -> list[list[float]]:
        if self.fail_on and any(self.fail_on in text for text in texts):
            raise RuntimeError("provider failure")
        self.embedded_texts.extend(texts)
        return [[1.0, float(len(text) % 5)] for text in texts]

    async def embed_query(self, _text: str) -> list[float]:
        return [0.0, 0.0]

    async def close(self) -> None:
        return None


def build_service(
    database: ReembedDatabase,
    store: ReembedVectorStore,
    embedder: RepairEmbedder,
) -> IngestionService:
    knowledge = SimpleNamespace(
        image_storage_service=None,
        _get_dataset_or_404=AsyncMock(side_effect=lambda _id: deepcopy(database.dataset)),
        _is_multimodal_dataset=lambda _dataset: False,
        _get_text_embedder=lambda _dataset, _config: embedder,
    )
    service = IngestionService.__new__(IngestionService)
    service.settings = SimpleNamespace(
        knowledge=SimpleNamespace(
            text_embedding_batch_size=2,
            text_embedding_max_concurrent=2,
            multimodal_embedding_max_concurrent=1,
        )
    )
    service.db = database
    service.vector_store = store
    service._ks = knowledge
    return service


@pytest.mark.asyncio
async def test_reembed_repairs_serving_rows_at_existing_identity() -> None:
    database = ReembedDatabase(
        [
            _segment(segment_id="seg-1", position=0, text="alpha beta"),
            _segment(
                segment_id="seg-2",
                position=1,
                text="gamma delta",
                vector_id="vec-custom-2",
            ),
        ]
    )
    store = ReembedVectorStore()
    service = build_service(database, store, RepairEmbedder())

    repaired = await service.reembed_document("dataset-a", "document-a")

    assert repaired == ["seg-1", "seg-2"]
    # Point identity is preserved: the custom vector_id wins over segment_id.
    assert set(store.points) == {"seg-1", "vec-custom-2"}
    payload = store.points["vec-custom-2"].payload
    assert payload["segment_id"] == "seg-2"
    assert payload["document_id"] == "document-a"
    assert payload["position"] == 1
    assert payload["text"] == "gamma delta"
    assert payload["metadata"]["page_number"] == 2
    # The serving rows were repaired in place: no delete-first window.
    assert database.segments["seg-1"]["status"] == "completed"
    assert database.segments["seg-1"]["enabled"] is True
    assert database.status_updates[-1] == ("completed", 100)


@pytest.mark.asyncio
async def test_reembed_skips_operator_disabled_and_errored_rows() -> None:
    database = ReembedDatabase(
        [
            _segment(segment_id="seg-serving", position=0, text="alpha beta"),
            _segment(
                segment_id="seg-paused",
                position=1,
                text="paused text",
                enabled=False,
            ),
            _segment(
                segment_id="seg-errored",
                position=2,
                text="errored text",
                status="error",
            ),
        ]
    )
    store = ReembedVectorStore()
    service = build_service(database, store, RepairEmbedder())

    repaired = await service.reembed_document("dataset-a", "document-a")

    assert repaired == ["seg-serving"]
    assert set(store.points) == {"seg-serving"}
    # The disabled row is never re-enabled by a vector repair.
    assert database.segments["seg-paused"]["enabled"] is False


@pytest.mark.asyncio
async def test_reembed_promotes_staged_rows_left_by_a_crashed_generation() -> None:
    database = ReembedDatabase(
        [
            _segment(segment_id="seg-live", position=0, text="alpha beta"),
            _segment(
                segment_id="seg-staged",
                position=1,
                text="staged text",
                status="indexing",
                enabled=False,
            ),
        ]
    )
    store = ReembedVectorStore()
    service = build_service(database, store, RepairEmbedder())

    repaired = await service.reembed_document("dataset-a", "document-a")

    assert repaired == ["seg-live", "seg-staged"]
    assert set(store.points) == {"seg-live", "seg-staged"}
    # The same staging flip the full pipeline uses promotes the row.
    assert database.segments["seg-staged"]["status"] == "completed"
    assert database.segments["seg-staged"]["enabled"] is True


@pytest.mark.asyncio
async def test_reembed_falls_back_to_full_pipeline_when_nothing_persisted() -> None:
    database = ReembedDatabase([])
    store = ReembedVectorStore()
    service = build_service(database, store, RepairEmbedder())
    service.ingest_document = AsyncMock(return_value=["seg-new"])  # type: ignore[method-assign]

    repaired = await service.reembed_document("dataset-a", "document-a")

    assert repaired == ["seg-new"]
    service.ingest_document.assert_awaited_once_with("dataset-a", "document-a")
    assert store.points == {}


@pytest.mark.asyncio
async def test_reembed_fails_closed_on_partial_embedding_failure() -> None:
    database = ReembedDatabase(
        [
            _segment(segment_id="seg-good", position=0, text="alpha beta"),
            _segment(segment_id="seg-bad", position=1, text="totally bad text"),
        ]
    )
    store = ReembedVectorStore()
    service = build_service(
        database, store, RepairEmbedder(fail_on="bad")
    )

    repaired = await service.reembed_document("dataset-a", "document-a")

    assert repaired is None
    # No partial repair may reach the vector store.
    assert store.points == {}
    assert store.upsert_batches == []
    # The document lands in the error terminal.
    assert database.status_updates[-1][0] == "error"


@pytest.mark.asyncio
async def test_reembed_failure_keeps_old_vectors_serving() -> None:
    """§6.4 scenario 7 contrast clause: a mid-reembed failure must never zero
    the document's vectors (legacy behavior was failure = delete all). The
    pre-existing serving points keep serving untouched, the document lands in
    the error terminal, and re-running the verb (one-click resume) completes
    the repair in place."""
    database = ReembedDatabase(
        [
            _segment(segment_id="seg-good", position=0, text="alpha beta"),
            _segment(segment_id="seg-bad", position=1, text="totally bad text"),
        ]
    )
    store = ReembedVectorStore()
    # Seed the current serving generation's points.
    for row in database.segments.values():
        store.points[row["segment_id"]] = qmodels.PointStruct(
            id=row["segment_id"],
            vector=[0.5, 0.5],
            payload={
                "tenant_id": "tenant-a",
                "dataset_id": "dataset-a",
                "document_id": "document-a",
                "segment_id": row["segment_id"],
                "text": row["text"],
            },
        )
    seeded = {point_id: deepcopy(point) for point_id, point in store.points.items()}

    service = build_service(database, store, RepairEmbedder(fail_on="bad"))
    repaired = await service.reembed_document("dataset-a", "document-a")

    assert repaired is None
    # Old vectors are untouched and keep serving — no delete, no partial
    # overwrite.
    assert store.upsert_batches == []
    assert set(store.points) == set(seeded)
    for point_id, point in seeded.items():
        assert store.points[point_id].vector == point.vector
        assert store.points[point_id].payload == point.payload
    assert database.status_updates[-1][0] == "error"

    # One-click resume: the same verb with a healthy provider completes the
    # repair at the existing identities.
    service_ok = build_service(database, store, RepairEmbedder())
    repaired = await service_ok.reembed_document("dataset-a", "document-a")
    assert repaired == ["seg-good", "seg-bad"]
    assert set(store.points) == {"seg-good", "seg-bad"}
    assert database.status_updates[-1] == ("completed", 100)


@pytest.mark.asyncio
async def test_reembed_second_qdrant_batch_failure_restores_every_old_point(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A late write failure cannot leave an earlier batch on new vectors."""

    rows = [
        _segment(segment_id="seg-1", position=0, text="alpha beta"),
        _segment(segment_id="seg-2", position=1, text="gamma delta"),
    ]
    database = ReembedDatabase(rows)
    store = ReembedVectorStore()
    for index, row in enumerate(rows):
        store.points[row["vector_id"]] = qmodels.PointStruct(
            id=row["vector_id"],
            vector=[0.25, float(index)],
            payload={
                "tenant_id": "tenant-a",
                "dataset_id": "dataset-a",
                "document_id": "document-a",
                "segment_id": row["segment_id"],
                "text": f"old-{row['text']}",
            },
        )
    old_points = deepcopy(store.points)
    # Two one-point durable-backup writes precede the two serving writes;
    # fail the second serving batch, after the first was already applied.
    store.fail_upsert_batch = 4
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.VectorStoreConfig.get_batch_size",
        lambda _total: 1,
    )

    repaired = await build_service(
        database,
        store,
        RepairEmbedder(),
    ).reembed_document("dataset-a", "document-a")

    assert repaired is None
    assert set(store.points) == set(old_points)
    for point_id, old_point in old_points.items():
        assert store.points[point_id].vector == old_point.vector
        assert store.points[point_id].payload == old_point.payload
    assert database.status_updates[-1][0] == "error"


@pytest.mark.asyncio
async def test_reembed_resumes_interrupted_publication_from_durable_backups() -> None:
    rows = [
        _segment(segment_id="seg-1", position=0, text="alpha beta"),
        _segment(segment_id="seg-2", position=1, text="gamma delta"),
    ]
    database = ReembedDatabase(rows)
    database.recovered_publication = True
    # Simulate migration-076 triggers advancing the negative seqlock after the
    # durable backups were written. Backup lookup must not depend on this value.
    database.publication_revision = -99_991
    store = ReembedVectorStore()
    revision = -100_000

    for index, row in enumerate(rows):
        original_payload = {
            "tenant_id": "tenant-a",
            "dataset_id": "dataset-a",
            "document_id": "document-a",
            "segment_id": row["segment_id"],
            "text": f"old-{row['text']}",
        }
        old_vector = [0.25, float(index)]
        backup_id = _rollback_backup_point_id(
            "dataset-a",
            row["vector_id"],
        )
        store.points[backup_id] = qmodels.PointStruct(
            id=backup_id,
            vector=old_vector,
            payload={
                "tenant_id": "tenant-a",
                "dataset_id": "dataset-a",
                "document_id": "document-a",
                "segment_id": row["segment_id"],
                "enabled": False,
                "_kb_index_rollback": {
                    "publication_revision": abs(revision),
                    "original_point_id": row["vector_id"],
                    "payload": original_payload,
                },
            },
        )
        # Simulate a process death after only the first serving point changed.
        store.points[row["vector_id"]] = qmodels.PointStruct(
            id=row["vector_id"],
            vector=[9.0, float(index)],
            payload={**original_payload, "text": "partial-new"},
        )

    repaired = await build_service(
        database,
        store,
        RepairEmbedder(),
    ).reembed_document("dataset-a", "document-a")

    assert repaired == ["seg-1", "seg-2"]
    assert set(store.points) == {"seg-1", "seg-2"}
    assert store.points["seg-1"].payload["text"] == "alpha beta"
    assert store.points["seg-2"].payload["text"] == "gamma delta"
    assert database.status_updates[-1] == ("completed", 100)


@pytest.mark.asyncio
async def test_reembed_reconstructs_contextual_prefix_embedding_input() -> None:
    """Vector parity contract: the engine embeds the prefix-augmented chunk
    text while the row stores the display text (contextual_prefix column).
    A repair must reconstruct exactly what the engine embedded — the
    canonical join is f"{prefix}\\n\\n{text}" — so re-embedding never rewrites
    vectors under different semantics, and the point payload carries the same
    composition."""

    prefixed = _segment(segment_id="seg-prefixed", position=0, text="chunk body")
    prefixed["contextual_prefix"] = "Section 1.2 — Setup"
    plain = _segment(segment_id="seg-plain", position=1, text="plain chunk")
    database = ReembedDatabase([prefixed, plain])
    store = ReembedVectorStore()
    embedder = RepairEmbedder()
    service = build_service(database, store, embedder)

    repaired = await service.reembed_document("dataset-a", "document-a")

    assert repaired == ["seg-prefixed", "seg-plain"]
    # The embedding input for the prefixed row is the canonical composition;
    # the plain row keeps its bare text.
    assert embedder.embedded_texts == [
        "Section 1.2 — Setup\n\nchunk body",
        "plain chunk",
    ]
    # The point payload mirrors the same parity contract.
    assert store.points["seg-prefixed"].payload["text"] == (
        "Section 1.2 — Setup\n\nchunk body"
    )
    assert store.points["seg-plain"].payload["text"] == "plain chunk"
    # The stored row keeps the display text untouched.
    assert database.segments["seg-prefixed"]["text"] == "chunk body"


@pytest.mark.asyncio
async def test_reembed_never_reparses_the_source_document() -> None:
    database = ReembedDatabase(
        [_segment(segment_id="seg-1", position=0, text="alpha beta")]
    )
    store = ReembedVectorStore()
    service = build_service(database, store, RepairEmbedder())
    service.ingest_document = AsyncMock()  # type: ignore[method-assign]

    await service.reembed_document("dataset-a", "document-a")

    service.ingest_document.assert_not_awaited()
    assert store.points and store.upsert_batches
