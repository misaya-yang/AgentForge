"""Capture → ingest_trace → get_trace_detail roundtrip for all trace families."""

from __future__ import annotations

from datetime import datetime

import pytest

from tests.services.eval.in_memory_trace_repository import InMemoryTraceRepository
from tests.services.eval.trace_roundtrip_fixtures import seed_family


def _assert_detail_tree(detail: dict, *, family: str) -> None:
    spans = detail["spans"]
    lifecycle = next(s for s in spans if s.get("parent_span_id") is None)
    children = [s for s in spans if s.get("parent_span_id") is not None]
    print(
        f"ROUNDTRIP family={family} trace_id={detail['trace']['trace_id']} "
        f"lifecycle={lifecycle['span_id']} children={len(children)} "
        f"events={len(detail['events'])}"
    )
    assert detail["trace"]["trace_family"] == family
    assert children, f"{family} detail must include child spans"
    assert all(child["parent_span_id"] == lifecycle["span_id"] for child in children)
    if family == "langgraph_proxy":
        assert any(s["span_kind"] == "gateway_proxy" for s in children)
        assert detail["trace"].get("otel_trace_id")
    if family == "rag":
        assert any(s["span_kind"] == "retriever" for s in children)
        assert detail["trace"].get("metadata", {}).get("dataset_id") == "ds-1"
    if family == "assistant":
        assert any(s["span_kind"] != "lifecycle" for s in children)
        assert detail["trace"].get("otel_trace_id")


@pytest.mark.asyncio
async def test_capture_ingest_detail_roundtrip_all_families(
    capsys: pytest.CaptureFixture[str],
) -> None:
    repo = InMemoryTraceRepository()
    ingested_ids: dict[str, str] = {}

    for family in ("assistant", "langgraph_proxy", "rag"):
        ingested_ids[family] = await seed_family(repo, family, request_suffix="ingest")
        detail = await repo.get_trace_detail(
            tenant_id="tenant-a",
            trace_id=ingested_ids[family],
            trace_family=family,
        )
        assert detail is not None
        _assert_detail_tree(detail, family=family)

    listed, total = await repo.list_traces(
        tenant_id="tenant-a",
        trace_family="assistant",
    )
    assert total == 1
    assert listed[0]["trace_id"] == ingested_ids["assistant"]

    out = capsys.readouterr().out
    print(out)
    assert "ROUNDTRIP family=assistant" in out
    assert "ROUNDTRIP family=langgraph_proxy" in out
    assert "ROUNDTRIP family=rag" in out


@pytest.mark.asyncio
async def test_gateway_trace_ingest_coerces_iso_timestamp_strings() -> None:
    repo = InMemoryTraceRepository()
    trace_id = "11111111-1111-4111-8111-111111111111"
    span_id = "22222222-2222-4222-8222-222222222222"
    await repo.ingest_trace(
        tenant_id="tenant-a",
        created_by="user-a",
        payload={
            "trace": {
                "trace_id": trace_id,
                "trace_family": "rag",
                "started_at": "2026-07-01T06:25:44.973968Z",
                "ended_at": "2026-07-01T06:25:45.073968+00:00",
                "retention_expires_at": "2026-09-29T06:25:44.973968+00:00",
                "spans": [
                    {
                        "span_id": span_id,
                        "span_kind": "retriever",
                        "started_at": "2026-07-01T06:25:44.973968Z",
                        "ended_at": "2026-07-01T06:25:45.073968+00:00",
                    }
                ],
                "events": [
                    {
                        "event_type": "rag_retrieval_completed",
                        "sequence_no": 1,
                        "occurred_at": "2026-07-01T06:25:45.073968Z",
                    }
                ],
            }
        },
        enqueue=False,
    )
    trace_row = repo.traces[trace_id]
    span_rows = repo.spans[trace_id]
    event_rows = repo.events[trace_id]

    assert isinstance(trace_row["started_at"], datetime)
    assert isinstance(trace_row["ended_at"], datetime)
    assert isinstance(trace_row["retention_expires_at"], datetime)
    assert all(isinstance(row["started_at"], datetime) for row in span_rows)
    assert all(isinstance(row["ended_at"], datetime) for row in span_rows)
    assert all(isinstance(row["occurred_at"], datetime) for row in event_rows)
