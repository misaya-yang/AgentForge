"""QA observations bind only to source versions proved at retrieval time."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.qa_service import LLMConfig, QAService
from knowledge_service.services.knowledge.retrieval_service import RetrieveResult

DATASET = {
    "tenant_id": "tenant-a", "content_revision": 4,
    "collection_name": "base", "index_config": {},
}
HASH = "a" * 64
USER = SimpleNamespace(tenant_id="tenant-a")


class Knowledge:
    def __init__(self) -> None:
        self.revision = 4
        self.db = SimpleNamespace(published_source_identities=AsyncMock(
            return_value={"document-a": (2, HASH)},
        ))
        self.results = [
            RetrieveResult("segment-a", "document-a", 0.9, "published", {}),
            RetrieveResult("segment-manual", "document-a", 0.8, "manual", {
                "source_type": "manual", "source_version": 99, "source_hash": "b" * 64,
            }),
        ]

    async def require_dataset_access(self, *_args, **_kwargs):
        return {**DATASET, "content_revision": self.revision}

    async def retrieve(self, **_kwargs):
        return self.results, {"trace_id": "trace-a"}


class LLM:
    async def chat_completion(self, _messages):
        return "answer", 3

    async def chat_completion_stream(self, _messages):
        yield {"type": "delta", "content": "answer"}


@pytest.mark.asyncio
async def test_qa_query_carries_only_authoritative_source_pair() -> None:
    knowledge = Knowledge()
    service = QAService(knowledge, LLMConfig(model="test"))
    service._get_llm_client = lambda: LLM()  # type: ignore[method-assign]

    result = await service.query(USER, "dataset-a", "question", include_raw_results=True)

    assert result.context_segments[0]["source_version"] == 2
    assert result.context_segments[0]["source_hash"] == HASH
    assert result.context_segments[0]["metadata"]["source_hash"] == HASH
    assert "source_version" not in result.context_segments[1]
    assert "source_hash" not in result.context_segments[1]["metadata"]
    knowledge.db.published_source_identities.assert_awaited_once()


@pytest.mark.asyncio
async def test_qa_stream_reuses_source_pair_in_retrieval_and_done() -> None:
    service = QAService(Knowledge(), LLMConfig(model="test"))
    service._get_llm_client = lambda: LLM()  # type: ignore[method-assign]

    events = [item async for item in service.query_stream(USER, "dataset-a", "question")]

    retrieval = events[0]["data"]["context_segments"][0]
    done = events[-1]["data"]["result"]["context_segments"][0]
    assert events[0]["event"] == "retrieval"
    assert retrieval["source_version"] == done["source_version"] == 2
    assert retrieval["source_hash"] == done["source_hash"] == HASH


@pytest.mark.asyncio
async def test_qa_refuses_source_attribution_after_generation_change() -> None:
    knowledge = Knowledge()

    async def changed_retrieve(**_kwargs):
        knowledge.revision = 5
        return knowledge.results, {}

    knowledge.retrieve = changed_retrieve  # type: ignore[method-assign]
    service = QAService(knowledge, LLMConfig(model="test"))
    service._get_llm_client = lambda: LLM()  # type: ignore[method-assign]

    with pytest.raises(ValidationFailedError, match="generation changed"):
        await service.query(USER, "dataset-a", "question")
    knowledge.db.published_source_identities.assert_not_awaited()
