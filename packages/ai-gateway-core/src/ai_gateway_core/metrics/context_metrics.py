"""
Context Metrics for Enterprise AI Assistant.

Provides observability into context engineering performance:
- Token budget tracking per layer
- Compression effectiveness
- Cache hit rates
- Memory utilization

Reference: Manus Context Engineering best practices
"""

from __future__ import annotations

import time
from collections import deque
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import TYPE_CHECKING, Any

from ai_gateway_core.logging import get_logger

if TYPE_CHECKING:
    from ai_gateway_core.persistence import DatabaseStorageLike

logger = get_logger(__name__)


# =============================================================================
# Enums and Constants
# =============================================================================


class MetricLayer(str, Enum):
    """Context layers for token tracking."""

    STATIC_PREFIX = "static_prefix"  # System prompt, tools, guardrails
    USER_CONTEXT = "user_context"  # User preferences, long-term memory
    SESSION_CONTEXT = "session_context"  # Compressed history, working memory
    REQUEST_CONTEXT = "request_context"  # RAG results, current query


# Default token budgets per layer
DEFAULT_TOKEN_BUDGETS = {
    MetricLayer.STATIC_PREFIX: 500,
    MetricLayer.USER_CONTEXT: 300,
    MetricLayer.SESSION_CONTEXT: 2000,
    MetricLayer.REQUEST_CONTEXT: 3000,
}

TOTAL_TOKEN_BUDGET = 8000  # Total context budget


# =============================================================================
# Data Classes
# =============================================================================


@dataclass
class LayerMetrics:
    """Metrics for a single context layer."""

    layer: MetricLayer
    token_count: int = 0
    budget: int = 0
    utilization: float = 0.0  # token_count / budget

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary."""
        return {
            "layer": self.layer.value,
            "token_count": self.token_count,
            "budget": self.budget,
            "utilization": round(self.utilization, 3),
        }


@dataclass
class CompressionMetrics:
    """Metrics for context compression."""

    original_tokens: int = 0
    compressed_tokens: int = 0
    compression_ratio: float = 1.0  # original / compressed
    messages_compressed: int = 0
    preserved_urls: int = 0
    preserved_code_blocks: int = 0
    summary_tokens: int = 0

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary."""
        return {
            "original_tokens": self.original_tokens,
            "compressed_tokens": self.compressed_tokens,
            "compression_ratio": round(self.compression_ratio, 2),
            "messages_compressed": self.messages_compressed,
            "preserved_urls": self.preserved_urls,
            "preserved_code_blocks": self.preserved_code_blocks,
            "summary_tokens": self.summary_tokens,
        }


@dataclass
class CacheMetrics:
    """Metrics for KV-cache performance."""

    cache_read_tokens: int = 0  # Tokens read from cache
    cache_creation_tokens: int = 0  # Tokens added to cache
    hit_rate: float = 0.0  # cache_read / (cache_read + cache_creation)
    estimated_savings_usd: float = 0.0  # Cost savings from cache hits

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary."""
        return {
            "cache_read_tokens": self.cache_read_tokens,
            "cache_creation_tokens": self.cache_creation_tokens,
            "hit_rate": round(self.hit_rate, 3),
            "estimated_savings_usd": round(self.estimated_savings_usd, 4),
        }


@dataclass
class MemoryMetrics:
    """Metrics for memory system usage."""

    long_term_loaded: bool = False
    session_loaded: bool = False
    working_memory_tasks: int = 0
    working_memory_restored: bool = False
    working_memory_persisted: bool = False

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary."""
        return {
            "long_term_loaded": self.long_term_loaded,
            "session_loaded": self.session_loaded,
            "working_memory_tasks": self.working_memory_tasks,
            "working_memory_restored": self.working_memory_restored,
            "working_memory_persisted": self.working_memory_persisted,
        }


