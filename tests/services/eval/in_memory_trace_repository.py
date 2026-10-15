"""In-memory AgentTraceRepository for capture → ingest → detail roundtrip tests."""

from __future__ import annotations

import re
from contextlib import asynccontextmanager
from copy import deepcopy
from datetime import datetime, timezone
from typing import Any

from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository


def _normalize_query(query: str) -> str:
    return re.sub(r"\s+", " ", query.strip().lower())


class InMemoryTraceRepository(AgentTraceRepository):
    """Intercepts SQL from AgentTraceRepository and stores rows in memory."""

    def __init__(self) -> None:
        self.traces: dict[str, dict[str, Any]] = {}
        self.spans: dict[str, list[dict[str, Any]]] = {}
        self.events: dict[str, list[dict[str, Any]]] = {}
        self.scores: dict[str, list[dict[str, Any]]] = {}

    @property
    def _pool(self):
        return self

    @asynccontextmanager
    async def acquire(self):
        yield self

    @asynccontextmanager
    async def transaction(self):
        snapshot = deepcopy((self.traces, self.spans, self.events, self.scores))
        try:
            yield
        except BaseException:
            self.traces, self.spans, self.events, self.scores = snapshot
            raise

    async def execute(self, query: str, *args: Any) -> str:
        return "UPDATE 0"

    @property
    def enabled(self) -> bool:
        return True

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        normalized = _normalize_query(query)

        if "insert into agent_traces" in normalized and "returning trace_id" in normalized:
            trace_id = str(args[0])
            old = self.traces.get(trace_id)
            if old and "where agent_traces.tenant_id = excluded.tenant_id" in normalized:
                if old["tenant_id"] != args[3] or (args[30] is not None and old["user_id"] != args[30]):
                    return None
            row = {
                "trace_id": trace_id,
                "trace_family": args[1],
                "workflow_kind": args[2],
                "tenant_id": args[3],
                "user_id": args[4],
                "session_id": args[5],
                "thread_id": args[6],
                "run_id": args[7],
                "request_id": args[8],
                "otel_trace_id": args[9],
                "traceparent": args[10],
                "model_id": args[11],
                "provider": args[12],
                "status": args[13],
                "started_at": args[14],
                "ended_at": args[15],
                "total_latency_ms": args[16],
                "first_token_latency_ms": args[17],
                "input_tokens": args[18],
                "output_tokens": args[19],
                "total_tokens": args[20],
                "total_cost_cents": args[21],
                "input_preview": args[22],
                "output_preview": args[23],
                "redaction_state": args[24],
                "metadata": args[25],
                "metrics": args[26],
                "privacy": args[27],
                "source_adapter": args[28],
                "retention_expires_at": args[29],
                "created_at": datetime.now(timezone.utc),
                "updated_at": datetime.now(timezone.utc),
            }
            self.traces[trace_id] = row
            return {"trace_id": trace_id}

        if "insert into agent_trace_spans" in normalized and "returning span_id" in normalized:
            span_id = str(args[0])
            trace_id = str(args[1])
            old = next((row for rows in self.spans.values() for row in rows if row["span_id"] == span_id), None)
            if old and "where agent_trace_spans.trace_id = excluded.trace_id" in normalized and old["trace_id"] != trace_id:
                return None
            span_row = {
                "span_id": span_id,
                "trace_id": trace_id,
                "parent_span_id": str(args[2]) if args[2] else None,
                "span_kind": args[3],
                "name": args[4],
                "status": args[5],
                "sequence_no": args[6],
                "started_at": args[7],
                "ended_at": args[8],
                "duration_ms": args[9],
                "input_preview": args[10],
                "output_preview": args[11],
                "attributes": args[12],
                "error_type": args[13],
                "error_message": args[14],
                "created_at": datetime.now(timezone.utc),
            }
            self.spans.setdefault(trace_id, []).append(span_row)
            return {"span_id": span_id}

        if "insert into agent_trace_events" in normalized and "returning event_id" in normalized:
            trace_id = str(args[0])
            event_row = {
                "event_id": f"event-{trace_id}-{args[3]}",
                "trace_id": trace_id,
                "span_id": str(args[1]) if args[1] else None,
                "event_type": args[2],
                "sequence_no": args[3],
                "occurred_at": args[4],
                "payload": args[5],
                "payload_size_bytes": args[6],
                "redacted": args[7],
                "created_at": datetime.now(timezone.utc),
            }
            self.events.setdefault(trace_id, []).append(event_row)
            return {"event_id": event_row["event_id"]}

        if normalized.startswith("select span_id from agent_trace_spans where"):
            return next((row for row in self.spans.get(str(args[0]), []) if row["span_id"] == str(args[1])), None)

        if "select *, 0::int as scores_count from agent_traces where" in normalized:
            trace_id, tenant_id, trace_family = str(args[0]), args[1], args[2]
            row = self.traces.get(trace_id)
            if not row or row["tenant_id"] != tenant_id or row["trace_family"] != trace_family:
                return None
            if len(args) > 3 and row.get("user_id") != args[3]:
                return None
            return row

        if "select count(*) as total from agent_traces t where" in normalized:
            tenant_id, trace_family = args[0], args[1]
            total = sum(
                1
                for row in self.traces.values()
                if row["tenant_id"] == tenant_id and row["trace_family"] == trace_family
            )
            return {"total": total}

        if normalized.startswith("select count(*)::int as total_traces"):
            tenant_id = args[0]
            rows = [row for row in self.traces.values() if row["tenant_id"] == tenant_id]
            return {
                "total_traces": len(rows),
                "failed_traces": sum(1 for row in rows if row["status"] == "failed"),
                "succeeded_traces": sum(1 for row in rows if row["status"] == "succeeded"),
                "assistant_traces": sum(1 for row in rows if row["trace_family"] == "assistant"),
                "langgraph_traces": sum(1 for row in rows if row["trace_family"] == "langgraph_proxy"),
                "rag_traces": sum(1 for row in rows if row["trace_family"] == "rag"),
                "avg_latency_ms": int(
                    sum(int(row.get("total_latency_ms") or 0) for row in rows) / max(len(rows), 1)
                ),
                "p95_latency_ms": max((int(row.get("total_latency_ms") or 0) for row in rows), default=0),
                "total_tokens": sum(int(row.get("total_tokens") or 0) for row in rows),
                "total_cost_cents": sum(int(row.get("total_cost_cents") or 0) for row in rows),
            }

        if "select count(distinct s.trace_id)::int as scored_traces" in normalized:
            tenant_id = args[0]
            scored = {
                score["trace_id"]
                for scores in self.scores.values()
                for score in scores
                if self.traces.get(score["trace_id"], {}).get("tenant_id") == tenant_id
            }
            return {"scored_traces": len(scored)}

        return None

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        normalized = _normalize_query(query)

        if "from agent_trace_spans" in normalized:
            trace_id = str(args[0])
            rows = sorted(
                self.spans.get(trace_id, []),
                key=lambda row: (int(row.get("sequence_no") or 0), str(row.get("started_at") or "")),
            )
            return rows

        if "from agent_trace_events" in normalized:
            trace_id = str(args[0])
            rows = sorted(
                self.events.get(trace_id, []),
                key=lambda row: (int(row.get("sequence_no") or 0), str(row.get("occurred_at") or "")),
            )
            return rows

        if "from agent_trace_scores" in normalized:
            trace_id = str(args[0])
            return list(self.scores.get(trace_id, []))

        if "from agent_traces t" in normalized and "group by t.trace_id" in normalized:
            tenant_id, trace_family = args[0], args[1]
            limit = args[-2]
            offset = args[-1]
            rows = [
                {**row, "scores_count": len(self.scores.get(trace_id, []))}
                for trace_id, row in self.traces.items()
                if row["tenant_id"] == tenant_id and row["trace_family"] == trace_family
            ]
            rows.sort(key=lambda row: str(row.get("created_at") or ""), reverse=True)
            return rows[offset : offset + limit]

        return []
