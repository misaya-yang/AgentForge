"""Back-compat shim — realtime_metrics moved to ai_gateway_core in Phase 5f Batch C.

Canonical location: ``ai_gateway_core.metrics.realtime_metrics``.
"""

from __future__ import annotations

from ai_gateway_core.metrics.realtime_metrics import (
    RealtimeMetricsService,
    RealtimeSnapshot,
    get_realtime_metrics,
    init_realtime_metrics,
)

__all__ = [
    "RealtimeMetricsService",
    "RealtimeSnapshot",
    "get_realtime_metrics",
    "init_realtime_metrics",
]
