"""
Metrics Service - Dashboard metrics collection and aggregation

Provides:
- MetricsRecorder: Core metrics recording to Redis
- RealtimeMetricsService: Real-time sliding window metrics
"""

from .data_status import compute_data_status
from .metrics_recorder import MetricsRecorder, get_metrics_recorder, init_metrics_recorder
from .realtime_metrics import (
    RealtimeMetricsService,
    RealtimeSnapshot,
    get_realtime_metrics,
    init_realtime_metrics,
)
from .security_event_recorder import (
    SecurityEventRecorder,
    get_security_event_recorder,
    init_security_event_recorder,
)
from .usage_recorder import UsageRecorder, get_usage_recorder, init_usage_recorder

__all__ = [
    "MetricsRecorder",
    "get_metrics_recorder",
    "init_metrics_recorder",
    "RealtimeMetricsService",
    "RealtimeSnapshot",
    "get_realtime_metrics",
    "init_realtime_metrics",
    "compute_data_status",
    "UsageRecorder",
    "get_usage_recorder",
    "init_usage_recorder",
    "SecurityEventRecorder",
    "get_security_event_recorder",
    "init_security_event_recorder",
]
