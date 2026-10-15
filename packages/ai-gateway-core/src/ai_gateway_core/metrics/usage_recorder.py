"""
Usage Recorder Service - Persistent storage for usage metrics.

This service handles:
- Recording individual usage events to PostgreSQL
- Batch processing for high-throughput scenarios
- Cost calculation based on model pricing
- Updating daily aggregates
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import time
import uuid
from dataclasses import dataclass, field, replace
from datetime import date, datetime, timezone
from decimal import Decimal
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from asyncpg import Connection

    from ai_gateway_core.persistence import DatabaseStorage

import contextlib

from ai_gateway_core.billing import (
    DEFAULT_TOKEN_PRICING_PER_1K_USD,
    build_pricing_snapshot,
    resolve_pricing_with_status,
    usd_to_microcents,
    validate_pricing_snapshot,
)

from .observability import KNOWN_ERROR_TYPES, should_sample_trace

logger = logging.getLogger(__name__)

# Global singleton instance
_usage_recorder: UsageRecorder | None = None

UNATTRIBUTED_PROVIDER = "unattributed"
UNATTRIBUTED_MODEL = "unattributed_model"
UNATTRIBUTED_SERVICE = "unattributed_service"


@dataclass
class UsageRecord:
    """Single usage record data structure."""

    tenant_id: str
    user_id: str
    model: str
    input_tokens: int = 0
    output_tokens: int = 0
    request_id: str = ""
    service_id: str = ""
    assistant_id: str = ""
    provider: str = ""
    latency_ms: int = 0
    first_token_ms: int = 0
    request_total_duration_ms: int = 0
    llm_inference_duration_ms: int = 0
    retrieval_duration_ms: int = 0
    tool_call_duration_ms: int = 0
    agent_or_graph_overhead_ms: int = 0
    tool_call_breakdown: dict[str, int] = field(default_factory=dict)
    error_type: str | None = None
    status: str = "success"
    request_type: str = "chat"
    metadata: dict[str, Any] = field(default_factory=dict)
    trace_steps: list[dict[str, Any]] = field(default_factory=list)
    timestamp: float = field(default_factory=time.time)

    # Computed fields (filled by UsageRecorder)
    input_cost_cents: int = 0
    output_cost_cents: int = 0


def _hour_bucket(ts: float) -> datetime:
    """Convert timestamp to hour bucket as naive UTC datetime."""
    dt = datetime.fromtimestamp(ts, tz=timezone.utc)
    # Return naive datetime (no timezone) to match TIMESTAMP columns in PostgreSQL
    return dt.replace(minute=0, second=0, microsecond=0, tzinfo=None)


def group_records_by_hour(records: list[UsageRecord]) -> dict[tuple, dict[str, Any]]:
    aggregates: dict[tuple, dict[str, Any]] = {}
    for record in records:
        bucket = _hour_bucket(record.timestamp)
        key = (
            record.tenant_id,
            record.user_id or "",
            record.model or "",
            record.assistant_id or "",
            record.service_id or "",
            bucket,
        )

        if key not in aggregates:
            aggregates[key] = {
                "request_count": 0,
                "success_count": 0,
                "error_count": 0,
                "total_input_tokens": 0,
                "total_output_tokens": 0,
                "total_cost_cents": 0,
                "latency_sum": 0,
                "first_token_sum": 0,
            }

        agg = aggregates[key]
        metadata = record.metadata or {}
        request_delta = int(metadata.get("_accounting_request_delta", 1))
        agg["request_count"] += request_delta
        if "_accounting_success_delta" in metadata:
            agg["success_count"] += int(metadata["_accounting_success_delta"])
            agg["error_count"] += int(metadata.get("_accounting_error_delta", 0))
        elif record.status == "success":
            agg["success_count"] += 1
        elif record.status not in {"running", "pending"}:
            agg["error_count"] += 1
        agg["total_input_tokens"] += record.input_tokens
        agg["total_output_tokens"] += record.output_tokens
        agg["total_cost_cents"] += record.input_cost_cents + record.output_cost_cents
        total_duration = (
            int(record.request_total_duration_ms)
            if int(record.request_total_duration_ms or 0) > 0
            else int(record.latency_ms or 0)
        )
        if request_delta:
            agg["latency_sum"] += total_duration
            agg["first_token_sum"] += record.first_token_ms

    return aggregates


class UsageRecorder:
    """
    Persistent usage recorder with batch processing support.

    Features:
    - Buffered writes for high throughput
    - Automatic cost calculation from model_pricing table
    - Daily aggregate updates
    - 30-day retention policy support
    """

    def __init__(
        self,
        database: DatabaseStorage | None = None,
        buffer_size: int = 100,
        flush_interval_seconds: float = 5.0,
        normal_trace_sample_rate: float = 0.03,
        trace_p95_cache_ttl_seconds: float = 60.0,
        default_trace_p95_threshold_ms: int = 10000,
    ):
        self.database = database
        self.buffer_size = buffer_size
        self.flush_interval = flush_interval_seconds
        self._normal_trace_sample_rate = max(0.0, min(float(normal_trace_sample_rate), 1.0))
        self._trace_p95_cache_ttl_seconds = max(float(trace_p95_cache_ttl_seconds), 1.0)
        self._default_trace_p95_threshold_ms = max(int(default_trace_p95_threshold_ms), 1)
        self._buffer: list[UsageRecord] = []
        self._buffer_lock = asyncio.Lock()
        self._flush_task: asyncio.Task | None = None
        self._running = False
        self._pricing_cache: dict[str, dict[str, Any]] = {}
        self._pricing_cache_time: float = 0
        self._pricing_cache_ttl: float = 300  # 5 minutes
        self._trace_p95_cache: dict[str, tuple[int, float]] = {}
        self._flushed_ids: set[str] = set()
        self._flushed_identities: set[tuple[str, str, str, str]] = set()
        self._flushed_observations: dict[
            tuple[str, str, str, str], UsageRecord
        ] = {}
        self._flushed_ids_max: int = 5000

        # ── Phase 6 dual-write event bus ────────────────────────────────
        # When ``EVENT_BUS_REDIS_URL`` is set, every ``record()`` ALSO
        # publishes a ``UsageRecordedV1`` event to the Redis Streams bus.
        # DB path stays the source of truth; bus publish is fire-and-forget.
        # Env is captured once at construction so the per-request hot path
        # never re-reads ``os.environ``.
        self._event_bus_url: str = os.environ.get("EVENT_BUS_REDIS_URL", "").strip()
        self._event_bus: Any = None  # ai_gateway_core.events.EventBus | None
        self._event_bus_failed: bool = not self._event_bus_url  # short-circuit when env unset

    def set_database(self, database: DatabaseStorage) -> None:
        """Set or update the database storage instance."""
        self.database = database

    @staticmethod
    def _normalize_identity(value: str | None, fallback: str) -> str:
        normalized = str(value or "").strip()
        if normalized.lower() in {"unknown", "none", "null", "n/a", "na"}:
            return fallback
        return normalized or fallback

    @staticmethod
    def _extract_provider_from_metadata(metadata: dict[str, Any]) -> str:
        for key in ("provider", "provider_id", "vendor", "llm_provider"):
            value = metadata.get(key)
            if value is None:
                continue
            normalized = str(value).strip()
            if normalized:
                return normalized
        return ""

    @staticmethod
    def _extract_model_from_metadata(metadata: dict[str, Any]) -> str:
        for key in ("model", "model_id", "llm_model"):
            value = metadata.get(key)
            if value is None:
                continue
            normalized = str(value).strip()
            if normalized:
                return normalized
        return ""

    # Well-known model name prefix -> provider mappings for fallback inference
    # Sorted longest-first so "mixtral" matches before "mistral"
    _MODEL_PREFIX_PROVIDER_MAP: list[tuple[str, str]] = [
        ("deepseek", "deepseek"),
        ("mixtral", "mistral"),
        ("mistral", "mistral"),
        ("gemini", "google"),
        ("claude", "anthropic"),
        ("command", "cohere"),
        ("llama", "meta"),
        ("qwen", "dashscope"),
        ("gpt", "openai"),
    ]

    async def _get_provider_for_model(self, model: str, tenant_id: str | None = None) -> str:
        normalized_model = str(model or "").strip()
        if not normalized_model:
            return ""
        # 1. Try exact match from model_pricing table
        pricing = await self._get_model_pricing(normalized_model, tenant_id=tenant_id)
        if pricing:
            provider = pricing.get("provider")
            if provider:
                return str(provider).strip()
        # 2. Fallback: infer from model name prefix (case-insensitive)
        model_lower = normalized_model.lower()
        for prefix, provider in self._MODEL_PREFIX_PROVIDER_MAP:
            if model_lower.startswith(prefix):
                return provider
        return ""

    async def _normalize_record_dimensions(self, record: UsageRecord) -> None:
        metadata = record.metadata if isinstance(record.metadata, dict) else {}

        record.model = self._normalize_identity(
            record.model or self._extract_model_from_metadata(metadata),
            UNATTRIBUTED_MODEL,
        )
        record.service_id = self._normalize_identity(record.service_id, UNATTRIBUTED_SERVICE)
        record.tenant_id = self._normalize_identity(record.tenant_id, "default")
        record.user_id = self._normalize_identity(record.user_id, "anonymous")

        provider = self._normalize_identity(
            record.provider or self._extract_provider_from_metadata(metadata),
            "",
        )
        if not provider:
            provider = await self._get_provider_for_model(record.model, tenant_id=record.tenant_id)

        record.provider = provider or UNATTRIBUTED_PROVIDER
        metadata.setdefault("provider", record.provider)
        metadata.setdefault("model", record.model)
        if record.provider == UNATTRIBUTED_PROVIDER and isinstance(metadata, dict):
            metadata.setdefault("attribution_issue", "missing_provider")
            record.metadata = metadata

        if "token_source" not in metadata:
            if record.input_tokens > 0 or record.output_tokens > 0:
                metadata["token_source"] = "upstream"
            elif (record.status or "").lower() != "success":
                metadata["token_source"] = "zero_on_failure"
            else:
                metadata["token_source"] = "estimated"
            record.metadata = metadata

        if record.request_total_duration_ms <= 0 and record.latency_ms > 0:
            record.request_total_duration_ms = int(record.latency_ms)

        if record.error_type:
            normalized_error = str(record.error_type).strip().lower()
            record.error_type = (
                normalized_error if normalized_error in KNOWN_ERROR_TYPES else "unknown"
            )

    @staticmethod
    def _request_identity(record: UsageRecord) -> tuple[str, str, str, str]:
        return (
            str(record.tenant_id or "default"),
            str(record.request_id or ""),
            str(record.service_id or ""),
            str(record.request_type or ""),
        )

    @staticmethod
    def _merge_idempotent_record(existing: UsageRecord, incoming: UsageRecord) -> None:
        """Merge duplicate request identities without changing billing dimensions."""
        existing.input_tokens = max(existing.input_tokens, incoming.input_tokens)
        existing.output_tokens = max(existing.output_tokens, incoming.output_tokens)
        existing.input_cost_cents = max(
            existing.input_cost_cents, incoming.input_cost_cents
        )
        existing.output_cost_cents = max(
            existing.output_cost_cents, incoming.output_cost_cents
        )
        original_pricing = existing.metadata.get("pricing_snapshot")
        existing.metadata = {
            **(existing.metadata if isinstance(existing.metadata, dict) else {}),
            **(incoming.metadata if isinstance(incoming.metadata, dict) else {}),
        }
        if original_pricing is not None:
            existing.metadata["pricing_snapshot"] = original_pricing
        if existing.status in {"running", "pending"} and incoming.status == "success":
            existing.status = "success"
            existing.error_type = incoming.error_type
        if not existing.trace_steps and incoming.trace_steps:
            existing.trace_steps = incoming.trace_steps
        existing.latency_ms = max(existing.latency_ms, incoming.latency_ms)
        existing.request_total_duration_ms = max(
            existing.request_total_duration_ms,
            incoming.request_total_duration_ms,
        )
        existing.first_token_ms = max(existing.first_token_ms, incoming.first_token_ms)
        existing.llm_inference_duration_ms = max(
            existing.llm_inference_duration_ms,
            incoming.llm_inference_duration_ms,
        )
        existing.retrieval_duration_ms = max(
            existing.retrieval_duration_ms, incoming.retrieval_duration_ms
        )
        existing.tool_call_duration_ms = max(
            existing.tool_call_duration_ms, incoming.tool_call_duration_ms
        )
        existing.agent_or_graph_overhead_ms = max(
            existing.agent_or_graph_overhead_ms,
            incoming.agent_or_graph_overhead_ms,
        )

    def _observation_advances(self, record: UsageRecord) -> bool:
        previous = self._flushed_observations.get(self._request_identity(record))
        if previous is None:
            return True
        if previous.status in {"running", "pending"} and record.status == "success":
            return True
        return any(
            incoming > current
            for incoming, current in (
                (record.input_tokens, previous.input_tokens),
                (record.output_tokens, previous.output_tokens),
                (record.input_cost_cents, previous.input_cost_cents),
                (record.output_cost_cents, previous.output_cost_cents),
            )
        )

    @staticmethod
    def _snapshot_record(record: UsageRecord) -> UsageRecord:
        return replace(
            record,
            metadata=dict(record.metadata or {}),
            trace_steps=list(record.trace_steps or []),
            tool_call_breakdown=dict(record.tool_call_breakdown or {}),
        )

    def _remember_observation(self, record: UsageRecord) -> UsageRecord | None:
        identity = self._usage_identity(record)
        if identity is None:
            return None
        previous = self._flushed_observations.get(identity)
        if previous is None:
            self._flushed_observations[identity] = self._snapshot_record(record)
            return None
        merged = self._snapshot_record(previous)
        self._merge_idempotent_record(merged, record)
        self._flushed_observations[identity] = merged
        return previous

    @staticmethod
    def _accounting_adjustment(
        record: UsageRecord,
        previous: UsageRecord,
    ) -> UsageRecord | None:
        input_delta = max(record.input_tokens - previous.input_tokens, 0)
        output_delta = max(record.output_tokens - previous.output_tokens, 0)
        input_cost_delta = max(
            record.input_cost_cents - previous.input_cost_cents, 0
        )
        output_cost_delta = max(
            record.output_cost_cents - previous.output_cost_cents, 0
        )
        success_delta = int(
            previous.status in {"running", "pending"} and record.status == "success"
        )
        if not any(
            (
                input_delta,
                output_delta,
                input_cost_delta,
                output_cost_delta,
                success_delta,
            )
        ):
            return None
        return replace(
            record,
            input_tokens=input_delta,
            output_tokens=output_delta,
            input_cost_cents=input_cost_delta,
            output_cost_cents=output_cost_delta,
            latency_ms=0,
            first_token_ms=0,
            request_total_duration_ms=0,
            llm_inference_duration_ms=0,
            retrieval_duration_ms=0,
            tool_call_duration_ms=0,
            agent_or_graph_overhead_ms=0,
            trace_steps=[],
            metadata={
                **(record.metadata or {}),
                "_accounting_adjustment": True,
                "_accounting_request_delta": 0,
                "_accounting_success_delta": success_delta,
                "_accounting_error_delta": 0,
            },
        )

    def _coalesce_idempotent_records(self, records: list[UsageRecord]) -> list[UsageRecord]:
        coalesced: dict[tuple[str, str, str, str], UsageRecord] = {}
        ordered: list[UsageRecord] = []
        for record in records:
            identity = self._request_identity(record)
            existing = coalesced.get(identity)
            if existing is None:
                coalesced[identity] = record
                ordered.append(record)
                continue
            self._merge_idempotent_record(existing, record)
        return ordered

    async def start(self) -> None:
        """Start the background flush task."""
        if self._running:
            return
        self._running = True
        self._flush_task = asyncio.create_task(self._periodic_flush())
        logger.info("UsageRecorder started")

    async def stop(self) -> None:
        """Stop the background flush task and flush remaining buffer."""
        self._running = False
        if self._flush_task:
            self._flush_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._flush_task
        # Flush remaining records
        await self._flush_buffer()
        logger.info("UsageRecorder stopped")

    async def record(self, record: UsageRecord) -> None:
        """
        Record a usage event.

        The record will be buffered and written to the database
        in batches for efficiency.

        Side-effect: when ``EVENT_BUS_REDIS_URL`` is set, the record is
        ALSO published to the Redis Streams event bus as a
        ``UsageRecordedV1`` event. The bus publish is fire-and-forget;
        it never raises, never delays the DB path. See
        ``_maybe_publish_usage_event``.
        """
        await self._normalize_record_dimensions(record)
        # Calculate cost
        await self._calculate_cost(record)

        async with self._buffer_lock:
            self._buffer.append(record)

            # Flush if buffer is full
            if len(self._buffer) >= self.buffer_size:
                await self._flush_buffer_locked()

        # Dual-write to event bus AFTER the DB buffer append so that even
        # if the bus is wedged we never lose the billing record.
        await self._maybe_publish_usage_event(record)

    async def _maybe_publish_usage_event(self, record: UsageRecord) -> None:
        """Best-effort fire-and-forget publish to the event bus.

        - No-op when ``EVENT_BUS_REDIS_URL`` is unset (default).
        - No-op when a previous bus init / publish raised — the failure
          flag latches so we don't spam the logs on every record.
        - Any exception is caught and logged at WARNING; the request
          path is unaffected.
        """
        # Fast-path: this combines "env unset" and "previous failure" into
        # one boolean read. Set in __init__ to True when no env, set on
        # first failure thereafter — request hot path is one attr read.
        if self._event_bus_failed:
            return

        try:
            if self._event_bus is None:
                from ai_gateway_core.events.bus import EventBus

                self._event_bus = EventBus(redis_url=self._event_bus_url)

            from ai_gateway_contracts.event_envelope import EventEnvelope, UsageRecordedV1

            envelope = EventEnvelope[UsageRecordedV1](
                event_type=UsageRecordedV1.EVENT_TYPE,
                producer="usage-recorder",
                tenant_id=record.tenant_id or "",
                request_id=record.request_id or "",
                payload=UsageRecordedV1(
                    tenant_id=record.tenant_id,
                    user_id=record.user_id,
                    model=record.model,
                    provider=record.provider or "",
                    service_id=record.service_id or "",
                    assistant_id=record.assistant_id or "",
                    input_tokens=int(record.input_tokens or 0),
                    output_tokens=int(record.output_tokens or 0),
                    latency_ms=int(record.latency_ms or 0),
                    first_token_ms=int(record.first_token_ms or 0),
                    request_total_duration_ms=int(record.request_total_duration_ms or 0),
                    error_type=record.error_type,
                    status=record.status or "success",
                    request_type=record.request_type or "chat",
                    timestamp=float(record.timestamp or time.time()),
                    metadata=dict(record.metadata or {}),
                ),
            )
            await self._event_bus.publish(envelope)
        except Exception as exc:  # noqa: BLE001 — bus must never fail the request
            logger.warning(
                "usage event publish skipped (latched): %s: %s",
                type(exc).__name__,
                exc,
            )
            self._event_bus_failed = True

    async def record_usage(
        self,
        tenant_id: str,
        user_id: str,
        model: str,
        input_tokens: int,
        output_tokens: int,
        request_id: str = "",
        service_id: str = "",
        assistant_id: str = "",
        provider: str = "",
        latency_ms: int = 0,
        first_token_ms: int = 0,
        request_total_duration_ms: int = 0,
        llm_inference_duration_ms: int = 0,
        retrieval_duration_ms: int = 0,
        tool_call_duration_ms: int = 0,
        agent_or_graph_overhead_ms: int = 0,
        tool_call_breakdown: dict[str, int] | None = None,
        error_type: str | None = None,
        status: str = "success",
        request_type: str = "chat",
        metadata: dict[str, Any] | None = None,
        trace_steps: list[dict[str, Any]] | None = None,
    ) -> None:
        """
        Convenience method to record usage without creating UsageRecord manually.
        """
        record = UsageRecord(
            tenant_id=tenant_id,
            user_id=user_id,
            model=model,
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            request_id=request_id,
            service_id=service_id,
            assistant_id=assistant_id,
            provider=provider,
            latency_ms=latency_ms,
            first_token_ms=first_token_ms,
            request_total_duration_ms=request_total_duration_ms,
            llm_inference_duration_ms=llm_inference_duration_ms,
            retrieval_duration_ms=retrieval_duration_ms,
            tool_call_duration_ms=tool_call_duration_ms,
            agent_or_graph_overhead_ms=agent_or_graph_overhead_ms,
            tool_call_breakdown=tool_call_breakdown or {},
            error_type=error_type,
            status=status,
            request_type=request_type,
            metadata=metadata or {},
            trace_steps=trace_steps or [],
        )
        await self.record(record)

    async def _calculate_cost(self, record: UsageRecord) -> None:
        """Calculate cost based on model pricing."""
        metadata = dict(record.metadata) if isinstance(record.metadata, dict) else {}
        snapshot = metadata.get("pricing_snapshot")
        identity = self._request_identity(record)
        prior = self._flushed_observations.get(identity)
        if prior is None and record.request_id:
            prior = next((item for item in self._buffer if self._request_identity(item) == identity), None)
        if prior is not None:
            snapshot = prior.metadata.get("pricing_snapshot") or snapshot
        if snapshot is not None:
            # Invalid receipts must not silently settle using a current price.
            receipt = validate_pricing_snapshot(
                snapshot, tenant_id=record.tenant_id, provider_id=record.provider, model_id=record.model,
            )
            pricing = {
                "input": receipt["input_price_per_1k"], "output": receipt["output_price_per_1k"],
                "provider": receipt["provider_id"], "pricing_status": receipt["pricing_status"],
            }
        else:
            pricing = await self._get_model_pricing(
                record.model, tenant_id=record.tenant_id, provider_id=record.provider,
            )
            receipt = build_pricing_snapshot(
                tenant_id=record.tenant_id, provider_id=record.provider, model_id=record.model,
                input_price_per_1k=(pricing or {}).get("input", 0),
                output_price_per_1k=(pricing or {}).get("output", 0),
                pricing_status=str((pricing or {}).get("pricing_status") or "unknown"),
            )
        metadata["pricing_snapshot"] = receipt
        record.metadata = metadata
        if pricing:
            input_price = Decimal(str(pricing.get("input", Decimal("0.001"))))
            output_price = Decimal(str(pricing.get("output", Decimal("0.002"))))

            input_cost_usd = (Decimal(int(record.input_tokens or 0)) / Decimal(1000)) * input_price
            output_cost_usd = (
                Decimal(int(record.output_tokens or 0)) / Decimal(1000)
            ) * output_price

            # Persist microcents in the legacy *_cost_cents columns to preserve
            # precision for low-cost models without using float arithmetic.
            record.input_cost_cents = usd_to_microcents(input_cost_usd)
            record.output_cost_cents = usd_to_microcents(output_cost_usd)

            metadata = record.metadata if isinstance(record.metadata, dict) else {}
            metadata.setdefault("provider", str(pricing.get("provider") or record.provider or ""))
            metadata.setdefault("model", record.model)
            metadata["pricing_status"] = str(pricing.get("pricing_status") or "unknown")
            record.metadata = metadata

    async def _get_model_pricing(
        self, model: str, *, tenant_id: str | None = None, provider_id: str | None = None,
    ) -> dict[str, Any] | None:
        """Get pricing for a model from cache or database."""
        if tenant_id is not None:
            if self.database and self.database._pool:
                async with self.database._pool.acquire() as conn:
                    rows = await conn.fetch(
                        """SELECT provider_id, input_price_per_1k, output_price_per_1k
                           FROM llm_models WHERE tenant_id = $1 AND model_id = $2
                           AND ($3::text IS NULL OR provider_id = $3) LIMIT 2""",
                        tenant_id, model,
                        None if provider_id in (None, "", UNATTRIBUTED_PROVIDER) else provider_id,
                    )
                if len(rows) > 1:
                    return {"input": 0, "output": 0, "provider": UNATTRIBUTED_PROVIDER,
                            "pricing_status": "ambiguous_provider"}
                if rows and "provider_id" in rows[0]:
                    row = rows[0]
                    return {
                        "input": row["input_price_per_1k"], "output": row["output_price_per_1k"],
                        "provider": row["provider_id"], "pricing_status": "tenant_model",
                    }
            # The global mutable table may contain historical tenant mirrors.
            # Only the immutable catalog is an explicit scoped-miss fallback.
            pricing, status = resolve_pricing_with_status(model)
            return {**pricing, "pricing_status": status}

        # Check cache
        now = time.time()
        if now - self._pricing_cache_time > self._pricing_cache_ttl:
            await self._refresh_pricing_cache()

        # Return cached pricing (case-insensitive exact key).
        normalized_model = model.lower()
        for cached_model, pricing in self._pricing_cache.items():
            if cached_model.lower() == normalized_model:
                return pricing

        # Try the most-specific partial match (for model variants such as
        # ``gpt-4o-mini-2024-07-18``). Only "requested starts with cached"
        # may match, matching resolve_pricing_with_status.
        candidates = [
            (len(cached_model), cached_model, pricing)
            for cached_model, pricing in self._pricing_cache.items()
            if normalized_model.startswith(cached_model.lower())
        ]
        if candidates:
            _length, _cached_model, pricing = max(candidates, key=lambda item: item[0])
            matched = dict(pricing)
            matched["pricing_status"] = "provider_model"
            return matched

        # Return resolved default pricing when model is unknown.
        pricing, pricing_status = resolve_pricing_with_status(model)
        resolved = dict(pricing)
        resolved["pricing_status"] = pricing_status
        return resolved

    async def _refresh_pricing_cache(self) -> None:
        """Refresh pricing cache from database."""
        if not self.database or not self.database._pool:
            # Use default pricing if no database
            self._pricing_cache = {
                model: {
                    "input": Decimal(str(pricing["input"])),
                    "output": Decimal(str(pricing["output"])),
                    "provider": str(pricing.get("provider") or UNATTRIBUTED_PROVIDER),
                    "pricing_status": "catalog" if model != "default" else "unknown",
                }
                for model, pricing in DEFAULT_TOKEN_PRICING_PER_1K_USD.items()
            }
            self._pricing_cache_time = time.time()
            return

        try:
            async with self.database._pool.acquire() as conn:
                rows = await conn.fetch(
                    """
                    SELECT model, provider, input_price_per_1k, output_price_per_1k
                    FROM model_pricing
                    WHERE is_active = TRUE
                    """
                )
                self._pricing_cache = {
                    row["model"]: {
                        "input": Decimal(str(row["input_price_per_1k"])),
                        "output": Decimal(str(row["output_price_per_1k"])),
                        "provider": str(row["provider"] or "").strip(),
                        "pricing_status": "catalog",
                    }
                    for row in rows
                }
                self._pricing_cache_time = time.time()
                logger.debug(f"Refreshed pricing cache with {len(rows)} models")
        except Exception as e:
            logger.warning(f"Failed to refresh pricing cache: {e}")

    async def _periodic_flush(self) -> None:
        """Background task to periodically flush the buffer."""
        while self._running:
            try:
                await asyncio.sleep(self.flush_interval)
                await self._flush_buffer()
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error(f"Error in periodic flush: {e}")

    async def _flush_buffer(self) -> None:
        """Flush buffer to database."""
        async with self._buffer_lock:
            await self._flush_buffer_locked()

    async def _flush_buffer_locked(self) -> None:
        """Flush buffer (must hold lock)."""
        if not self._buffer:
            return

        records = self._buffer.copy()
        self._buffer.clear()
        self._ensure_request_ids(records)
        records = self._coalesce_idempotent_records(records)

        # Exact retries are ignored, while cumulative partial -> final usage is
        # allowed through so the durable row and aggregate deltas can advance.
        records = [r for r in records if self._observation_advances(r)]
        if not records:
            return

        if not self.database or not self.database._pool:
            logger.warning(f"No database connection, dropping {len(records)} usage records")
            return

        accepted_records: list[UsageRecord] = []
        try:
            async with self.database._pool.acquire() as conn:
                async with conn.transaction():
                    accepted_records = await self._write_records(conn, records)

                if accepted_records:
                    await self._run_post_write_stage(
                        conn,
                        accepted_records,
                        "quota_counters",
                        self._update_quota_counters,
                    )
                    await self._run_post_write_stage(
                        conn,
                        accepted_records,
                        "request_traces",
                        self._write_sampled_traces,
                    )
                    await self._run_post_write_stage(
                        conn,
                        accepted_records,
                        "daily_aggregates",
                        self._update_daily_aggregates,
                    )
                    await self._run_post_write_stage(
                        conn,
                        accepted_records,
                        "hourly_aggregates",
                        self._update_hourly_aggregates,
                    )
            # Track flushed request_ids to prevent double-counting on retry
            for r in records:
                self._flushed_ids.add(r.request_id)
                self._flushed_identities.add(self._request_identity(r))
            if len(self._flushed_ids) > self._flushed_ids_max:
                # Evict: clear and keep only current batch ids
                self._flushed_ids = {r.request_id for r in records}
                self._flushed_identities = {self._request_identity(r) for r in records}
                self._flushed_observations = {
                    self._request_identity(r): self._snapshot_record(r)
                    for r in records
                }
            logger.debug(
                "Flushed %s usage records (%s accepted)",
                len(records),
                len(accepted_records),
            )
        except Exception as e:
            logger.error(f"Failed to flush usage records: {e}")
            # Re-add failed records to buffer (with limit)
            if len(self._buffer) + len(records) <= self.buffer_size * 2:
                self._buffer.extend(records)

    async def _run_post_write_stage(
        self,
        conn: Connection,
        records: list[UsageRecord],
        stage: str,
        func: Any,
    ) -> None:
        try:
            await func(conn, records)
        except Exception as exc:
            logger.error("Usage post-write stage failed (%s): %s", stage, exc)
            await self._record_reconciliation_event(conn, records, stage, exc)

    @staticmethod
    def _ensure_request_ids(records: list[UsageRecord]) -> None:
        for record in records:
            if not record.request_id:
                record.request_id = str(uuid.uuid4())

    async def _get_trace_p95_threshold_ms(self, conn: Connection, tenant_id: str) -> int:
        cache_key = tenant_id or "default"
        now = time.time()
        cached = self._trace_p95_cache.get(cache_key)
        if cached and now - cached[1] <= self._trace_p95_cache_ttl_seconds:
            return cached[0]

        threshold = self._default_trace_p95_threshold_ms
        try:
            value = await conn.fetchval(
                """
                SELECT p95_latency_ms
                FROM usage_daily_aggregates
                WHERE tenant_id = $1
                  AND p95_latency_ms > 0
                ORDER BY date DESC, p95_latency_ms DESC
                LIMIT 1
                """,
                tenant_id,
            )
            if value is not None:
                threshold = max(int(float(value)), 1)
        except Exception as e:
            logger.debug(f"Failed to load P95 threshold for tenant {tenant_id}: {e}")

        self._trace_p95_cache[cache_key] = (threshold, now)
        return threshold

    @staticmethod
    def _record_total_duration(record: UsageRecord) -> int:
        if record.request_total_duration_ms and record.request_total_duration_ms > 0:
            return int(record.request_total_duration_ms)
        if record.latency_ms and record.latency_ms > 0:
            return int(record.latency_ms)
        return 0

    async def _write_sampled_traces(self, conn: Connection, records: list[UsageRecord]) -> None:
        sampled_rows: list[tuple[Any, ...]] = []
        sampled_flags: list[tuple[str, str, str]] = []
        threshold_cache: dict[str, int] = {}

        for record in records:
            if (record.metadata or {}).get("_accounting_adjustment"):
                continue
            tenant_id = self._normalize_identity(record.tenant_id, "default")
            if tenant_id not in threshold_cache:
                threshold_cache[tenant_id] = await self._get_trace_p95_threshold_ms(conn, tenant_id)

            total_duration = self._record_total_duration(record)
            sampled, reason = should_sample_trace(
                status=record.status,
                request_total_duration_ms=total_duration,
                p95_threshold_ms=threshold_cache[tenant_id],
                normal_sample_rate=self._normal_trace_sample_rate,
            )
            if not sampled:
                continue

            trace_steps = record.trace_steps if isinstance(record.trace_steps, list) else []
            sampled_rows.append(
                (
                    record.request_id,
                    tenant_id,
                    self._normalize_identity(record.user_id, "anonymous"),
                    self._normalize_identity(record.service_id, UNATTRIBUTED_SERVICE),
                    self._normalize_identity(record.assistant_id, ""),
                    self._normalize_identity(record.provider, UNATTRIBUTED_PROVIDER),
                    self._normalize_identity(record.model, UNATTRIBUTED_MODEL),
                    record.status or "success",
                    record.error_type,
                    total_duration,
                    max(int(record.first_token_ms or 0), 0),
                    max(int(record.llm_inference_duration_ms or 0), 0),
                    max(int(record.retrieval_duration_ms or 0), 0),
                    max(int(record.tool_call_duration_ms or 0), 0),
                    max(int(record.agent_or_graph_overhead_ms or 0), 0),
                    max(int(record.input_tokens or 0), 0),
                    max(int(record.output_tokens or 0), 0),
                    max(int(record.input_cost_cents + record.output_cost_cents), 0),
                    reason,
                    json.dumps(trace_steps, ensure_ascii=False),
                    json.dumps(record.metadata or {}, ensure_ascii=False),
                    datetime.fromtimestamp(record.timestamp, tz=timezone.utc),
                )
            )
            sampled_flags.append((record.request_id, tenant_id, reason))

        if not sampled_rows:
            return

        await conn.executemany(
            """
            INSERT INTO request_traces (
                request_id, tenant_id, user_id, service_id, assistant_id, provider, model,
                status, error_type, request_total_duration_ms, first_token_latency_ms,
                llm_inference_duration_ms, retrieval_duration_ms, tool_call_duration_ms,
                agent_or_graph_overhead_ms, input_tokens, output_tokens, total_cost_cents,
                sample_reason, trace_steps, metadata, created_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7,
                $8, $9, $10, $11,
                $12, $13, $14,
                $15, $16, $17, $18,
                $19, $20::jsonb, $21::jsonb, $22
            )
            """,
            sampled_rows,
        )

        await conn.executemany(
            """
            UPDATE usage_records
            SET trace_sampled = TRUE, sample_reason = $3
            WHERE request_id = $1 AND tenant_id = $2
            """,
            sampled_flags,
        )

    async def _record_reconciliation_event(
        self,
        conn: Connection,
        records: list[UsageRecord],
        stage: str,
        exc: Exception,
    ) -> None:
        if not records:
            return
        first = records[0]
        try:
            await conn.execute(
                """
                INSERT INTO billing_events (
                    event_id, request_id, service_id, user_id, tenant_id,
                    input_tokens, output_tokens, total_tokens,
                    model, assistant_id, duration_ms, status,
                    cost_amount, cost_currency, metadata, event_time
                ) VALUES (
                    $1, $2, $3, $4, $5,
                    $6, $7, $8,
                    $9, $10, $11, $12,
                    $13, $14, $15::jsonb, $16
                )
                ON CONFLICT (event_id) DO NOTHING
                """,
                f"usage-reconcile:{stage}:{first.request_id}:{uuid.uuid4()}",
                first.request_id or "",
                first.service_id or UNATTRIBUTED_SERVICE,
                first.user_id or "anonymous",
                first.tenant_id or "default",
                max(int(first.input_tokens or 0), 0),
                max(int(first.output_tokens or 0), 0),
                max(int((first.input_tokens or 0) + (first.output_tokens or 0)), 0),
                first.model or UNATTRIBUTED_MODEL,
                first.assistant_id or "",
                self._record_total_duration(first),
                "reconciliation_required",
                None,
                "USD",
                json.dumps(
                    {
                        "event": "usage_accounting_reconciliation_required",
                        "stage": stage,
                        "error_type": type(exc).__name__,
                        "error": str(exc)[:512],
                        "record_count": len(records),
                    },
                    ensure_ascii=False,
                ),
                datetime.fromtimestamp(time.time(), tz=timezone.utc),
            )
        except Exception as event_exc:
            logger.error(
                "Failed to record usage reconciliation event (%s): %s",
                stage,
                event_exc,
            )

    async def _write_records(self, conn: Connection, records: list[UsageRecord]) -> list[UsageRecord]:
        """Write one flush with one PostgreSQL round trip.

        The production asyncpg connection exposes ``fetch``.  The small
        ``fetchrow`` fallback keeps compatibility with lightweight adapters,
        but the live path no longer performs one network round trip per row.
        Duplicate idempotency identities are collapsed before the INSERT so a
        single PostgreSQL statement cannot hit the ON CONFLICT cardinality
        restriction.
        """
        if records and hasattr(conn, "fetch"):
            return await self._write_records_batch(conn, records)
        return await self._write_records_compat(conn, records)

    @staticmethod
    def _usage_identity(record: UsageRecord) -> tuple[str, str, str, str] | None:
        request_id = str(record.request_id or "")
        if not request_id:
            return None
        return (
            str(record.tenant_id),
            request_id,
            str(record.service_id or ""),
            str(record.request_type or ""),
        )

    @staticmethod
    def _merge_usage_status(current: str, incoming: str) -> str:
        if current in {"running", "pending"} and incoming == "success":
            return incoming
        return current

    def _collapse_usage_records(
        self, records: list[UsageRecord]
    ) -> list[tuple[UsageRecord, UsageRecord]]:
        """Return ``(database row, accounting row)`` pairs.

        The database row carries the same status/error/metadata merge that
        sequential ON CONFLICT updates previously applied.  The accounting row
        remains the first inserted observation so quota totals do not drift.
        """
        collapsed: list[tuple[UsageRecord, UsageRecord]] = []
        positions: dict[tuple[str, str, str, str], int] = {}
        for record in records:
            identity = self._usage_identity(record)
            if identity is None or identity not in positions:
                if identity is not None:
                    positions[identity] = len(collapsed)
                collapsed.append((record, record))
                continue
            index = positions[identity]
            current, accounting = collapsed[index]
            collapsed[index] = (
                replace(
                    current,
                    input_tokens=max(current.input_tokens, record.input_tokens),
                    output_tokens=max(current.output_tokens, record.output_tokens),
                    input_cost_cents=max(
                        current.input_cost_cents, record.input_cost_cents
                    ),
                    output_cost_cents=max(
                        current.output_cost_cents, record.output_cost_cents
                    ),
                    status=self._merge_usage_status(current.status, record.status),
                    error_type=current.error_type or record.error_type,
                    metadata={**(current.metadata or {}), **(record.metadata or {})},
                ),
                replace(
                    accounting,
                    input_tokens=max(accounting.input_tokens, record.input_tokens),
                    output_tokens=max(accounting.output_tokens, record.output_tokens),
                    input_cost_cents=max(
                        accounting.input_cost_cents, record.input_cost_cents
                    ),
                    output_cost_cents=max(
                        accounting.output_cost_cents, record.output_cost_cents
                    ),
                    status=self._merge_usage_status(accounting.status, record.status),
                ),
            )
        return collapsed

    @staticmethod
    def _usage_record_values(record: UsageRecord) -> tuple[Any, ...]:
        return (
            record.tenant_id,
            record.user_id,
            record.request_id,
            record.service_id or None,
            record.assistant_id or None,
            record.model,
            record.provider or None,
            record.input_tokens,
            record.output_tokens,
            record.input_cost_cents,
            record.output_cost_cents,
            record.latency_ms,
            record.first_token_ms,
            record.request_total_duration_ms or record.latency_ms,
            record.llm_inference_duration_ms,
            record.retrieval_duration_ms,
            record.tool_call_duration_ms,
            record.agent_or_graph_overhead_ms,
            json.dumps(record.tool_call_breakdown or {}, ensure_ascii=False),
            record.error_type,
            record.status,
            record.request_type,
            json.dumps(record.metadata, ensure_ascii=False) if record.metadata else "{}",
            datetime.utcfromtimestamp(record.timestamp),
        )

    async def _write_records_batch(
        self, conn: Connection, records: list[UsageRecord]
    ) -> list[UsageRecord]:
        collapsed = self._collapse_usage_records(records)
        values_sql: list[str] = []
        arguments: list[Any] = []
        column_count = 24
        for database_record, _accounting_record in collapsed:
            offset = len(arguments)
            placeholders = [f"${offset + index}" for index in range(1, column_count + 1)]
            placeholders[18] += "::jsonb"
            placeholders[22] += "::jsonb"
            values_sql.append(f"({', '.join(placeholders)})")
            arguments.extend(self._usage_record_values(database_record))

        rows = await conn.fetch(
            f"""
            INSERT INTO usage_records (
                tenant_id, user_id, request_id,
                service_id, assistant_id, model, provider,
                input_tokens, output_tokens,
                input_cost_cents, output_cost_cents,
                latency_ms, first_token_ms,
                request_total_duration_ms,
                llm_inference_duration_ms, retrieval_duration_ms,
                tool_call_duration_ms, agent_or_graph_overhead_ms,
                tool_call_breakdown, error_type, status,
                request_type, metadata, created_at
            ) VALUES {', '.join(values_sql)}
            ON CONFLICT (
                tenant_id,
                (COALESCE(request_id, '')),
                (COALESCE(service_id, '')),
                (COALESCE(request_type, ''))
            )
            WHERE request_id IS NOT NULL AND request_id <> ''
            DO UPDATE SET
                -- SPO-05 / G1: partial SSE usage arrives before the final
                -- usage; the conflict branch takes the monotonic max so the
                -- final (larger) numbers always win.
                input_tokens = GREATEST(usage_records.input_tokens, EXCLUDED.input_tokens),
                output_tokens = GREATEST(usage_records.output_tokens, EXCLUDED.output_tokens),
                input_cost_cents = GREATEST(usage_records.input_cost_cents, EXCLUDED.input_cost_cents),
                output_cost_cents = GREATEST(usage_records.output_cost_cents, EXCLUDED.output_cost_cents),
                status = CASE
                    WHEN usage_records.status IN ('running', 'pending')
                         AND EXCLUDED.status = 'success'
                    THEN EXCLUDED.status
                    ELSE usage_records.status
                END,
                error_type = COALESCE(usage_records.error_type, EXCLUDED.error_type),
                metadata = COALESCE(usage_records.metadata, '{{}}'::jsonb)
                    || COALESCE(EXCLUDED.metadata, '{{}}'::jsonb)
            RETURNING
                (xmax = 0) AS inserted,
                tenant_id,
                COALESCE(request_id, '') AS request_id_key,
                COALESCE(service_id, '') AS service_id_key,
                COALESCE(request_type, '') AS request_type_key
            """,
            *arguments,
        )

        inserted_identities = {
            (
                str(row["tenant_id"]),
                str(row["request_id_key"]),
                str(row["service_id_key"]),
                str(row["request_type_key"]),
            )
            for row in rows
            if bool(row["inserted"]) and str(row["request_id_key"])
        }
        accepted: list[UsageRecord] = []
        for database_record, accounting_record in collapsed:
            identity = self._usage_identity(database_record)
            previous = self._remember_observation(database_record)
            if identity is None or identity in inserted_identities:
                accepted.append(accounting_record)
            elif previous is not None:
                adjustment = self._accounting_adjustment(database_record, previous)
                if adjustment is not None:
                    accepted.append(adjustment)
        return accepted

    async def _write_records_compat(
        self, conn: Connection, records: list[UsageRecord]
    ) -> list[UsageRecord]:
        """Compatibility path for connection adapters without ``fetch``."""
        accepted: list[UsageRecord] = []
        for r in records:
            row = await conn.fetchrow(
                """
            INSERT INTO usage_records (
                tenant_id, user_id, request_id,
                service_id, assistant_id, model, provider,
                input_tokens, output_tokens,
                input_cost_cents, output_cost_cents,
                latency_ms, first_token_ms,
                request_total_duration_ms,
                llm_inference_duration_ms, retrieval_duration_ms,
                tool_call_duration_ms, agent_or_graph_overhead_ms,
                tool_call_breakdown, error_type, status,
                request_type, metadata, created_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7,
                $8, $9,
                $10, $11,
                $12, $13,
                $14,
                $15, $16,
                $17, $18,
                $19::jsonb, $20, $21,
                $22, $23::jsonb, $24
            )
            ON CONFLICT (
                tenant_id,
                (COALESCE(request_id, '')),
                (COALESCE(service_id, '')),
                (COALESCE(request_type, ''))
            )
            WHERE request_id IS NOT NULL AND request_id <> ''
            DO UPDATE SET
                -- SPO-05 / G1: monotonic max for partial-then-final SSE usage.
                input_tokens = GREATEST(usage_records.input_tokens, EXCLUDED.input_tokens),
                output_tokens = GREATEST(usage_records.output_tokens, EXCLUDED.output_tokens),
                input_cost_cents = GREATEST(usage_records.input_cost_cents, EXCLUDED.input_cost_cents),
                output_cost_cents = GREATEST(usage_records.output_cost_cents, EXCLUDED.output_cost_cents),
                status = CASE
                    WHEN usage_records.status IN ('running', 'pending')
                         AND EXCLUDED.status = 'success'
                    THEN EXCLUDED.status
                    ELSE usage_records.status
                END,
                error_type = COALESCE(usage_records.error_type, EXCLUDED.error_type),
                metadata = COALESCE(usage_records.metadata, '{}'::jsonb)
                    || COALESCE(EXCLUDED.metadata, '{}'::jsonb)
            RETURNING (xmax = 0) AS inserted
                """,
                *self._usage_record_values(r),
            )
            inserted = False
            if row:
                try:
                    inserted = bool(row.get("inserted"))  # type: ignore[attr-defined]
                except AttributeError:
                    inserted = bool(row["inserted"])
            if inserted:
                accepted.append(r)
                self._remember_observation(r)
            else:
                previous = self._remember_observation(r)
                if previous is not None:
                    adjustment = self._accounting_adjustment(r, previous)
                    if adjustment is not None:
                        accepted.append(adjustment)
        return accepted

    async def _update_quota_counters(self, conn: Connection, records: list[UsageRecord]) -> None:
        """Update quota counters from flushed usage records."""
        aggregates: dict[tuple[str, str], dict[str, int]] = {}
        for record in records:
            key = (record.tenant_id, record.user_id)
            current = aggregates.setdefault(
                key,
                {
                    "tokens": 0,
                    "cost_microcents": 0,
                    "requests": 0,
                },
            )
            current["tokens"] += max(int(record.input_tokens + record.output_tokens), 0)
            current["cost_microcents"] += max(
                int(record.input_cost_cents + record.output_cost_cents), 0
            )
            current["requests"] += int(
                (record.metadata or {}).get("_accounting_request_delta", 1)
            )

        if not aggregates:
            return

        await conn.executemany(
            """
            UPDATE user_quotas
            SET
                current_daily_tokens = current_daily_tokens + $3,
                current_monthly_tokens = current_monthly_tokens + $3,
                current_monthly_cost_cents = current_monthly_cost_cents + $4,
                current_daily_requests = current_daily_requests + $5,
                updated_at = CURRENT_TIMESTAMP
            WHERE tenant_id = $1 AND user_id = $2
            """,
            [
                (
                    tenant_id,
                    user_id,
                    agg["tokens"],
                    int(round(agg["cost_microcents"] / 10000)),
                    agg["requests"],
                )
                for (tenant_id, user_id), agg in aggregates.items()
            ],
        )

    async def _update_daily_aggregates(self, conn: Connection, records: list[UsageRecord]) -> None:
        """Update daily aggregates table."""
        # Group records by aggregation key
        aggregates: dict[tuple, dict[str, Any]] = {}

        for record in records:
            # 修复：使用记录的 timestamp 而非 date.today()，避免跨日 flush 导致数据记录到错误日期
            record_date = datetime.fromtimestamp(record.timestamp, tz=timezone.utc).date()
            key = (
                record.tenant_id,
                record.user_id or "",
                record.model or "",
                record.assistant_id or "",
                record.service_id or "",
                record_date,
            )

            if key not in aggregates:
                aggregates[key] = {
                    "request_count": 0,
                    "success_count": 0,
                    "error_count": 0,
                    "total_input_tokens": 0,
                    "total_output_tokens": 0,
                    "total_cost_cents": 0,
                    "latency_sum": 0,
                    "first_token_sum": 0,
                }

            agg = aggregates[key]
            metadata = record.metadata or {}
            request_delta = int(metadata.get("_accounting_request_delta", 1))
            agg["request_count"] += request_delta
            if "_accounting_success_delta" in metadata:
                agg["success_count"] += int(metadata["_accounting_success_delta"])
                agg["error_count"] += int(metadata.get("_accounting_error_delta", 0))
            elif record.status == "success":
                agg["success_count"] += 1
            elif record.status not in {"running", "pending"}:
                agg["error_count"] += 1
            agg["total_input_tokens"] += record.input_tokens
            agg["total_output_tokens"] += record.output_tokens
            agg["total_cost_cents"] += record.input_cost_cents + record.output_cost_cents
            if request_delta:
                agg["latency_sum"] += self._record_total_duration(record)
                agg["first_token_sum"] += record.first_token_ms

        for key, agg in aggregates.items():
            tenant_id, user_id, model, assistant_id, service_id, agg_date = key
            request_count = agg["request_count"]
            avg_latency = agg["latency_sum"] // request_count if request_count > 0 else 0
            avg_first_token = agg["first_token_sum"] // request_count if request_count > 0 else 0

            updated_id = await conn.fetchval(
                """
                UPDATE usage_daily_aggregates
                SET
                    request_count = request_count + $7,
                    success_count = success_count + $8,
                    error_count = error_count + $9,
                    total_input_tokens = total_input_tokens + $10,
                    total_output_tokens = total_output_tokens + $11,
                    total_cost_cents = total_cost_cents + $12,
                    avg_latency_ms = CASE WHEN request_count + $7 > 0 THEN (
                        (avg_latency_ms * request_count + $13 * $7)
                        / (request_count + $7)
                    )::integer ELSE 0 END,
                    avg_first_token_ms = CASE WHEN request_count + $7 > 0 THEN (
                        (avg_first_token_ms * request_count + $14 * $7)
                        / (request_count + $7)
                    )::integer ELSE 0 END,
                    updated_at = CURRENT_TIMESTAMP
                WHERE tenant_id = $1
                  AND COALESCE(user_id, '') = $2
                  AND COALESCE(model, '') = $3
                  AND COALESCE(assistant_id, '') = $4
                  AND COALESCE(service_id, '') = $5
                  AND date = $6
                RETURNING id
                """,
                tenant_id,
                user_id or "",
                model or "",
                assistant_id or "",
                service_id or "",
                agg_date,
                request_count,
                agg["success_count"],
                agg["error_count"],
                agg["total_input_tokens"],
                agg["total_output_tokens"],
                agg["total_cost_cents"],
                avg_latency,
                avg_first_token,
            )
            if updated_id:
                continue

            await conn.execute(
                """
                INSERT INTO usage_daily_aggregates (
                    tenant_id, user_id, model, assistant_id, service_id, date,
                    request_count, success_count, error_count,
                    total_input_tokens, total_output_tokens, total_cost_cents,
                    avg_latency_ms, avg_first_token_ms
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14
                )
                """,
                tenant_id,
                user_id or "",
                model or "",
                assistant_id or "",
                service_id or "",
                agg_date,
                request_count,
                agg["success_count"],
                agg["error_count"],
                agg["total_input_tokens"],
                agg["total_output_tokens"],
                agg["total_cost_cents"],
                avg_latency,
                avg_first_token,
            )

    async def _update_hourly_aggregates(self, conn: Connection, records: list[UsageRecord]) -> None:
        """Update hourly aggregates table."""
        aggregates = group_records_by_hour(records)

        for key, agg in aggregates.items():
            tenant_id, user_id, model, assistant_id, service_id, bucket_start = key
            bucket_date = bucket_start.date()
            request_count = agg["request_count"]
            avg_latency = agg["latency_sum"] // request_count if request_count > 0 else 0
            avg_first_token = agg["first_token_sum"] // request_count if request_count > 0 else 0

            updated_id = await conn.fetchval(
                """
                UPDATE usage_hourly_aggregates
                SET
                    request_count = request_count + $8,
                    success_count = success_count + $9,
                    error_count = error_count + $10,
                    total_input_tokens = total_input_tokens + $11,
                    total_output_tokens = total_output_tokens + $12,
                    total_cost_cents = total_cost_cents + $13,
                    avg_latency_ms = (
                        (avg_latency_ms * request_count + $14 * $8) /
                        NULLIF(request_count + $8, 0)
                    )::integer,
                    avg_first_token_ms = (
                        (avg_first_token_ms * request_count + $15 * $8) /
                        NULLIF(request_count + $8, 0)
                    )::integer,
                    updated_at = CURRENT_TIMESTAMP
                WHERE tenant_id = $1
                  AND COALESCE(user_id, '') = $2
                  AND COALESCE(model, '') = $3
                  AND COALESCE(assistant_id, '') = $4
                  AND COALESCE(service_id, '') = $5
                  AND bucket_start = $6
                  AND date = $7
                RETURNING id
                """,
                tenant_id,
                user_id or "",
                model or "",
                assistant_id or "",
                service_id or "",
                bucket_start,
                bucket_date,
                request_count,
                agg["success_count"],
                agg["error_count"],
                agg["total_input_tokens"],
                agg["total_output_tokens"],
                agg["total_cost_cents"],
                avg_latency,
                avg_first_token,
            )
            if updated_id:
                continue

            await conn.execute(
                """
                INSERT INTO usage_hourly_aggregates (
                    tenant_id, user_id, model, assistant_id, service_id,
                    bucket_start, date,
                    request_count, success_count, error_count,
                    total_input_tokens, total_output_tokens, total_cost_cents,
                    avg_latency_ms, avg_first_token_ms
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15
                )
                """,
                tenant_id,
                user_id or "",
                model or "",
                assistant_id or "",
                service_id or "",
                bucket_start,
                bucket_date,
                request_count,
                agg["success_count"],
                agg["error_count"],
                agg["total_input_tokens"],
                agg["total_output_tokens"],
                agg["total_cost_cents"],
                avg_latency,
                avg_first_token,
            )

    # ============ Query Methods ============

    async def get_usage_summary(
        self,
        tenant_id: str,
        start_date: date | None = None,
        end_date: date | None = None,
        user_id: str | None = None,
        model: str | None = None,
        service_id: str | None = None,
        assistant_id: str | None = None,
        provider: str | None = None,
    ) -> dict[str, Any]:
        """Get aggregated usage summary."""
        if not self.database or not self.database._pool:
            return self._empty_summary(data_status="unavailable")

        if not start_date:
            start_date = date.today()
        if not end_date:
            end_date = date.today()

        try:
            async with self.database._pool.acquire() as conn:
                # When provider filter is present we use raw usage_records to avoid
                # lossy provider inference from model_pricing joins.
                if provider:
                    # SPO-05 / D3: half-open interval on created_at keeps the
                    # index usable instead of casting every row with ::date.
                    conditions = [
                        "tenant_id = $1",
                        "created_at >= $2::date",
                        "created_at < ($3::date + interval '1 day')",
                        "provider = $4",
                    ]
                    params: list[Any] = [tenant_id, start_date, end_date, provider]
                    if user_id:
                        conditions.append(f"user_id = ${len(params) + 1}")
                        params.append(user_id)
                    if model:
                        conditions.append(f"model = ${len(params) + 1}")
                        params.append(model)
                    if service_id:
                        conditions.append(f"service_id = ${len(params) + 1}")
                        params.append(service_id)
                    if assistant_id:
                        conditions.append(f"assistant_id = ${len(params) + 1}")
                        params.append(assistant_id)

                    query = f"""
                        SELECT
                            COUNT(*) as total_requests,
                            COUNT(*) FILTER (WHERE status = 'success') as success_count,
                            COUNT(*) FILTER (WHERE status != 'success') as error_count,
                            COALESCE(SUM(input_tokens), 0) as total_input_tokens,
                            COALESCE(SUM(output_tokens), 0) as total_output_tokens,
                            COALESCE(SUM(total_cost_cents), 0) as total_cost_cents,
                            COALESCE(AVG(COALESCE(NULLIF(request_total_duration_ms, 0), latency_ms, 0)), 0) as avg_latency_ms
                        FROM usage_records
                        WHERE {" AND ".join(conditions)}
                    """
                    row = await conn.fetchrow(query, *params)
                else:
                    # Daily aggregates are more efficient for non-provider filtered views.
                    conditions = ["u.tenant_id = $1", "u.date >= $2", "u.date <= $3"]
                    params = [tenant_id, start_date, end_date]
                    if user_id:
                        conditions.append(f"u.user_id = ${len(params) + 1}")
                        params.append(user_id)
                    if model:
                        conditions.append(f"u.model = ${len(params) + 1}")
                        params.append(model)
                    if service_id:
                        conditions.append(f"u.service_id = ${len(params) + 1}")
                        params.append(service_id)
                    if assistant_id:
                        conditions.append(f"u.assistant_id = ${len(params) + 1}")
                        params.append(assistant_id)

                    query = f"""
                        SELECT
                            COALESCE(SUM(request_count), 0) as total_requests,
                            COALESCE(SUM(success_count), 0) as success_count,
                            COALESCE(SUM(error_count), 0) as error_count,
                            COALESCE(SUM(total_input_tokens), 0) as total_input_tokens,
                            COALESCE(SUM(total_output_tokens), 0) as total_output_tokens,
                            COALESCE(SUM(total_cost_cents), 0) as total_cost_cents,
                            COALESCE(AVG(avg_latency_ms), 0) as avg_latency_ms
                        FROM usage_daily_aggregates u
                        WHERE {" AND ".join(conditions)}
                    """
                    row = await conn.fetchrow(query, *params)

                if not row:
                    return self._empty_summary()

                total_requests = int(row["total_requests"])
                success_count = int(row["success_count"])

                return {
                    "total_requests": total_requests,
                    "success_rate": round(success_count / total_requests * 100, 2)
                    if total_requests > 0
                    else None,
                    "total_input_tokens": int(row["total_input_tokens"]),
                    "total_output_tokens": int(row["total_output_tokens"]),
                    "total_tokens": int(row["total_input_tokens"])
                    + int(row["total_output_tokens"]),
                    # cost_cents is stored as microcents (cents * 10000) for precision
                    # Convert: microcents / 10000 = cents, cents / 100 = USD
                    "total_cost_usd": round(int(row["total_cost_cents"]) / 1000000, 6),
                    "avg_latency_ms": int(row["avg_latency_ms"]),
                    "start_date": start_date.isoformat(),
                    "end_date": end_date.isoformat(),
                }

        except Exception as e:
            logger.error(f"Failed to get usage summary: {e}")
            return self._empty_summary(data_status="collection_error")

    async def get_usage_breakdown(
        self,
        tenant_id: str,
        dimension: str,  # model, user, assistant, service
        start_date: date | None = None,
        end_date: date | None = None,
        user_id: str | None = None,
        service_id: str | None = None,
        limit: int = 20,
    ) -> list[dict[str, Any]]:
        """Get usage breakdown by dimension."""
        if not self.database or not self.database._pool:
            return []

        if not start_date:
            start_date = date.today()
        if not end_date:
            end_date = date.today()

        # Whitelist of valid dimension -> column mappings (SQL injection prevention)
        dimension_mapping = {
            "model": "u.model",
            "user": "u.user_id",
            "assistant": "u.assistant_id",
            "service": "u.service_id",
            "provider": "provider",  # Special handling via usage_records
        }
        if dimension not in dimension_mapping:
            logger.warning(f"Invalid dimension requested: {dimension}")
            return []
        dimension_column = dimension_mapping[dimension]

        try:
            async with self.database._pool.acquire() as conn:
                # Provider breakdown uses raw usage_records to avoid unknown join artifacts.
                if dimension == "provider":
                    query = """
                        SELECT
                            COALESCE(NULLIF(provider, ''), $4) as dimension_value,
                            COALESCE(NULLIF(provider, ''), $4) as dimension_label,
                            COUNT(*) as total_requests,
                            COALESCE(SUM(input_tokens), 0) as total_input_tokens,
                            COALESCE(SUM(output_tokens), 0) as total_output_tokens,
                            COALESCE(SUM(total_cost_cents), 0) as total_cost_cents
                        FROM usage_records
                        WHERE tenant_id = $1
                          AND created_at::date >= $2
                          AND created_at::date <= $3
                    """
                    params: list[Any] = [tenant_id, start_date, end_date, UNATTRIBUTED_PROVIDER]

                    if user_id:
                        query += " AND user_id = $" + str(len(params) + 1)
                        params.append(user_id)
                    if service_id:
                        query += " AND service_id = $" + str(len(params) + 1)
                        params.append(service_id)

                    query += f" GROUP BY COALESCE(NULLIF(provider, ''), $4) ORDER BY total_cost_cents DESC LIMIT ${len(params) + 1}"
                    params.append(limit)
                else:
                    if dimension == "service":
                        query = """
                            SELECT
                                u.service_id as dimension_value,
                                COALESCE(NULLIF(s.name, ''), u.service_id) as dimension_label,
                                SUM(u.request_count) as total_requests,
                                SUM(u.total_input_tokens) as total_input_tokens,
                                SUM(u.total_output_tokens) as total_output_tokens,
                                SUM(u.total_cost_cents) as total_cost_cents
                            FROM usage_daily_aggregates u
                            LEFT JOIN services s
                              ON s.service_id = u.service_id
                            WHERE u.tenant_id = $1
                              AND u.date >= $2
                              AND u.date <= $3
                              AND u.service_id IS NOT NULL
                        """
                        group_by_expr = "u.service_id, dimension_label"
                    elif dimension == "user":
                        query = """
                            SELECT
                                u.user_id as dimension_value,
                                COALESCE(
                                    NULLIF(usr.display_name, ''),
                                    NULLIF(usr.username, ''),
                                    CASE
                                        WHEN u.user_id LIKE 'apikey:%' THEN 'API Key · ' || COALESCE(NULLIF(ak.name, ''), SUBSTRING(u.user_id FROM 8 FOR 6))
                                        WHEN u.user_id LIKE 'anon:%' THEN 'Anonymous · ' || RIGHT(u.user_id, 6)
                                        ELSE u.user_id
                                    END
                                ) as dimension_label,
                                SUM(u.request_count) as total_requests,
                                SUM(u.total_input_tokens) as total_input_tokens,
                                SUM(u.total_output_tokens) as total_output_tokens,
                                SUM(u.total_cost_cents) as total_cost_cents
                            FROM usage_daily_aggregates u
                            LEFT JOIN users usr
                              ON usr.user_id = u.user_id
                             AND usr.tenant_id = u.tenant_id
                            LEFT JOIN api_keys ak
                              ON ak.tenant_id = u.tenant_id
                             AND ('apikey:' || SUBSTRING(ak.key_hash, 1, 16)) = u.user_id
                            WHERE u.tenant_id = $1
                              AND u.date >= $2
                              AND u.date <= $3
                              AND u.user_id IS NOT NULL
                        """
                        group_by_expr = "u.user_id, dimension_label"
                    else:
                        query = f"""
                            SELECT
                                {dimension_column} as dimension_value,
                                {dimension_column} as dimension_label,
                                SUM(u.request_count) as total_requests,
                                SUM(u.total_input_tokens) as total_input_tokens,
                                SUM(u.total_output_tokens) as total_output_tokens,
                                SUM(u.total_cost_cents) as total_cost_cents
                            FROM usage_daily_aggregates u
                            WHERE u.tenant_id = $1
                              AND u.date >= $2
                              AND u.date <= $3
                              AND {dimension_column} IS NOT NULL
                        """
                        group_by_expr = f"{dimension_column}"

                    params: list[Any] = [tenant_id, start_date, end_date]

                    if user_id:
                        query += " AND u.user_id = $" + str(len(params) + 1)
                        params.append(user_id)
                    if service_id:
                        query += " AND u.service_id = $" + str(len(params) + 1)
                        params.append(service_id)

                    query += (
                        f" GROUP BY {group_by_expr}"
                        f" ORDER BY total_cost_cents DESC LIMIT ${len(params) + 1}"
                    )
                    params.append(limit)

                rows = await conn.fetch(query, *params)

                total_cost = sum(int(row["total_cost_cents"]) for row in rows)

                return [
                    {
                        dimension: row["dimension_value"],
                        "label": row["dimension_label"],
                        "requests": int(row["total_requests"]),
                        "input_tokens": int(row["total_input_tokens"]),
                        "output_tokens": int(row["total_output_tokens"]),
                        "total_tokens": int(row["total_input_tokens"])
                        + int(row["total_output_tokens"]),
                        # cost_cents is stored as microcents (cents * 10000) for precision
                        "cost_usd": round(int(row["total_cost_cents"]) / 1000000, 6),
                        "percentage": round(int(row["total_cost_cents"]) / total_cost * 100, 1)
                        if total_cost > 0
                        else 0,
                    }
                    for row in rows
                ]

        except Exception as e:
            logger.error(f"Failed to get usage breakdown: {e}")
            return []

    async def get_usage_timeseries(
        self,
        tenant_id: str,
        start_date: date | None = None,
        end_date: date | None = None,
        user_id: str | None = None,
        model: str | None = None,
        service_id: str | None = None,
        provider: str | None = None,
        granularity: str = "day",
    ) -> list[dict[str, Any]]:
        """Get usage time series."""
        if not self.database or not self.database._pool:
            return []

        if not start_date:
            start_date = date.today()
        if not end_date:
            end_date = date.today()

        try:
            async with self.database._pool.acquire() as conn:
                bucket_expr = (
                    "date_trunc('hour', created_at)"
                    if granularity == "hour"
                    else "date_trunc('day', created_at)"
                )
                query = f"""
                    SELECT
                        {bucket_expr} AS bucket,
                        COUNT(*) AS requests,
                        COALESCE(SUM(input_tokens), 0) AS input_tokens,
                        COALESCE(SUM(output_tokens), 0) AS output_tokens,
                        COALESCE(SUM(total_cost_cents), 0) AS cost_cents,
                        COALESCE(
                            AVG(COALESCE(NULLIF(request_total_duration_ms, 0), latency_ms, 0)),
                            0
                        ) AS avg_latency_ms
                    FROM usage_records
                    WHERE tenant_id = $1
                      AND created_at::date >= $2
                      AND created_at::date <= $3
                """
                params: list[Any] = [tenant_id, start_date, end_date]
                if provider:
                    query += f" AND provider = ${len(params) + 1}"
                    params.append(provider)
                if user_id:
                    query += f" AND user_id = ${len(params) + 1}"
                    params.append(user_id)
                if model:
                    query += f" AND model = ${len(params) + 1}"
                    params.append(model)
                if service_id:
                    query += f" AND service_id = ${len(params) + 1}"
                    params.append(service_id)

                query += " GROUP BY bucket ORDER BY bucket"
                rows = await conn.fetch(query, *params)

                return [
                    {
                        "date": row["bucket"].isoformat(),
                        "requests": int(row["requests"]),
                        "input_tokens": int(row["input_tokens"]),
                        "output_tokens": int(row["output_tokens"]),
                        "total_tokens": int(row["input_tokens"]) + int(row["output_tokens"]),
                        "cost_usd": round(int(row["cost_cents"]) / 1000000, 6),
                        "avg_latency_ms": int(row["avg_latency_ms"])
                        if row["avg_latency_ms"]
                        else 0,
                    }
                    for row in rows
                ]

        except Exception as e:
            logger.error(f"Failed to get usage timeseries: {e}")
            return []

    @staticmethod
    def _build_usage_record_conditions(
        *,
        tenant_id: str,
        start_date: date,
        end_date: date,
        user_id: str | None = None,
        model: str | None = None,
        service_id: str | None = None,
        provider: str | None = None,
    ) -> tuple[list[str], list[Any]]:
        conditions = [
            "tenant_id = $1",
            "created_at::date >= $2",
            "created_at::date <= $3",
        ]
        params: list[Any] = [tenant_id, start_date, end_date]

        if user_id:
            conditions.append(f"user_id = ${len(params) + 1}")
            params.append(user_id)
        if model:
            conditions.append(f"model = ${len(params) + 1}")
            params.append(model)
        if service_id:
            conditions.append(f"service_id = ${len(params) + 1}")
            params.append(service_id)
        if provider:
            conditions.append(f"provider = ${len(params) + 1}")
            params.append(provider)

        return conditions, params

    async def get_latency_breakdown_timeseries(
        self,
        tenant_id: str,
        start_date: date | None = None,
        end_date: date | None = None,
        user_id: str | None = None,
        model: str | None = None,
        service_id: str | None = None,
        provider: str | None = None,
        granularity: str = "day",
    ) -> list[dict[str, Any]]:
        """Get detailed latency/phase breakdown for dashboard performance panel."""
        if not self.database or not self.database._pool:
            return []

        if not start_date:
            start_date = date.today()
        if not end_date:
            end_date = date.today()

        bucket_expr = (
            "date_trunc('hour', created_at)"
            if granularity == "hour"
            else "date_trunc('day', created_at)"
        )
        conditions, params = self._build_usage_record_conditions(
            tenant_id=tenant_id,
            start_date=start_date,
            end_date=end_date,
            user_id=user_id,
            model=model,
            service_id=service_id,
            provider=provider,
        )

        try:
            async with self.database._pool.acquire() as conn:
                query = f"""
                    SELECT
                        {bucket_expr} AS bucket,
                        COUNT(*) AS request_count,
                        COALESCE(
                            AVG(COALESCE(NULLIF(request_total_duration_ms, 0), latency_ms, 0)),
                            0
                        ) AS avg_total_ms,
                        COALESCE(
                            PERCENTILE_CONT(0.50) WITHIN GROUP (
                                ORDER BY COALESCE(NULLIF(request_total_duration_ms, 0), latency_ms, 0)
                            ),
                            0
                        ) AS p50_total_ms,
                        COALESCE(
                            PERCENTILE_CONT(0.95) WITHIN GROUP (
                                ORDER BY COALESCE(NULLIF(request_total_duration_ms, 0), latency_ms, 0)
                            ),
                            0
                        ) AS p95_total_ms,
                        COALESCE(
                            PERCENTILE_CONT(0.99) WITHIN GROUP (
                                ORDER BY COALESCE(NULLIF(request_total_duration_ms, 0), latency_ms, 0)
                            ),
                            0
                        ) AS p99_total_ms,
                        COALESCE(AVG(first_token_ms), 0) AS avg_first_token_ms,
                        COALESCE(AVG(llm_inference_duration_ms), 0) AS avg_llm_inference_ms,
                        COALESCE(AVG(retrieval_duration_ms), 0) AS avg_retrieval_ms,
                        COALESCE(AVG(tool_call_duration_ms), 0) AS avg_tool_call_ms,
                        COALESCE(AVG(agent_or_graph_overhead_ms), 0) AS avg_overhead_ms
                    FROM usage_records
                    WHERE {" AND ".join(conditions)}
                    GROUP BY bucket
                    ORDER BY bucket
                """
                rows = await conn.fetch(query, *params)
                return [
                    {
                        "date": row["bucket"].isoformat(),
                        "request_count": int(row["request_count"]),
                        "avg_total_ms": int(row["avg_total_ms"] or 0),
                        "p50_total_ms": int(row["p50_total_ms"] or 0),
                        "p95_total_ms": int(row["p95_total_ms"] or 0),
                        "p99_total_ms": int(row["p99_total_ms"] or 0),
                        "avg_first_token_ms": int(row["avg_first_token_ms"] or 0),
                        "avg_llm_inference_ms": int(row["avg_llm_inference_ms"] or 0),
                        "avg_retrieval_ms": int(row["avg_retrieval_ms"] or 0),
                        "avg_tool_call_ms": int(row["avg_tool_call_ms"] or 0),
                        "avg_overhead_ms": int(row["avg_overhead_ms"] or 0),
                    }
                    for row in rows
                ]
        except Exception as e:
            logger.error(f"Failed to get latency breakdown: {e}")
            return []

    async def get_failure_breakdown(
        self,
        tenant_id: str,
        start_date: date | None = None,
        end_date: date | None = None,
        dimension: str = "service",
        user_id: str | None = None,
        model: str | None = None,
        service_id: str | None = None,
        provider: str | None = None,
        limit: int = 50,
    ) -> list[dict[str, Any]]:
        """Get failure categorization breakdown by dimension + error_type."""
        if not self.database or not self.database._pool:
            return []

        if not start_date:
            start_date = date.today()
        if not end_date:
            end_date = date.today()

        dimension_map = {
            "service": "COALESCE(NULLIF(service_id, ''), 'unattributed_service')",
            "model": "COALESCE(NULLIF(model, ''), 'unattributed_model')",
            "provider": "COALESCE(NULLIF(provider, ''), 'unattributed')",
            "user": "COALESCE(NULLIF(user_id, ''), 'anonymous')",
        }
        dimension_expr = dimension_map.get(dimension)
        if not dimension_expr:
            return []

        conditions, params = self._build_usage_record_conditions(
            tenant_id=tenant_id,
            start_date=start_date,
            end_date=end_date,
            user_id=user_id,
            model=model,
            service_id=service_id,
            provider=provider,
        )

        try:
            async with self.database._pool.acquire() as conn:
                query = f"""
                    SELECT
                        {dimension_expr} AS dimension_value,
                        COALESCE(NULLIF(error_type, ''), 'unknown') AS error_type,
                        COUNT(*) FILTER (WHERE status != 'success') AS failure_count,
                        COUNT(*) FILTER (WHERE status = 'success') AS success_count,
                        COUNT(*) AS total_count
                    FROM usage_records
                    WHERE {" AND ".join(conditions)}
                    GROUP BY dimension_value, error_type
                    HAVING COUNT(*) FILTER (WHERE status != 'success') > 0
                    ORDER BY failure_count DESC, total_count DESC
                    LIMIT ${len(params) + 1}
                """
                params.append(limit)
                rows = await conn.fetch(query, *params)
                return [
                    {
                        "dimension": dimension,
                        "dimension_value": row["dimension_value"],
                        "error_type": row["error_type"],
                        "failure_count": int(row["failure_count"]),
                        "success_count": int(row["success_count"]),
                        "total_count": int(row["total_count"]),
                        "success_rate": (
                            round(int(row["success_count"]) / int(row["total_count"]) * 100, 2)
                            if int(row["total_count"]) > 0
                            else 0.0
                        ),
                    }
                    for row in rows
                ]
        except Exception as e:
            logger.error(f"Failed to get failure breakdown: {e}")
            return []

    async def get_request_trace_by_id(
        self, tenant_id: str, request_id: str
    ) -> dict[str, Any] | None:
        """Get latest sampled trace by request_id."""
        if not self.database or not self.database._pool:
            return None
        try:
            async with self.database._pool.acquire() as conn:
                row = await conn.fetchrow(
                    """
                    SELECT *
                    FROM request_traces
                    WHERE tenant_id = $1 AND request_id = $2
                    ORDER BY created_at DESC
                    LIMIT 1
                    """,
                    tenant_id,
                    request_id,
                )
                return self._trace_row_to_dict(row) if row else None
        except Exception as e:
            logger.error(f"Failed to get request trace: {e}")
            return None

    async def list_request_traces(
        self,
        tenant_id: str,
        start_date: date | None = None,
        end_date: date | None = None,
        service_id: str | None = None,
        user_id: str | None = None,
        provider: str | None = None,
        model: str | None = None,
        status: str | None = None,
        error_type: str | None = None,
        limit: int = 100,
    ) -> list[dict[str, Any]]:
        """List sampled traces for dashboard trace table."""
        if not self.database or not self.database._pool:
            return []

        if not start_date:
            start_date = date.today()
        if not end_date:
            end_date = date.today()

        conditions = [
            "tenant_id = $1",
            "created_at::date >= $2",
            "created_at::date <= $3",
        ]
        params: list[Any] = [tenant_id, start_date, end_date]
        if service_id:
            conditions.append(f"service_id = ${len(params) + 1}")
            params.append(service_id)
        if user_id:
            conditions.append(f"user_id = ${len(params) + 1}")
            params.append(user_id)
        if provider:
            conditions.append(f"provider = ${len(params) + 1}")
            params.append(provider)
        if model:
            conditions.append(f"model = ${len(params) + 1}")
            params.append(model)
        if status:
            conditions.append(f"status = ${len(params) + 1}")
            params.append(status)
        if error_type:
            conditions.append(f"error_type = ${len(params) + 1}")
            params.append(error_type)

        try:
            async with self.database._pool.acquire() as conn:
                query = f"""
                    SELECT *
                    FROM request_traces
                    WHERE {" AND ".join(conditions)}
                    ORDER BY created_at DESC
                    LIMIT ${len(params) + 1}
                """
                params.append(limit)
                rows = await conn.fetch(query, *params)
                return [self._trace_row_to_dict(row) for row in rows]
        except Exception as e:
            logger.error(f"Failed to list request traces: {e}")
            return []

    @staticmethod
    def _trace_row_to_dict(row: Any) -> dict[str, Any]:
        if not row:
            return {}
        trace_steps = row["trace_steps"]
        metadata = row["metadata"]
        if isinstance(trace_steps, str):
            try:
                trace_steps = json.loads(trace_steps)
            except Exception:
                trace_steps = []
        if isinstance(metadata, str):
            try:
                metadata = json.loads(metadata)
            except Exception:
                metadata = {}
        return {
            "trace_id": str(row["trace_id"]),
            "request_id": row["request_id"],
            "tenant_id": row["tenant_id"],
            "user_id": row["user_id"],
            "service_id": row["service_id"],
            "assistant_id": row["assistant_id"],
            "provider": row["provider"],
            "model": row["model"],
            "status": row["status"],
            "error_type": row["error_type"],
            "request_total_duration_ms": int(row["request_total_duration_ms"] or 0),
            "first_token_latency_ms": int(row["first_token_latency_ms"] or 0),
            "llm_inference_duration_ms": int(row["llm_inference_duration_ms"] or 0),
            "retrieval_duration_ms": int(row["retrieval_duration_ms"] or 0),
            "tool_call_duration_ms": int(row["tool_call_duration_ms"] or 0),
            "agent_or_graph_overhead_ms": int(row["agent_or_graph_overhead_ms"] or 0),
            "input_tokens": int(row["input_tokens"] or 0),
            "output_tokens": int(row["output_tokens"] or 0),
            "total_cost_usd": round(int(row["total_cost_cents"] or 0) / 1000000, 6),
            "sample_reason": row["sample_reason"],
            "trace_steps": trace_steps if isinstance(trace_steps, list) else [],
            "metadata": metadata if isinstance(metadata, dict) else {},
            "timestamp": row["created_at"].isoformat() if row["created_at"] else None,
        }

    async def get_last_ingested_at(
        self,
        tenant_id: str,
        start_date: date | None = None,
        end_date: date | None = None,
        granularity: str = "day",
    ) -> datetime | None:
        if not self.database or not self.database._pool:
            return None
        return await self.database.get_usage_last_ingested_at(
            tenant_id=tenant_id,
            start_date=start_date,
            end_date=end_date,
            granularity=granularity,
        )

    def _empty_summary(self, *, data_status: str = "empty") -> dict[str, Any]:
        """Return empty summary."""
        return {
            "total_requests": 0,
            "success_rate": None,
            "data_status": data_status,
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "total_tokens": 0,
            "total_cost_usd": 0.0,
            "avg_latency_ms": 0,
            "start_date": date.today().isoformat(),
            "end_date": date.today().isoformat(),
        }


def get_usage_recorder() -> UsageRecorder:
    """Get the global UsageRecorder singleton."""
    global _usage_recorder
    if _usage_recorder is None:
        _usage_recorder = UsageRecorder()
    return _usage_recorder


def init_usage_recorder(database: DatabaseStorage) -> UsageRecorder:
    """Initialize the global UsageRecorder with database storage."""
    global _usage_recorder
    if _usage_recorder is None:
        _usage_recorder = UsageRecorder(database)
    else:
        _usage_recorder.set_database(database)
    return _usage_recorder
