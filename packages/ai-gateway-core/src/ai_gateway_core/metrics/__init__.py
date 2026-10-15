"""Metrics contract.

Both services record usage and realtime metrics. The concrete recorders
(Prometheus/Redis/DB-backed) live per-service; assistant code talks to
them via these lightweight Protocols.

NoOp reference impls are provided so an un-injected AssistantService
degrades to silent no-op recording instead of NoneType-crashing.
"""

from __future__ import annotations

from typing import Any, Protocol, runtime_checkable


@runtime_checkable
class UsageRecorderLike(Protocol):
    """Contract for recording per-request usage (tokens, cost, latency)."""

    async def record_usage(self, **fields: Any) -> None: ...


@runtime_checkable
class RealtimeMetricsLike(Protocol):
    """Contract for realtime counters consumed by dashboards."""

    async def record_token_usage(self, input_tokens: int, output_tokens: int) -> None: ...


class NoOpUsageRecorder:
    """Protocol-satisfying null UsageRecorder. All calls silently succeed."""

    async def record_usage(self, **fields: Any) -> None:
        return None


class NoOpRealtimeMetrics:
    """Protocol-satisfying null RealtimeMetrics. All calls silently succeed."""

    async def record_token_usage(self, input_tokens: int, output_tokens: int) -> None:
        return None


from .context_metrics import (
    ContextMetricsCollector,
    MetricLayer,
    get_context_metrics_collector,
)
from .observability import KNOWN_ERROR_TYPES, should_sample_trace
from .realtime_metrics import RealtimeMetricsService, RealtimeSnapshot, get_realtime_metrics
from .usage_recorder import UsageRecord, UsageRecorder, get_usage_recorder

__all__ = [
    "KNOWN_ERROR_TYPES",
    "ContextMetricsCollector",
    "MetricLayer",
    "NoOpRealtimeMetrics",
    "NoOpUsageRecorder",
    "RealtimeMetricsLike",
    "RealtimeMetricsService",
    "RealtimeSnapshot",
    "UsageRecord",
    "UsageRecorder",
    "UsageRecorderLike",
    "get_context_metrics_collector",
    "get_realtime_metrics",
    "get_usage_recorder",
    "should_sample_trace",
]
