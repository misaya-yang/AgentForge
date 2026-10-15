"""Back-compat shim — usage_recorder moved to ai_gateway_core in Phase 5f Batch C.

Canonical location: ``ai_gateway_core.metrics.usage_recorder``.
"""

from __future__ import annotations

from ai_gateway_core.metrics.usage_recorder import (
    UsageRecord,
    UsageRecorder,
    group_records_by_hour,
    get_usage_recorder,
    init_usage_recorder,
)

__all__ = [
    "UsageRecord",
    "UsageRecorder",
    "group_records_by_hour",
    "get_usage_recorder",
    "init_usage_recorder",
]
