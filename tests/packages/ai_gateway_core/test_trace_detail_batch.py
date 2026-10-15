from __future__ import annotations

from typing import Any

import pytest
from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository


class RecordingBatchRepository(AgentTraceRepository):
    def __init__(self) -> None:
        self.fetch_calls: list[tuple[str, tuple[Any, ...]]] = []

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        self.fetch_calls.append((query, args))
        if "FROM agent_traces" in query:
            return [{"trace_id": "trace-1", "metadata": {}, "metrics": {}, "privacy": {}}]
        assert args[0] == ["trace-1"]
        if "FROM agent_trace_spans" in query:
            return [{"trace_id": "trace-1", "span_id": "span-1", "attributes": {}}]
        if "FROM agent_trace_events" in query:
            return [{"trace_id": "trace-1", "event_id": "event-1", "payload": {}}]
        if "FROM agent_trace_scores" in query:
            return [{"trace_id": "trace-1", "score_id": "score-1", "metadata": {}}]
        raise AssertionError(f"Unexpected query: {query}")


@pytest.mark.asyncio
async def test_get_trace_details_uses_four_queries_and_filters_child_ids() -> None:
    repo = RecordingBatchRepository()

    details = await repo.get_trace_details(
        tenant_id="tenant-a",
        trace_ids=["trace-1", "foreign-trace", "trace-1"],
        trace_family="rag",
    )

    assert list(details) == ["trace-1"]
    assert len(repo.fetch_calls) == 4
    assert details["trace-1"]["trace"]["scores_count"] == 1
    assert [span["span_id"] for span in details["trace-1"]["spans"]] == ["span-1"]
    assert [event["event_id"] for event in details["trace-1"]["events"]] == ["event-1"]
    assert [score["score_id"] for score in details["trace-1"]["scores"]] == ["score-1"]
