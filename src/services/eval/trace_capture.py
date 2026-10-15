"""Best-effort gateway trace ingest helpers for LangGraph proxy and RAG families."""

from __future__ import annotations

import asyncio
import logging
import re
import uuid
from datetime import datetime, timedelta, timezone
from typing import Any

from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository

logger = logging.getLogger(__name__)

_REDACTION_PATTERNS: tuple[tuple[re.Pattern[str], str], ...] = (
    (
        re.compile(r"(?i)\bauthorization\s*[:=]\s*bearer\s+[A-Za-z0-9._~+/=-]+"),
        "Authorization: Bearer [redacted]",
    ),
    (
        re.compile(
            r"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password)"
            r"\s*[:=]\s*[\"']?[^\"'\s,;}]+"
        ),
        r"\1=[redacted]",
    ),
)


def redact_preview(value: Any, *, limit: int = 500) -> str:
    text = str(value or "")
    for pattern, replacement in _REDACTION_PATTERNS:
        text = pattern.sub(replacement, text)
    if len(text) <= limit:
        return text
    return f"{text[:limit]}...[truncated]"


def trace_id_for_request(*, request_id: str, trace_family: str, route_key: str) -> str:
    seed = f"{trace_family}:{request_id}:{route_key}"
    return str(uuid.uuid5(uuid.NAMESPACE_URL, seed))


def span_id_for(trace_id: str, span_key: str) -> str:
    return str(uuid.uuid5(uuid.UUID(trace_id), span_key))


def retention_expires_at(*, retention_days: int) -> datetime:
    return datetime.now(timezone.utc) + timedelta(days=max(1, retention_days))


def schedule_gateway_trace_ingest(
    database: Any,
    *,
    tenant_id: str,
    created_by: str,
    trace: dict[str, Any],
    retention_days: int = 90,
    enqueue: bool = False,
) -> None:
    if not getattr(database, "enabled", False):
        return
    trace = dict(trace)
    trace["retention_expires_at"] = retention_expires_at(retention_days=retention_days).isoformat()
    payload = {"trace": trace, "enqueue": enqueue}

    async def _ingest() -> None:
        try:
            repository = AgentTraceRepository(database)
            if trace.get("source_adapter") == "gateway.agent_runtime":
                from .assistant_trace_capture import enrich_runtime_trace_usage

                await enrich_runtime_trace_usage(repository, trace)
            await repository.ingest_trace(
                tenant_id=tenant_id,
                created_by=created_by,
                payload=payload,
                enqueue=enqueue,
            )
        except Exception:
            logger.exception("Gateway trace ingest failed for trace_family=%s", trace.get("trace_family"))

    try:
        loop = asyncio.get_running_loop()
        loop.create_task(_ingest())
    except RuntimeError:
        logger.warning("No running event loop; skipped gateway trace ingest")
