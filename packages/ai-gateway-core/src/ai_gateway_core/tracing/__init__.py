"""OpenTelemetry distributed tracing for the AI Gateway microservices.

This package provides three building blocks used by Gateway and Knowledge:

1. ``init_tracing(service_name, ...)`` — idempotent SDK setup. Configures a
   ``TracerProvider`` with a batch OTLP/gRPC exporter (or no exporter when
   ``OTEL_EXPORTER_OTLP_ENDPOINT`` is unset, which keeps the SDK active so
   spans are still recorded but never shipped — useful for tests/dev).
2. ``OTelInboundMiddleware`` — extracts W3C ``traceparent`` from inbound
   request headers and starts a server span. The active context is exposed
   on ``request.state`` (``traceparent`` / ``tracestate``) so the proxy
   layer can forward it without rebuilding it from the OTel context.
3. ``instrument_httpx_client(client)`` — adds OTel spans to outbound
   ``httpx.AsyncClient`` calls so a request that fans out from gateway →
   Gateway → knowledge-service appears as one trace tree.

Existing ``X-Request-Id`` propagation (``RequestIDMiddleware``) is left
untouched. ``traceparent`` is the W3C standard span correlation; X-Request-Id
remains the human-friendly aggregation key for log scraping.
"""

from .httpx_hooks import instrument_httpx_client
from .init import init_tracing, is_initialized
from .middleware import OTelInboundMiddleware
from .propagation import internal_http_headers

__all__ = [
    "OTelInboundMiddleware",
    "init_tracing",
    "instrument_httpx_client",
    "internal_http_headers",
    "is_initialized",
]
