"""Back-compat shim — observability helpers moved to ai_gateway_core in Phase 5f Batch C.

Canonical location: ``ai_gateway_core.metrics.observability``.
"""

from __future__ import annotations

from ai_gateway_core.metrics.observability import (
    ERROR_TYPE_AUTH,
    ERROR_TYPE_CONTENT_FILTER,
    ERROR_TYPE_PROVIDER,
    ERROR_TYPE_RATE_LIMIT,
    ERROR_TYPE_TIMEOUT,
    ERROR_TYPE_TOOL,
    ERROR_TYPE_UNKNOWN,
    KNOWN_ERROR_TYPES,
    classify_error_type,
    ensure_duration_breakdown,
    extract_duration_breakdown,
    should_sample_trace,
)

__all__ = [
    "ERROR_TYPE_AUTH",
    "ERROR_TYPE_CONTENT_FILTER",
    "ERROR_TYPE_PROVIDER",
    "ERROR_TYPE_RATE_LIMIT",
    "ERROR_TYPE_TIMEOUT",
    "ERROR_TYPE_TOOL",
    "ERROR_TYPE_UNKNOWN",
    "KNOWN_ERROR_TYPES",
    "classify_error_type",
    "ensure_duration_breakdown",
    "extract_duration_breakdown",
    "should_sample_trace",
]