@dataclass
class ContextMetrics:
    """
    Comprehensive context metrics for a single request.

    Tracks token usage, compression, caching, and memory across
    all phases of the agent loop.
    """

    # Request identification
    request_id: str = ""
    session_id: str = ""
    tenant_id: str = ""
    user_id: str = ""

    # Timing
    timestamp: datetime = field(default_factory=datetime.now)
    total_duration_ms: float = 0.0

    # Token metrics per layer
    layer_metrics: list[LayerMetrics] = field(default_factory=list)

    # Aggregate token metrics
    total_tokens: int = 0
    budget_tokens: int = TOTAL_TOKEN_BUDGET
    utilization: float = 0.0  # total / budget

    # Compression metrics
    compression: CompressionMetrics = field(default_factory=CompressionMetrics)

    # Cache metrics
    cache: CacheMetrics = field(default_factory=CacheMetrics)

    # Memory metrics
    memory: MemoryMetrics = field(default_factory=MemoryMetrics)

    # Phase durations
    phase_durations: dict[str, float] = field(default_factory=dict)

    def calculate_totals(self) -> None:
        """Calculate aggregate metrics from layer metrics."""
        self.total_tokens = sum(lm.token_count for lm in self.layer_metrics)
        self.utilization = self.total_tokens / max(1, self.budget_tokens)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization."""
        return {
            "request_id": self.request_id,
            "session_id": self.session_id,
            "tenant_id": self.tenant_id,
            "user_id": self.user_id,
            "timestamp": self.timestamp.isoformat(),
            "total_duration_ms": round(self.total_duration_ms, 2),
            "total_tokens": self.total_tokens,
            "budget_tokens": self.budget_tokens,
            "utilization": round(self.utilization, 3),
            "layer_metrics": [lm.to_dict() for lm in self.layer_metrics],
            "compression": self.compression.to_dict(),
            "cache": self.cache.to_dict(),
            "memory": self.memory.to_dict(),
            "phase_durations": {k: round(v, 2) for k, v in self.phase_durations.items()},
        }

    def to_event_data(self) -> dict[str, Any]:
        """Convert to event data for frontend streaming."""
        return {
            "total_tokens": self.total_tokens,
            "utilization": round(self.utilization * 100, 1),  # Percentage
            "compression_ratio": self.compression.compression_ratio,
            "cache_hit_rate": round(self.cache.hit_rate * 100, 1),  # Percentage
            "layers": {
                lm.layer.value: {
                    "tokens": lm.token_count,
                    "utilization": round(lm.utilization * 100, 1),
                }
                for lm in self.layer_metrics
            },
        }


# =============================================================================
# Metrics Collector
# =============================================================================


class ContextMetricsCollector:
    """
    Collects and persists context metrics.

    Usage:
        ```python
        collector = ContextMetricsCollector(db)

        # During request processing
        metrics = ContextMetrics(request_id="req_123", ...)

        # After request completion
        await collector.record(metrics)

        # Query historical metrics
        stats = await collector.get_session_stats(session_id)
        ```
    """

    def __init__(
        self,
        database: DatabaseStorageLike | None = None,
        enable_persistence: bool = True,
    ):
        """
        Initialize collector.

        Args:
            database: Database storage for persistence
            enable_persistence: Whether to persist metrics to database
        """
        self.database = database
        self.enable_persistence = enable_persistence and database is not None
        self._max_recent = 100  # Keep last N metrics in memory
        # Use deque with maxlen for O(1) append and automatic eviction
        self._recent_metrics: deque[ContextMetrics] = deque(maxlen=self._max_recent)

    async def record(self, metrics: ContextMetrics) -> None:
        """
        Record context metrics.

        Args:
            metrics: The metrics to record
        """
        # Calculate totals
        metrics.calculate_totals()

        # Store in memory (deque automatically evicts oldest when full)
        self._recent_metrics.append(metrics)

        # Persist to database if enabled
        if self.enable_persistence:
            await self._persist_metrics(metrics)

        logger.debug(
            f"Recorded context metrics: {metrics.total_tokens} tokens, "
            f"{metrics.utilization:.1%} utilization, "
            f"compression={metrics.compression.compression_ratio:.1f}x"
        )

    async def _persist_metrics(self, metrics: ContextMetrics) -> None:
        """Persist metrics to database."""
        if not self.database:
            return

        try:
            # Store as usage_statistics with dimension="context"
            query = """
                INSERT INTO usage_statistics (
                    dimension, dimension_id, period_type, period_start,
                    request_count, success_count, input_tokens, output_tokens,
                    avg_response_time_ms
                ) VALUES (
                    'context', $1, 'request', $2,
                    1, 1, $3, 0, $4
                )
            """
            await self.database.execute(
                query,
                metrics.session_id,
                metrics.timestamp,
                metrics.total_tokens,
                int(metrics.total_duration_ms),
            )
        except Exception as e:
            logger.warning(f"Failed to persist context metrics: {e}")

    async def get_session_stats(
        self,
        session_id: str,
        limit: int = 50,
    ) -> dict[str, Any]:
        """
        Get aggregated statistics for a session.

        Args:
            session_id: Session to query
            limit: Maximum records to aggregate

        Returns:
            Aggregated statistics
        """
        # Filter recent metrics for this session
        session_metrics = [m for m in self._recent_metrics if m.session_id == session_id][-limit:]

        if not session_metrics:
            return {
                "session_id": session_id,
                "request_count": 0,
                "avg_tokens": 0,
                "avg_utilization": 0,
                "avg_compression_ratio": 0,
                "avg_cache_hit_rate": 0,
            }

        return {
            "session_id": session_id,
            "request_count": len(session_metrics),
            "avg_tokens": sum(m.total_tokens for m in session_metrics) // len(session_metrics),
            "avg_utilization": sum(m.utilization for m in session_metrics) / len(session_metrics),
            "avg_compression_ratio": sum(m.compression.compression_ratio for m in session_metrics)
            / len(session_metrics),
            "avg_cache_hit_rate": sum(m.cache.hit_rate for m in session_metrics)
            / len(session_metrics),
            "total_tokens_used": sum(m.total_tokens for m in session_metrics),
        }

    async def get_tenant_stats(
        self,
        tenant_id: str,
        hours: int = 24,
    ) -> dict[str, Any]:
        """
        Get aggregated statistics for a tenant over time.

        Args:
            tenant_id: Tenant to query
            hours: Time window in hours

        Returns:
            Aggregated statistics
        """
        cutoff = datetime.now().timestamp() - (hours * 3600)

        # Filter recent metrics for this tenant
        tenant_metrics = [
            m
            for m in self._recent_metrics
            if m.tenant_id == tenant_id and m.timestamp.timestamp() > cutoff
        ]

        if not tenant_metrics:
            return {
                "tenant_id": tenant_id,
                "hours": hours,
                "request_count": 0,
                "unique_sessions": 0,
                "total_tokens": 0,
            }

        unique_sessions = len({m.session_id for m in tenant_metrics})

        return {
            "tenant_id": tenant_id,
            "hours": hours,
            "request_count": len(tenant_metrics),
            "unique_sessions": unique_sessions,
            "total_tokens": sum(m.total_tokens for m in tenant_metrics),
            "avg_tokens_per_request": sum(m.total_tokens for m in tenant_metrics)
            // len(tenant_metrics),
            "avg_utilization": sum(m.utilization for m in tenant_metrics) / len(tenant_metrics),
        }


# =============================================================================
# Metrics Builder
# =============================================================================


class ContextMetricsBuilder:
    """
    Builder for constructing ContextMetrics during request processing.

    Usage:
        ```python
        builder = ContextMetricsBuilder(request_id, session_id, tenant_id, user_id)

        # Track each phase
        with builder.phase("memory_loading"):
            # ... load memory

        builder.set_layer_tokens(MetricLayer.STATIC_PREFIX, 450)
        builder.set_compression(original=5000, compressed=1200, messages=10)

        # Finalize
        metrics = builder.build()
        ```
    """

    def __init__(
        self,
        request_id: str,
        session_id: str,
        tenant_id: str,
        user_id: str,
    ):
        """Initialize builder with request context."""
        self._metrics = ContextMetrics(
            request_id=request_id,
            session_id=session_id,
            tenant_id=tenant_id,
            user_id=user_id,
        )
        self._start_time = time.time()
        self._phase_start: float | None = None
        self._current_phase: str | None = None
        self._layer_tokens: dict[MetricLayer, int] = {}

    class _PhaseContext:
        """Context manager for tracking phase duration."""

        def __init__(self, builder: ContextMetricsBuilder, phase_name: str):
            self.builder = builder
            self.phase_name = phase_name
            self.start_time: float = 0

        def __enter__(self):
            self.start_time = time.time()
            return self

        def __exit__(self, exc_type, exc_val, exc_tb):
            duration_ms = (time.time() - self.start_time) * 1000
            self.builder._metrics.phase_durations[self.phase_name] = duration_ms
            return False

    def phase(self, phase_name: str) -> _PhaseContext:
        """
        Context manager for tracking phase duration.

        Args:
            phase_name: Name of the phase (e.g., "memory_loading")

        Returns:
            Context manager
        """
        return self._PhaseContext(self, phase_name)

    def set_layer_tokens(self, layer: MetricLayer, tokens: int) -> None:
        """
        Set token count for a context layer.

        Args:
            layer: The context layer
            tokens: Token count
        """
        self._layer_tokens[layer] = tokens

    def set_compression(
        self,
        original: int,
        compressed: int,
        messages: int,
        preserved_urls: int = 0,
        preserved_code_blocks: int = 0,
        summary_tokens: int = 0,
    ) -> None:
        """
        Set compression metrics.

        Args:
            original: Original token count
            compressed: Compressed token count
            messages: Number of messages compressed
            preserved_urls: URLs preserved
            preserved_code_blocks: Code blocks preserved
            summary_tokens: Tokens in summary
        """
        self._metrics.compression = CompressionMetrics(
            original_tokens=original,
            compressed_tokens=compressed,
            compression_ratio=original / max(1, compressed),
            messages_compressed=messages,
            preserved_urls=preserved_urls,
            preserved_code_blocks=preserved_code_blocks,
            summary_tokens=summary_tokens,
        )

    def set_cache(
        self,
        cache_read: int,
        cache_creation: int,
        cost_per_1k_cached: float = 0.0003,  # Anthropic cached input price
        cost_per_1k_uncached: float = 0.003,  # Anthropic standard input price
    ) -> None:
        """
        Set cache metrics.

        Args:
            cache_read: Tokens read from cache
            cache_creation: Tokens added to cache
            cost_per_1k_cached: Cost per 1K cached tokens
            cost_per_1k_uncached: Cost per 1K uncached tokens
        """
        total = cache_read + cache_creation
        hit_rate = cache_read / max(1, total)

        # Calculate savings: what it would have cost without cache
        actual_cost = (
            cache_read * cost_per_1k_cached + cache_creation * cost_per_1k_uncached
        ) / 1000
        no_cache_cost = total * cost_per_1k_uncached / 1000
        savings = no_cache_cost - actual_cost

        self._metrics.cache = CacheMetrics(
            cache_read_tokens=cache_read,
            cache_creation_tokens=cache_creation,
            hit_rate=hit_rate,
            estimated_savings_usd=max(0, savings),
        )

    def set_memory(
        self,
        long_term_loaded: bool = False,
        session_loaded: bool = False,
        working_memory_tasks: int = 0,
        working_memory_restored: bool = False,
        working_memory_persisted: bool = False,
    ) -> None:
        """Set memory metrics."""
        self._metrics.memory = MemoryMetrics(
            long_term_loaded=long_term_loaded,
            session_loaded=session_loaded,
            working_memory_tasks=working_memory_tasks,
            working_memory_restored=working_memory_restored,
            working_memory_persisted=working_memory_persisted,
        )

    def build(self) -> ContextMetrics:
        """
        Build the final ContextMetrics object.

        Returns:
            Completed ContextMetrics
        """
        # Set total duration
        self._metrics.total_duration_ms = (time.time() - self._start_time) * 1000

        # Build layer metrics
        for layer in MetricLayer:
            tokens = self._layer_tokens.get(layer, 0)
            budget = DEFAULT_TOKEN_BUDGETS.get(layer, 0)
            self._metrics.layer_metrics.append(
                LayerMetrics(
                    layer=layer,
                    token_count=tokens,
                    budget=budget,
                    utilization=tokens / max(1, budget),
                )
            )

        # Calculate totals
        self._metrics.calculate_totals()

        return self._metrics


# =============================================================================
# Module-level Singleton
# =============================================================================


_collector: ContextMetricsCollector | None = None


def get_context_metrics_collector() -> ContextMetricsCollector:
    """Get the global context metrics collector."""
    global _collector
    if _collector is None:
        _collector = ContextMetricsCollector(enable_persistence=False)
    return _collector


def init_context_metrics_collector(
    database: DatabaseStorageLike | None = None,
) -> ContextMetricsCollector:
    """Initialize the global context metrics collector."""
    global _collector
    _collector = ContextMetricsCollector(
        database=database,
        enable_persistence=database is not None,
    )
    return _collector
