"""Unit tests for the T1 stable-identity incremental upsert engine.

The engine (ingestion_service.ingest_document) reconciles a freshly chunked
generation against the persisted one, keyed by (document_id, content_type,
position):

  - unchanged (same position + content_hash, row serving): skipped entirely,
    zero re-embedding.
  - staged-resumable (same hash, row still status='indexing' from a crashed
    run): no re-embedding; only the completion flip.
  - changed (same position, different hash): keeps the existing row's
    segment_id/vector_id so the row updates in place and the Qdrant point is
    upserted at the SAME point id.
  - new: deterministic uuid5 lineage ids so replay never duplicates.
  - excess old positions: deleted only AFTER staging succeeds.

New/changed rows stage enabled=False + status='indexing' and are flipped to
serving only after the whole generation persists; operator-disabled rows are
never re-enabled.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
from copy import deepcopy
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import httpx
import pytest
from knowledge_service.services.knowledge.embedding import OpenAICompatibleEmbedding
from knowledge_service.services.knowledge.ingestion_service import (
    IngestionService,
    _stable_index_node_id,
    _stable_segment_id,
)
from qdrant_client.http import models as qmodels

LONG_CONTENT = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu"
CHANGED_CONTENT = "omega beta gamma delta epsilon zulu eta theta iota kappa lambda nu"
SHORT_CONTENT = "only-one-chunk"

CHUNKING_CONFIG = {
    "chunk_size": 50,
    "chunk_overlap": 0,
    "min_chunk_size": 50,
    "max_chunk_size": 60,
}


def _dataset() -> dict:
    return {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 2,
        "embedding_config": {},
        "index_config": {
            "chunking": dict(CHUNKING_CONFIG),
            "retrieval": {"top_k": 10},
        },
    }


def _document(content: str) -> dict:
    return {
        "document_id": "document-a",
        "dataset_id": "dataset-a",
        "source_type": "upload",
        "title": "doc.txt",
        "mime_type": "text/plain",
        "content": content,
        "metadata": {"processing_mode": "text_only"},
    }


class EngineVectorStore:
    """Point-id keyed store; upsert replaces, delete_points removes."""

    def __init__(self) -> None:
        self.points: dict[str, qmodels.PointStruct] = {}
        self.events: list[str] = []
        self.deleted_ids: list[str] = []
        self.replacement_upsert_calls = 0
        self.fail_replacement_upsert_at: int | None = None
        self.block_replacement_upsert_at: int | None = None
        self.blocked_replacement_upsert = asyncio.Event()
        self.release_replacement_upsert = asyncio.Event()

    async def ensure_collection(self, **_kwargs: Any) -> str:
        return "collection-a"

    async def upsert(self, *, points: list[Any], **_kwargs: Any) -> None:
        self.events.append(f"upsert:{sorted(str(point.id) for point in points)}")
        is_rollback_backup = bool(points) and all(
            "_kb_index_rollback" in (getattr(point, "payload", None) or {})
            for point in points
        )
        if not is_rollback_backup:
            self.replacement_upsert_calls += 1
            if self.replacement_upsert_calls == self.block_replacement_upsert_at:
                self.blocked_replacement_upsert.set()
                await self.release_replacement_upsert.wait()
            if self.replacement_upsert_calls == self.fail_replacement_upsert_at:
                raise RuntimeError("replacement Qdrant batch rejected")
        for point in points:
            self.points[str(point.id)] = point

    async def snapshot_points(
        self, _collection: str, point_ids: list[str], **_kwargs: Any
    ) -> dict[str, qmodels.PointStruct]:
        self.events.append(f"snapshot:{sorted(point_ids)}")
        return {
            point_id: deepcopy(self.points[point_id])
            for point_id in point_ids
            if point_id in self.points
        }

    async def delete_points(
        self, _collection: str, point_ids: list[str], **_kwargs: Any
    ) -> None:
        self.events.append(f"delete-points:{sorted(point_ids)}")
        self.deleted_ids.extend(point_ids)
        for point_id in point_ids:
            self.points.pop(str(point_id), None)


class EngineDatabase:
    """Fake PG limited to the segment-reconciliation surface.

    insert_segments mirrors the production ON CONFLICT
    (document_id, content_type, position) DO UPDATE semantics.
    """

    def __init__(self, dataset: dict, document: dict) -> None:
        self.dataset = dataset
        self.document = document
        self.versions: dict[int, dict[str, Any]] = {}
        # (content_type, position) -> row
        self.segments: dict[tuple[str, int], dict[str, Any]] = {}
        self.status_updates: list[str] = []
        self.events: list[str] = []
        self.field_updates: list[dict[str, Any]] = []
        self.fail_field_updates: bool = False
        self.fail_next_insert: bool = False

    # -- documents ---------------------------------------------------------
    @contextlib.asynccontextmanager
    async def dataset_index_write_lease(self, *_args: Any, **_kwargs: Any):
        yield

    @contextlib.asynccontextmanager
    async def dataset_index_publication_lease(
        self, *_args: Any, **_kwargs: Any
    ):
        self.events.append("publication:begin")
        yield SimpleNamespace(
            connection=self,
            revision=-100_000,
            recovered=False,
        )

    async def abort_index_publication(self, *_args: Any, **_kwargs: Any) -> None:
        self.events.append("publication:abort")

    async def get_dataset(self, _dataset_id: str, **_kwargs: Any) -> dict:
        return deepcopy(self.dataset)

    async def get_document(self, _document_id: str, **_kwargs: Any) -> dict:
        return deepcopy(self.document)

    async def get_document_version(
        self, _document_id: str, version_number: int, *, include_pending: bool = False,
    ) -> dict[str, Any] | None:
        row = self.versions.get(version_number)
        if row and (include_pending or row.get("change_type") != "pending_restore"):
            return deepcopy(row)
        return None

    async def update_document_status(
        self, _document_id: str, *, status: str, **_kwargs: Any
    ) -> None:
        self.status_updates.append(status)
        self.events.append(f"status:{status}")

    async def update_document_fields(
        self, _document_id: str, fields: dict, **_kwargs: Any
    ) -> None:
        if self.fail_field_updates:
            raise RuntimeError("simulated pre-102 schema: column missing")
        self.field_updates.append(dict(fields))
        if "metadata" in fields:
            self.document["metadata"] = deepcopy(fields["metadata"])

    async def update_document_content(self, _document_id: str, content: str) -> None:
        self.document["content"] = content

    async def refresh_document_segment_count(self, _document_id: str) -> int:
        return len(self.segments)

    async def clear_dataset_needs_reindex(self, _dataset_id: str) -> None:
        return None

    # -- segments ----------------------------------------------------------
    async def get_segment_hashes_by_document(
        self, _document_id: str, *, content_type: str = "text", **_kwargs: Any
    ) -> dict[int, dict[str, Any]]:
        result = {}
        for (ctype, position), row in self.segments.items():
            if ctype != content_type:
                continue
            result[position] = {
                "segment_id": row["segment_id"],
                "vector_id": row.get("vector_id"),
                "content_hash": row.get("content_hash"),
                "status": row.get("status"),
                "enabled": row.get("enabled"),
            }
        return result

    async def insert_segments(self, segments: list[dict], **_kwargs: Any) -> None:
        self.events.append(f"insert:{len(segments)}")
        if self.fail_next_insert:
            self.fail_next_insert = False
            raise RuntimeError("database rejected replacement batch")
        for seg in segments:
            ctype = seg.get("content_type") or "text"
            key = (ctype, int(seg.get("position", 0) or 0))
            self.segments[key] = dict(seg)

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
        self.events.append(f"activate:{promoted}")
        return promoted

    async def delete_segments_by_document(
        self,
        _document_id: str,
        exclude_ids: list[str] | None = None,
        content_type: str | None = None,
        **_kwargs: Any,
    ) -> int:
        exclude = set(exclude_ids or [])
        removed = 0
        for key in list(self.segments):
            ctype, _position = key
            row = self.segments[key]
            if content_type and ctype != content_type:
                continue
            if row["segment_id"] in exclude:
                continue
            del self.segments[key]
            removed += 1
        self.events.append(f"delete:{removed}")
        return removed

    async def commit_text_segment_publication(
        self,
        *,
        document_id: str,
        segment_rows: list[dict],
        keep_segment_ids: list[str],
        staged_segment_ids: list[str],
        delete_excess: bool,
        candidate_content: str | None = None,
        expected_content: str | None = None,
        candidate_metadata_patch: dict[str, Any] | None = None,
        candidate_word_count: int | None = None,
        candidate_version_number: int | None = None,
        previous_version_number: int | None = None,
        finalize_document: bool = False,
        **_kwargs: Any,
    ) -> tuple[int, int]:
        before = deepcopy(self.segments)
        before_document = deepcopy(self.document)
        before_versions = deepcopy(self.versions)
        try:
            await self.insert_segments(segment_rows)
            deleted = 0
            if delete_excess:
                deleted = await self.delete_segments_by_document(
                    document_id,
                    exclude_ids=keep_segment_ids,
                    content_type="text",
                )
            promoted = await self.activate_staged_segments(
                document_id,
                staged_segment_ids,
            )
            if candidate_content is not None:
                if self.document["content"] != expected_content:
                    raise RuntimeError("document source changed during index publication")
                self.document["content"] = candidate_content
                self.document["metadata"] = {
                    **self.document.get("metadata", {}),
                    **(candidate_metadata_patch or {}),
                }
                if candidate_word_count is not None:
                    self.document["word_count"] = candidate_word_count
                if candidate_version_number is not None:
                    if self.document["metadata"].get("_document_pending_restore_version") != {
                        "previous_version": previous_version_number,
                        "candidate_version": candidate_version_number,
                    }:
                        raise RuntimeError("restore candidate changed")
                    before_version = self.versions[previous_version_number]
                    if before_version["change_type"] != "pending_before_restore" or before_version["content"] != expected_content:
                        raise RuntimeError("pre-restore source snapshot changed")
                    version = self.versions[candidate_version_number]
                    if version["change_type"] != "pending_restore" or version["content"] != candidate_content:
                        raise RuntimeError("restore candidate content changed")
                    before_version["change_type"] = "updated"
                    version["change_type"] = "restored"
                    self.document["metadata"].pop("_document_pending_restore_version")
                    self.document["metadata"]["_document_restored_source_version"] = candidate_version_number
                    self.document["current_version"] = candidate_version_number
                    self.document["version_count"] = self.document.get("version_count", 0) + 2
            if finalize_document:
                self.document["status"] = "completed"
                self.status_updates.append("completed")
                self.events.append("status:completed")
                for marker in (
                    "_document_ingest_action", "_document_recover_stage",
                    "_document_pipeline_execution_id",
                ):
                    self.document["metadata"].pop(marker, None)
        except Exception:
            self.segments = before
            self.document = before_document
            self.versions = before_versions
            raise
        self.events.append("publication:commit")
        return promoted, deleted

    async def get_image_segments_by_document(self, _document_id: str) -> list[dict]:
        return []

    async def save_image_segment(self, segment: dict) -> None:
        self.segments[("image", int(segment.get("position", 0) or 0))] = dict(segment)


class CountingEmbedder:
    _dimension = 2
    dimension = 2

    def __init__(self) -> None:
        self.embedded_texts: list[str] = []

    async def embed_documents(self, texts: list[str]) -> list[list[float]]:
        self.embedded_texts.extend(texts)
        return [[float(len(text) % 7), 0.5] for text in texts]

    async def embed_query(self, _text: str) -> list[float]:
        return [0.0, 0.5]

    async def close(self) -> None:
        return None


def build_service(
    *,
    dataset: dict,
    database: EngineDatabase,
    store: EngineVectorStore,
    embedder: CountingEmbedder,
) -> IngestionService:
    knowledge = SimpleNamespace(
        image_storage_service=None,
        _get_dataset_or_404=AsyncMock(side_effect=lambda _id: deepcopy(dataset)),
        _is_multimodal_dataset=lambda _dataset: False,
        _get_text_embedder=lambda _dataset, _config: embedder,
        associate_images_to_chunks=AsyncMock(
            return_value={"associations_created": 0, "segments_with_images": 0}
        ),
    )
    service = IngestionService.__new__(IngestionService)
    service.settings = SimpleNamespace(
        knowledge=SimpleNamespace(
            pdf_min_text_chars_for_ocr=200,
            scanned_min_images_for_image_only=5,
            ocr_enabled=True,
            text_embedding_batch_size=8,
            text_embedding_max_concurrent=1,
            multimodal_embedding_max_concurrent=1,
        )
    )
    service.db = database
    service.vector_store = store
    service._ks = knowledge
    return service


def make_world(content: str):
    dataset = _dataset()
    database = EngineDatabase(dataset, _document(content))
    store = EngineVectorStore()
    return dataset, database, store


async def ingest_once(dataset, database, store, content: str) -> CountingEmbedder:
    """Run one generation over the shared PG/Qdrant state."""
    database.document = _document(content)
    embedder = CountingEmbedder()
    service = build_service(
        dataset=dataset, database=database, store=store, embedder=embedder
    )
    await service.ingest_document("dataset-a", "document-a")
    return embedder


def text_rows(database: EngineDatabase) -> dict[int, dict[str, Any]]:
    return {
        position: row
        for (ctype, position), row in database.segments.items()
        if ctype == "text"
    }


@pytest.mark.asyncio
async def test_first_ingest_stages_rows_then_flips_them_on_completion() -> None:
    dataset, database, store = make_world(LONG_CONTENT)

    embedder = await ingest_once(dataset, database, store, LONG_CONTENT)

    assert database.status_updates[-1] == "completed"
    rows = text_rows(database)
    assert sorted(rows) == [0, 1]
    # Every chunk was embedded exactly once.
    assert len(embedder.embedded_texts) == 2
    # Staged rows carry deterministic lineage identity and are serving after
    # the completion flip.
    for position, row in rows.items():
        assert row["segment_id"] == _stable_segment_id("document-a", "text", position)
        assert row["vector_id"] == row["segment_id"]
        assert row["index_node_id"] == _stable_index_node_id(
            "document-a", "text", position
        )
        assert row["index_node_hash"] == row["content_hash"]
        assert row["enabled"] is True
        assert row["status"] == "completed"
        # Vector point published under the row's vector id.
        assert row["vector_id"] in store.points
        assert "activate:2" in database.events


@pytest.mark.asyncio
async def test_lifecycle_restore_uses_terminal_that_reenables_document() -> None:
    dataset, database, store = make_world(LONG_CONTENT)
    database.document["metadata"]["_document_lifecycle_reindex"] = {
        "status": "pending", "desired_enabled": True, "desired_archived": False,
    }
    database.document["enabled"] = False
    database.document["archived"] = True
    service = build_service(
        dataset=dataset, database=database, store=store, embedder=CountingEmbedder(),
    )
    terminal = AsyncMock(wraps=service._complete_document_generation)
    service._complete_document_generation = terminal  # type: ignore[method-assign]

    await service.ingest_document("dataset-a", "document-a")

    terminal.assert_awaited_once()


@pytest.mark.asyncio
async def test_completion_flip_precedes_completed_status_and_manifest_returns() -> None:
    """Pins the atomic completion contract (PRD T1 item 5).

    Staged rows must be activated BEFORE the document's "completed" status
    write (a reader must never observe a completed document whose rows are
    still indexing), all staging inserts precede the flip, and
    ingest_document returns the staged manifest the worker records into the
    execution ledger.
    """
    dataset, database, store = make_world(LONG_CONTENT)
    service = build_service(
        dataset=dataset, database=database, store=store, embedder=CountingEmbedder()
    )

    manifest = await service.ingest_document("dataset-a", "document-a")

    last_insert = max(
        index
        for index, event in enumerate(database.events)
        if event.startswith("insert:")
    )
    activate = max(
        index
        for index, event in enumerate(database.events)
        if event.startswith("activate:")
    )
    completed = database.events.index("status:completed")
    assert last_insert < activate < completed

    # The returned manifest is exactly the staged segment ids — nothing more,
    # nothing missing — which is what the worker writes to the execution row.
    staged_ids = {row["segment_id"] for row in text_rows(database).values()}
    assert manifest is not None
    assert len(manifest) == len(staged_ids) == 2
    assert set(manifest) == staged_ids


@pytest.mark.asyncio
async def test_unchanged_content_skips_embedding_entirely() -> None:
    dataset, database, store = make_world(LONG_CONTENT)
    await ingest_once(dataset, database, store, LONG_CONTENT)
    first_rows = {
        position: row["segment_id"] for position, row in text_rows(database).items()
    }
    first_points = set(store.points)
    events_before = len(database.events)

    embedder = await ingest_once(dataset, database, store, LONG_CONTENT)

    # Zero re-embedding, identity untouched, still serving.
    assert embedder.embedded_texts == []
    assert database.status_updates[-1] == "completed"
    rows = text_rows(database)
    assert {position: row["segment_id"] for position, row in rows.items()} == first_rows
    assert all(row["enabled"] is True and row["status"] == "completed"
               for row in rows.values())
    assert set(store.points) == first_points
    # No staging writes and no flip were needed.
    second_run_events = database.events[events_before:]
    assert not any(event.startswith("insert:") for event in second_run_events)
    assert not any(event.startswith("activate:") for event in second_run_events)


@pytest.mark.asyncio
async def test_crashed_staged_row_with_missing_point_is_reembedded_before_activation() -> None:
    dataset, database, store = make_world(SHORT_CONTENT)
    await ingest_once(dataset, database, store, SHORT_CONTENT)
    row = text_rows(database)[0]
    point_id = row["vector_id"]
    row.update(status="indexing", enabled=False)
    store.points.pop(point_id)
    embedder = CountingEmbedder()
    service = build_service(
        dataset=dataset, database=database, store=store, embedder=embedder,
    )

    await service.ingest_document("dataset-a", "document-a")

    assert database.status_updates[-1] == "completed"
    assert embedder.embedded_texts
    assert point_id in store.points
    assert text_rows(database)[0]["status"] == "completed"


@pytest.mark.asyncio
async def test_crashed_staged_row_with_foreign_point_refuses_activation() -> None:
    dataset, database, store = make_world(SHORT_CONTENT)
    await ingest_once(dataset, database, store, SHORT_CONTENT)
    row = text_rows(database)[0]
    point_id = row["vector_id"]
    row.update(status="indexing", enabled=False)
    original = deepcopy(store.points[point_id])
    store.points[point_id] = original.model_copy(update={
        "payload": {**original.payload, "document_id": "foreign-document"},
    })
    embedder = CountingEmbedder()
    service = build_service(
        dataset=dataset, database=database, store=store, embedder=embedder,
    )

    await service.ingest_document("dataset-a", "document-a")

    assert database.status_updates[-1] == "error"
    assert embedder.embedded_texts == []
    assert text_rows(database)[0]["status"] == "indexing"
    assert store.points[point_id].payload["document_id"] == "foreign-document"


@pytest.mark.asyncio
async def test_provider_http_failure_keeps_serving_text_generation() -> None:
    dataset, database, store = make_world(SHORT_CONTENT)
    dataset.update(
        embedding_provider="openai_compatible",
        embedding_model="fixture-2",
    )

    def make_embedder(handler):
        embedder = OpenAICompatibleEmbedding(
            base_url="http://embeddings.invalid/v1", model="fixture-2", dimension=2,
        )
        embedder.MAX_RETRIES = 1
        embedder._client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
        return embedder

    def success(request: httpx.Request) -> httpx.Response:
        count = len(json.loads(request.content)["input"])
        return httpx.Response(200, json={
            "data": [{"index": i, "embedding": [0.25, 0.5]} for i in range(count)],
        })

    first = build_service(
        dataset=dataset, database=database, store=store,
        embedder=make_embedder(success),
    )
    await first.ingest_document("dataset-a", "document-a")
    old_rows = deepcopy(text_rows(database))
    old_points = deepcopy(store.points)
    old_content = database.document["content"]

    requests: list[httpx.Request] = []

    def fail(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        return httpx.Response(503, json={"error": "provider unavailable"})

    retry = build_service(
        dataset=dataset, database=database, store=store,
        embedder=make_embedder(fail),
    )
    await retry.ingest_document(
        "dataset-a", "document-a", source_text_override=CHANGED_CONTENT,
    )

    assert requests
    assert database.status_updates[-1] == "error"
    assert database.document["content"] == old_content
    assert text_rows(database) == old_rows
    assert store.points == old_points


@pytest.mark.asyncio
@pytest.mark.parametrize("bad_vector", [[0.25], [float("nan"), 0.5]])
async def test_invalid_provider_vector_keeps_serving_generation(
    bad_vector: list[float],
) -> None:
    dataset, database, store = make_world(SHORT_CONTENT)
    await ingest_once(dataset, database, store, SHORT_CONTENT)
    old_rows = deepcopy(text_rows(database))
    old_points = deepcopy(store.points)

    class InvalidEmbedder(CountingEmbedder):
        async def embed_documents(self, texts: list[str]) -> list[list[float]]:
            self.embedded_texts.extend(texts)
            return [bad_vector for _ in texts]

    service = build_service(
        dataset=dataset, database=database, store=store,
        embedder=InvalidEmbedder(),
    )
    await service.ingest_document(
        "dataset-a", "document-a", source_text_override=CHANGED_CONTENT,
    )

    assert database.status_updates[-1] == "error"
    assert database.document["content"] == SHORT_CONTENT
    assert text_rows(database) == old_rows
    assert store.points == old_points


@pytest.mark.asyncio
async def test_changed_content_updates_in_place_keeping_identity() -> None:
    dataset, database, store = make_world(LONG_CONTENT)
    await ingest_once(dataset, database, store, LONG_CONTENT)
    before = {
        position: (row["segment_id"], row["vector_id"])
        for position, row in text_rows(database).items()
    }
    assert sorted(before) == [0, 1]
    points_before = set(store.points)

    embedder = await ingest_once(dataset, database, store, CHANGED_CONTENT)

    # Both chunks changed -> both re-embedded, both updated in place.
    assert len(embedder.embedded_texts) == 2
    rows = text_rows(database)
    for position, row in rows.items():
        old_segment_id, old_vector_id = before[position]
        # Identity is preserved -> true in-place upsert, no FK/point rotation.
        assert row["segment_id"] == old_segment_id
        assert row["vector_id"] == old_vector_id
        assert row["enabled"] is True
        assert row["status"] == "completed"
    # The Qdrant points were replaced at the SAME ids, never deleted+added.
    assert set(store.points) == points_before
    assert set(store.deleted_ids).isdisjoint(points_before)


@pytest.mark.asyncio
async def test_retry_db_commit_failure_restores_old_row_point_and_payload() -> None:
    """The ON CONFLICT path must roll back both stores to the old revision."""

    dataset, database, store = make_world(LONG_CONTENT)
    await ingest_once(dataset, database, store, LONG_CONTENT)
    old_rows = deepcopy(text_rows(database))
    old_points = deepcopy(store.points)

    database.fail_next_insert = True
    await ingest_once(dataset, database, store, CHANGED_CONTENT)

    assert database.status_updates[-1] == "error"
    assert text_rows(database) == old_rows
    assert set(store.points) == set(old_points)
    for point_id, old_point in old_points.items():
        assert store.points[point_id].vector == old_point.vector
        assert store.points[point_id].payload == old_point.payload
    # Changed positions reused serving IDs, so rollback restored them; it did
    # not run the old destructive compensation that deleted those IDs.
    assert set(store.deleted_ids).isdisjoint(old_points)
    assert "publication:abort" in database.events


@pytest.mark.asyncio
async def test_reextract_failure_keeps_old_document_content_with_serving_segments() -> None:
    dataset, database, store = make_world(SHORT_CONTENT)
    await ingest_once(dataset, database, store, SHORT_CONTENT)
    old_rows = deepcopy(text_rows(database))
    old_content = database.document["content"]
    database.document["metadata"].update(
        original_file_key="original-key",
        original_filename="doc.txt",
        original_mime_type="text/plain",
    )
    replacement = ("new source text with different facts and much more detail. " * 6).strip()
    service = build_service(
        dataset=dataset,
        database=database,
        store=store,
        embedder=CountingEmbedder(),
    )
    service._ks.image_storage_service = SimpleNamespace(
        download_original_file=AsyncMock(return_value=replacement.encode())
    )
    service._ks._extract_text_from_bytes = lambda *_args: (replacement, None)
    store.replacement_upsert_calls = 0
    store.fail_replacement_upsert_at = 1

    await service.ingest_document("dataset-a", "document-a")

    assert database.status_updates[-1] == "error"
    assert database.document["content"] == old_content
    assert text_rows(database) == old_rows


@pytest.mark.asyncio
async def test_reextract_publishes_content_with_segments_after_preparation() -> None:
    dataset, database, store = make_world(SHORT_CONTENT)
    await ingest_once(dataset, database, store, SHORT_CONTENT)
    database.document["metadata"].update(
        original_file_key="original-key",
        original_filename="doc.txt",
        original_mime_type="text/plain",
    )
    replacement = ("published replacement facts for the same source. " * 7).strip()
    service = build_service(
        dataset=dataset,
        database=database,
        store=store,
        embedder=CountingEmbedder(),
    )
    service._ks.image_storage_service = SimpleNamespace(
        download_original_file=AsyncMock(return_value=replacement.encode())
    )
    service._ks._extract_text_from_bytes = lambda *_args: (replacement, None)
    store.replacement_upsert_calls = 0
    store.block_replacement_upsert_at = 1

    retry_task = asyncio.create_task(service.ingest_document("dataset-a", "document-a"))
    await asyncio.wait_for(store.blocked_replacement_upsert.wait(), timeout=2)
    assert database.document["content"] == SHORT_CONTENT
    assert text_rows(database)[0]["text"] == SHORT_CONTENT

    store.release_replacement_upsert.set()
    await asyncio.wait_for(retry_task, timeout=2)
    assert database.status_updates[-1] == "completed"
    assert database.document["content"] == replacement
    assert any("published replacement facts" in row["text"] for row in text_rows(database).values())


@pytest.mark.asyncio
async def test_specialized_extraction_candidate_keeps_old_version_on_failed_index() -> None:
    dataset, database, store = make_world(SHORT_CONTENT)
    await ingest_once(dataset, database, store, SHORT_CONTENT)
    old_rows = deepcopy(text_rows(database))
    database.document["metadata"]["original_file_key"] = "durable-original"
    replacement = "OCR replacement fact KITE-42 " * 12
    service = build_service(
        dataset=dataset, database=database, store=store,
        embedder=CountingEmbedder(),
    )
    service._ks.image_storage_service = SimpleNamespace(
        download_original_file=AsyncMock(side_effect=AssertionError("duplicate extraction"))
    )
    store.replacement_upsert_calls = 0
    store.fail_replacement_upsert_at = 1

    await service.ingest_document(
        "dataset-a", "document-a",
        source_text_override=replacement,
        candidate_metadata_patch={"vlm_ocr_pages": 2},
    )

    assert database.status_updates[-1] == "error"
    assert database.document["content"] == SHORT_CONTENT
    assert "vlm_ocr_pages" not in database.document["metadata"]
    assert text_rows(database) == old_rows

    store.fail_replacement_upsert_at = None
    await service.ingest_document(
        "dataset-a", "document-a",
        source_text_override=replacement,
        candidate_metadata_patch={"vlm_ocr_pages": 2},
    )
    assert database.status_updates[-1] == "completed"
    assert database.document["content"] == replacement
    assert database.document["metadata"]["vlm_ocr_pages"] == 2
    assert database.document["word_count"] == len(replacement.split())
    assert any("KITE-42" in row["text"] for row in text_rows(database).values())


@pytest.mark.asyncio
async def test_pending_restore_version_is_hidden_until_text_generation_publishes() -> None:
    dataset, database, store = make_world(SHORT_CONTENT)
    await ingest_once(dataset, database, store, SHORT_CONTENT)
    old_rows = deepcopy(text_rows(database))
    database.document["current_version"] = 1
    database.document["version_count"] = 1
    database.document["metadata"]["_document_pending_restore_version"] = {
        "previous_version": 2,
        "candidate_version": 3,
    }
    replacement = "KITE-42 restored answer"
    database.versions[2] = {
        "document_id": "document-a",
        "version_number": 2,
        "change_type": "pending_before_restore",
        "content": SHORT_CONTENT,
    }
    database.versions[3] = {
        "document_id": "document-a",
        "version_number": 3,
        "change_type": "pending_restore",
        "content": replacement,
    }
    service = build_service(
        dataset=dataset, database=database, store=store,
        embedder=CountingEmbedder(),
    )
    store.replacement_upsert_calls = 0
    store.fail_replacement_upsert_at = 1
    await service.ingest_document("dataset-a", "document-a")
    assert database.document["content"] == SHORT_CONTENT
    assert database.document["current_version"] == 1
    assert database.document["version_count"] == 1
    assert database.versions[2]["change_type"] == "pending_before_restore"
    assert database.versions[3]["change_type"] == "pending_restore"
    assert text_rows(database) == old_rows

    store.fail_replacement_upsert_at = None
    original_complete = service._complete_document_generation
    post_publication_complete = AsyncMock(side_effect=AssertionError("post-commit terminal write"))
    service._complete_document_generation = post_publication_complete  # type: ignore[method-assign]
    await service.ingest_document("dataset-a", "document-a")
    post_publication_complete.assert_not_awaited()
    service._complete_document_generation = original_complete  # type: ignore[method-assign]
    assert database.document["content"] == replacement
    assert database.document["current_version"] == 3
    assert database.document["version_count"] == 3
    assert database.versions[2]["change_type"] == "updated"
    assert database.versions[3]["change_type"] == "restored"
    assert "_document_pending_restore_version" not in database.document["metadata"]
    assert database.document["metadata"]["_document_restored_source_version"] == 3
    assert any("KITE-42" in row["text"] for row in text_rows(database).values())

    # Crash after the content/segment transaction but before terminal status:
    # replay must not re-extract a longer original upload over this short,
    # explicitly restored version.
    database.document["metadata"]["original_file_key"] = "old-upload"
    service._ks.image_storage_service = SimpleNamespace(
        download_original_file=AsyncMock(side_effect=AssertionError("old upload replayed"))
    )
    await service.ingest_document("dataset-a", "document-a")
    assert database.document["content"] == replacement
    service._ks.image_storage_service.download_original_file.assert_not_awaited()


@pytest.mark.asyncio
async def test_content_only_publication_uses_existing_revision_fence() -> None:
    dataset, database, store = make_world(SHORT_CONTENT)
    service = build_service(
        dataset=dataset,
        database=database,
        store=store,
        embedder=CountingEmbedder(),
    )

    await service._publish_text_generation(
        collection="collection-a",
        points=[],
        segment_rows=[],
        excess_vector_ids=[],
        overwritten_point_ids=[],
        keep_segment_ids=[],
        staged_segment_ids=[],
        delete_excess=False,
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        document_id="document-a",
        expected_ingestion_identity=database.dataset.get("ingestion_identity", ""),
        candidate_content="new serving content",
        expected_content=SHORT_CONTENT,
    )

    assert database.document["content"] == "new serving content"
    assert "publication:commit" in database.events


@pytest.mark.asyncio
async def test_retry_second_qdrant_batch_failure_restores_old_generation(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Retry never exposes a partially replaced multi-batch generation."""

    from knowledge_service.services.knowledge.vector_store import VectorStoreConfig

    dataset, database, store = make_world(LONG_CONTENT)
    await ingest_once(dataset, database, store, LONG_CONTENT)
    old_rows = deepcopy(text_rows(database))
    old_points = deepcopy(store.points)

    store.replacement_upsert_calls = 0
    store.fail_replacement_upsert_at = 2
    monkeypatch.setattr(VectorStoreConfig, "get_batch_size", lambda _total: 1)

    await ingest_once(dataset, database, store, CHANGED_CONTENT)

    assert database.status_updates[-1] == "error"
    assert text_rows(database) == old_rows
    assert set(store.points) == set(old_points)
    for point_id, old_point in old_points.items():
        assert store.points[point_id].vector == old_point.vector
        assert store.points[point_id].payload == old_point.payload
    assert "publication:abort" in database.events


