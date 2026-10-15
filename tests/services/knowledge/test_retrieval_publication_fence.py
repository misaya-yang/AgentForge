"""Retrieval seqlock regressions for cross-store segment publication."""

from __future__ import annotations

import asyncio

import pytest
from knowledge_service.persistence.database import IndexLeaseUnavailableError
from knowledge_service.services.knowledge import retrieval_service as retrieval_module
from knowledge_service.services.knowledge.retrieval_service import (
    _with_interactive_qdrant_budget,
    dataset_retrieval_generation,
)


class _PublicationHarness:
    vector_store = object()

    def __init__(self, revision: int) -> None:
        self.revision = revision
        self.attempts = 0

    @_with_interactive_qdrant_budget
    async def read_generation(self) -> tuple[str, object, str, str, str, str, int]:
        self.attempts += 1
        return dataset_retrieval_generation(
            {
                "tenant_id": "tenant-a",
                "content_revision": self.revision,
                "index_config": {},
            }
        )


@pytest.mark.asyncio
async def test_reader_waits_through_publication_and_returns_only_new_revision() -> None:
    harness = _PublicationHarness(revision=-8)

    async def complete_publication() -> None:
        await asyncio.sleep(0.04)
        harness.revision = 9

    publisher = asyncio.create_task(complete_publication())
    observed = await harness.read_generation()
    await publisher

    assert observed == ("tenant-a", 9, "", "", "", "", 0)
    assert harness.attempts >= 2


@pytest.mark.asyncio
async def test_publication_retry_is_bounded(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(retrieval_module, "_PUBLICATION_RETRY_LIMIT", 1)
    monkeypatch.setattr(retrieval_module, "_PUBLICATION_RETRY_BASE_SECONDS", 0.001)
    monkeypatch.setattr(retrieval_module, "_PUBLICATION_RETRY_MAX_SECONDS", 0.001)
    harness = _PublicationHarness(revision=-1)

    with pytest.raises(
        IndexLeaseUnavailableError,
        match="publication is still in progress",
    ):
        await harness.read_generation()

    assert harness.attempts == 2
