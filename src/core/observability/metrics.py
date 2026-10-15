"""Compatibility exports for the Gateway-owned metrics collector."""

from ...services.metrics.collector import (
    Counter as Counter,
)
from ...services.metrics.collector import (
    Gauge as Gauge,
)
from ...services.metrics.collector import (
    Histogram as Histogram,
)
from ...services.metrics.collector import (
    MetricsCollector as MetricsCollector,
)
from ...services.metrics.collector import (
    MetricValue as MetricValue,
)
from ...services.metrics.collector import (
    RequestMetrics as RequestMetrics,
)
from ...services.metrics.collector import (
    get_metrics as get_metrics,
)

__all__ = [
    "MetricValue",
    "Counter",
    "Gauge",
    "Histogram",
    "RequestMetrics",
    "MetricsCollector",
    "get_metrics",
]