@pytest.mark.asyncio
async def test_retry_publication_worker_cancellation_restores_old_generation(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Cancellation before PG commit rolls back Qdrant and preserves old PG rows."""

    from knowledge_service.services.knowledge.vector_store import VectorStoreConfig

    dataset, database, store = make_world(LONG_CONTENT)
    await ingest_once(dataset, database, store, LONG_CONTENT)
    old_rows = deepcopy(text_rows(database))
    old_points = deepcopy(store.points)

    store.replacement_upsert_calls = 0
    store.block_replacement_upsert_at = 2
    monkeypatch.setattr(VectorStoreConfig, "get_batch_size", lambda _total: 1)
    retry_task = asyncio.create_task(
        ingest_once(dataset, database, store, CHANGED_CONTENT)
    )
    await asyncio.wait_for(store.blocked_replacement_upsert.wait(), timeout=2)

    retry_task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(retry_task, timeout=2)

    assert text_rows(database) == old_rows
    assert set(store.points) == set(old_points)
    for point_id, old_point in old_points.items():
        assert store.points[point_id].vector == old_point.vector
        assert store.points[point_id].payload == old_point.payload
    assert "publication:abort" in database.events


TAIL_ONLY_CHANGED_CONTENT = (
    "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mux"
)


@pytest.mark.asyncio
async def test_single_chunk_change_reembeds_only_that_chunk() -> None:
    """§6.4 scenario 1 in exact form: modify ONE paragraph (here: the second
    of two chunks) — only that chunk is re-embedded, and the point set's
    symmetric difference is exactly the changed chunk. The engine guarantees
    the stronger property: even the changed chunk keeps its point id, so no
    identity rotates at all."""
    dataset, database, store = make_world(LONG_CONTENT)
    await ingest_once(dataset, database, store, LONG_CONTENT)
    before = {
        position: (row["segment_id"], row["vector_id"])
        for position, row in text_rows(database).items()
    }
    assert sorted(before) == [0, 1]
    sibling_vector = deepcopy(store.points[before[0][1]].vector)
    changed_vector_before = deepcopy(store.points[before[1][1]].vector)

    embedder = await ingest_once(dataset, database, store, TAIL_ONLY_CHANGED_CONTENT)

    # Exactly one chunk — the changed one — was re-embedded.
    assert len(embedder.embedded_texts) == 1
    assert "mux" in embedder.embedded_texts[0]
    rows = text_rows(database)
    # Symmetric difference of point sets == changed-chunk set: nothing added,
    # nothing removed ...
    assert set(store.points) == {before[0][1], before[1][1]}
    # ... and identities did not rotate.
    for position, row in rows.items():
        assert (row["segment_id"], row["vector_id"]) == before[position]
    # The sibling's vector is untouched; only the changed chunk got a new one.
    assert store.points[before[0][1]].vector == sibling_vector
    assert store.points[before[1][1]].vector != changed_vector_before


@pytest.mark.asyncio
async def test_retry_success_deletes_excess_only_after_staging_succeeds() -> None:
    dataset, database, store = make_world(LONG_CONTENT)
    await ingest_once(dataset, database, store, LONG_CONTENT)
    assert len(text_rows(database)) == 2
    excess_vector = text_rows(database)[1]["vector_id"]
    events_before = len(database.events)

    await ingest_once(dataset, database, store, SHORT_CONTENT)

    assert database.status_updates[-1] == "completed"
    rows = text_rows(database)
    assert sorted(rows) == [0]
    assert rows[0]["enabled"] is True
    # Staging (insert) happened before the excess deletion within this run.
    second_run_events = database.events[events_before:]
    insert_idx = next(
        i for i, event in enumerate(second_run_events) if event.startswith("insert:")
    )
    delete_idx = next(
        i for i, event in enumerate(second_run_events) if event.startswith("delete:")
    )
    assert insert_idx < delete_idx
    assert "delete:1" in second_run_events
    # The excess row's vector was removed from the store.
    assert excess_vector not in store.points
    assert any(
        excess_vector in event
        for event in store.events
        if event.startswith("delete-points")
    )


@pytest.mark.asyncio
async def test_crashed_staging_resumes_with_flip_and_no_reembedding() -> None:
    dataset, database, store = make_world(LONG_CONTENT)
    await ingest_once(dataset, database, store, LONG_CONTENT)

    # Simulate a crash right after staging persisted but before the flip.
    for row in database.segments.values():
        row["status"] = "indexing"
        row["enabled"] = False

    embedder = await ingest_once(dataset, database, store, LONG_CONTENT)

    # No re-embedding: staged rows with a matching hash only get flipped.
    assert embedder.embedded_texts == []
    assert database.status_updates[-1] == "completed"
    rows = text_rows(database)
    assert all(
        row["status"] == "completed" and row["enabled"] is True
        for row in rows.values()
    )
    assert any(event.startswith("activate:2") for event in database.events)


@pytest.mark.asyncio
async def test_operator_disabled_serving_row_is_not_reenabled() -> None:
    dataset, database, store = make_world(LONG_CONTENT)
    await ingest_once(dataset, database, store, LONG_CONTENT)

    # Operator disables one serving chunk (disable path keeps status completed).
    disabled_key = next(iter(database.segments))
    database.segments[disabled_key]["enabled"] = False

    await ingest_once(dataset, database, store, LONG_CONTENT)

    assert database.status_updates[-1] == "completed"
    assert database.segments[disabled_key]["enabled"] is False
    assert database.segments[disabled_key]["status"] == "completed"
    # The untouched rows remain serving.
    serving = [
        row
        for key, row in database.segments.items()
        if key != disabled_key and row.get("status") == "completed"
    ]
    assert serving and all(row["enabled"] is True for row in serving)


def test_stable_segment_identity_is_deterministic() -> None:
    first = _stable_segment_id("document-a", "text", 3)
    second = _stable_segment_id("document-a", "text", 3)
    assert first == second
    # Different positions/content types/documents never collide.
    assert _stable_segment_id("document-a", "text", 4) != first
    assert _stable_segment_id("document-a", "image", 3) != first
    assert _stable_segment_id("document-b", "text", 3) != first
    assert _stable_index_node_id("document-a", "text", 3) == "document-a::text::3"


@pytest.mark.asyncio
async def test_chunking_override_beats_live_dataset_config() -> None:
    """Addendum §1-T1 anti-drift acceptance (unit form).

    A replay pins the submission-time chunking snapshot. Even if the dataset
    config drifts after submission, the override must drive the chunking:
    replaying with the original snapshot keeps every chunk identical (zero
    re-embedding), while the drifted live config would re-cut the document.
    """

    dataset, database, store = make_world(LONG_CONTENT)
    await ingest_once(dataset, database, store, LONG_CONTENT)
    first_rows = {
        position: row["content_hash"] for position, row in text_rows(database).items()
    }
    assert sorted(first_rows) == [0, 1]

    # Operator drifts the live config after the replay was submitted.
    drifted = dict(CHUNKING_CONFIG)
    drifted["chunk_size"] = 30
    drifted["min_chunk_size"] = 30
    drifted["max_chunk_size"] = 40
    dataset["index_config"]["chunking"] = drifted

    embedder = CountingEmbedder()
    service = build_service(
        dataset=dataset, database=database, store=store, embedder=embedder
    )
    await service.ingest_document(
        "dataset-a",
        "document-a",
        chunking_config_override=dict(CHUNKING_CONFIG),
    )

    assert database.status_updates[-1] == "completed"
    rows = text_rows(database)
    # Snapshot chunking won: identical chunks, no re-embedding.
    assert {
        position: row["content_hash"] for position, row in rows.items()
    } == first_rows
    assert embedder.embedded_texts == []

    # Sanity: the drifted live config must never silently re-cut the document.
    # A config outside the validator's bounds fails closed instead of
    # chunking with different rules than the pinned snapshot.
    embedder_drift = CountingEmbedder()
    service_drift = build_service(
        dataset=dataset, database=database, store=store, embedder=embedder_drift
    )
    await service_drift.ingest_document("dataset-a", "document-a")
    assert database.status_updates[-1] != "completed"
    assert embedder_drift.embedded_texts == []
    assert {
        position: row["content_hash"] for position, row in text_rows(database).items()
    } == first_rows


# ------------------------------------------------- T3 ingestion provenance


def _provenance_stamp(database: EngineDatabase) -> dict[str, Any]:
    """The update_document_fields call carrying the embedding identity."""
    stamped = [
        fields for fields in database.field_updates if "embedding_model" in fields
    ]
    assert len(stamped) == 1, f"expected one provenance stamp, got {stamped}"
    return stamped[0]


@pytest.mark.asyncio
async def test_ingest_stamps_embedding_provenance_from_dataset() -> None:
    """Migration-102 provenance: the document row records which model/version
    embedded its vectors, so the retrieval identity guard and future audits
    can reason per-document."""
    dataset, database, store = make_world(LONG_CONTENT)
    dataset["embedding_model_version"] = "v1"

    await ingest_once(dataset, database, store, LONG_CONTENT)

    assert database.status_updates[-1] == "completed"
    stamp = _provenance_stamp(database)
    assert stamp["embedding_model"] == "hash-384"
    assert stamp["embedding_model_version"] == "v1"
    assert stamp["embedding_dimension"] == 2


@pytest.mark.asyncio
async def test_ingest_provenance_falls_back_to_embedder_identity() -> None:
    """Legacy datasets without an embedding_model/version on the row still get
    a stamp — from the embedder that actually produced the vectors."""
    dataset, database, store = make_world(LONG_CONTENT)
    dataset["embedding_model"] = ""

    class IdentifiedEmbedder(CountingEmbedder):
        model = "fallback-model"
        model_version = "v7"

    embedder = IdentifiedEmbedder()
    service = build_service(
        dataset=dataset, database=database, store=store, embedder=embedder
    )
    await service.ingest_document("dataset-a", "document-a")

    assert database.status_updates[-1] == "completed"
    stamp = _provenance_stamp(database)
    assert stamp["embedding_model"] == "fallback-model"
    assert stamp["embedding_model_version"] == "v7"


@pytest.mark.asyncio
async def test_ingest_provenance_failure_never_fails_the_generation() -> None:
    """Degrade-safe contract: a pre-102 database (missing provenance columns)
    must not break ingestion — the stamp is swallowed and the generation
    completes."""
    dataset, database, store = make_world(LONG_CONTENT)
    database.fail_field_updates = True

    embedder = await ingest_once(dataset, database, store, LONG_CONTENT)

    assert database.status_updates[-1] == "completed"
    assert len(embedder.embedded_texts) == 2
    assert sorted(text_rows(database)) == [0, 1]
