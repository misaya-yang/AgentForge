"""Ordinary text restore preparation has no serving side effects."""

from __future__ import annotations

import hashlib
from types import SimpleNamespace

import pytest
from knowledge_service.services.knowledge.text_restore_candidate import (
    TextRestoreCandidatePlanner,
)

GENERATION = "00000000-0000-4000-8000-000000000101"
NEXT_GENERATION = "00000000-0000-4000-8000-000000000102"
DATASET = {
    "dataset_id": "dataset-a", "tenant_id": "tenant-a",
    "collection_name": "base", "embedding_dimension": 2,
}


class Embedder:
    def __init__(self, *, incomplete: bool = False) -> None:
        self.incomplete = incomplete
        self.calls: list[list[str]] = []
        self.closed = False

    async def embed_documents(self, texts: list[str]) -> list[list[float]]:
        self.calls.append(texts)
        if self.incomplete:
            return []
        return [[float(len(text)), 0.5] for text in texts]

    async def close(self) -> None:
        self.closed = True


class Service:
    def __init__(self, *, incomplete: bool = False, multimodal: bool = False) -> None:
        self.embedder = Embedder(incomplete=incomplete)
        self.multimodal = multimodal
        self.settings = SimpleNamespace(
            knowledge=SimpleNamespace(text_embedding_batch_size=2),
        )
        self.db = SimpleNamespace()
        self.vector_store = SimpleNamespace()

    def _is_multimodal_dataset(self, _dataset: dict) -> bool:
        return self.multimodal

    async def _get_text_embedder(self, _dataset: dict, _config: dict) -> Embedder:
        assert not self.multimodal
        return self.embedder

    async def _get_unified_multimodal_embedder(
        self, _dataset: dict, _config: dict,
    ) -> Embedder:
        assert self.multimodal
        return self.embedder


@pytest.mark.asyncio
async def test_text_restore_plan_is_complete_stable_and_unpublished() -> None:
    content = "  Historical ordinary text with a stable source.  \n"
    service = Service()
    planner = TextRestoreCandidatePlanner(service)
    options = {
        "document_id": "document-a", "dataset": DATASET,
        "content": content, "title": "Document A", "chunking": {},
    }
    first = await planner.prepare(generation_id=GENERATION, **options)
    replay = await planner.prepare(generation_id=GENERATION, **options)
    next_generation = await planner.prepare(generation_id=NEXT_GENERATION, **options)

    assert first.source_kind == "text"
    assert first.content == content
    assert first.source_hash == hashlib.sha256(content.encode()).hexdigest()
    assert first.object_manifest == [] and first.summary_row is None
    assert first.segment_ids == replay.segment_ids
    assert first.segment_ids != next_generation.segment_ids
    assert len(first.segment_rows) == len(first.points_by_collection["base"]) > 0
    assert all(row["status"] == "indexing" and row["enabled"] is False
               for row in first.segment_rows)
    assert all(point.payload["document_id"] == "document-a"
               and point.payload["content_type"] == "text"
               and "level" not in point.payload
               for point in first.points_by_collection["base"])
    assert service.embedder.closed


@pytest.mark.asyncio
async def test_text_restore_uses_dataset_unified_embedding_space() -> None:
    service = Service(multimodal=True)
    plan = await TextRestoreCandidatePlanner(service).prepare(
        document_id="document-a", dataset=DATASET, generation_id=GENERATION,
        content="Historical ordinary text", title="Document A", chunking={},
    )
    assert plan.segment_ids
    assert service.embedder.calls and service.embedder.closed


@pytest.mark.asyncio
async def test_text_restore_refuses_partial_embedding_before_publication() -> None:
    service = Service(incomplete=True)
    with pytest.raises(RuntimeError, match="embedding batch is incomplete"):
        await TextRestoreCandidatePlanner(service).prepare(
            document_id="document-a", dataset=DATASET, generation_id=GENERATION,
            content="Historical ordinary text", title="Document A", chunking={},
        )
    assert service.embedder.closed
