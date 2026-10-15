"""Inbound OTel server-span middleware.

Mirrors ``RequestIDMiddleware`` (in the same package) — both are
``BaseHTTPMiddleware`` subclasses, both bind transport-level metadata to
``request.state`` for downstream code to read, and both wrap the
``call_next`` call so it executes inside an active context.

Why ``BaseHTTPMiddleware`` and not the FastAPI auto-instrumentor?
----------------------------------------------------------------
``opentelemetry-instrumentation-fastapi`` works fine for plain JSON
responses but interferes with our streaming SSE path: it internally
buffers some response metadata which adds a few ms to TTFT (we measure
this in ``project_perf_debug_session_0410.md``). The hand-rolled
middleware below extracts traceparent, opens a span, sets attributes,
and never touches the response body. SSE streams pass straight through.

Status code handling
--------------------
- 5xx → span status = ERROR.
- All other status codes → status = OK (4xx is "client error", not a
  service trace failure).
- Uncaught exceptions inside ``call_next`` are recorded on the span
  (``record_exception`` + status ERROR) and re-raised so FastAPI's normal
  exception handlers still run.
"""

from __future__ import annotations

from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request
from starlette.responses import Response


class OTelInboundMiddleware(BaseHTTPMiddleware):
    """Open one server span per inbound HTTP request.

    Reads ``traceparent`` / ``tracestate`` headers (W3C Trace Context),
    starts a server-kind span keyed by ``"{method} {path}"``, attaches
    the OTel context, and exposes the active traceparent on
    ``request.state.traceparent`` so the proxy layer can forward it
    explicitly without rebuilding it from the OTel context.

    No-op safety
    ------------
    If ``init_tracing`` was never called, the global provider is the
    SDK default (``trace.NoOpTracerProvider``) and the span is a
    ``NonRecordingSpan``. The middleware still runs; it just produces
    no exported spans. This is the dev-no-collector path.
    """

    async def dispatch(self, request: Request, call_next) -> Response:
        # Local imports — keep cold-start fast for services that don't
        # use OTel at all (none today, but future-proof).
        from opentelemetry import trace
        from opentelemetry.propagate import extract, inject
        from opentelemetry.trace import SpanKind, Status, StatusCode

        # Extract upstream W3C Trace Context. ``extract`` reads from a
        # mapping (not a Request) so we hand it the headers dict.
        # Starlette gives us case-insensitive header access; the OTel
        # propagator handles its own case folding.
        ctx = extract(dict(request.headers))

        tracer = trace.get_tracer("ai_gateway_core.tracing")
        span_name = f"{request.method} {request.url.path}"

        with tracer.start_as_current_span(
            span_name,
            context=ctx,
            kind=SpanKind.SERVER,
        ) as span:
            # Inbound HTTP attributes. We use the legacy ``http.*``
            # names (still supported by Tempo / Jaeger / Honeycomb) for
            # broad collector compatibility. Newer ``url.path`` /
            # ``http.request.method`` semconv works too, but the legacy
            # set is what most operator dashboards key off.
            try:
                span.set_attribute("http.method", request.method)
                span.set_attribute("http.url", str(request.url))
                span.set_attribute("http.target", request.url.path)
                span.set_attribute("http.scheme", request.url.scheme)
                ua = request.headers.get("user-agent")
                if ua:
                    span.set_attribute("http.user_agent", ua)
            except Exception:  # noqa: BLE001
                # Attribute setting must not break the request path.
                pass

            # Cross-link to the existing X-Request-Id system. When
            # RequestIDMiddleware runs BEFORE this one, request.state
            # already has request_id; surface it on the span so log
            # correlation queries (request.id == "<id>") match traces.
            req_id = getattr(request.state, "request_id", None)
            if req_id:
                try:
                    span.set_attribute("request.id", req_id)
                except Exception:  # noqa: BLE001
                    pass

            # Tenant / user attributes — only set when an upstream auth
            # middleware has already populated request.state.user_info.
            user_info = getattr(request.state, "user_info", None)
            if user_info is not None:
                try:
                    tenant_id = getattr(user_info, "tenant_id", None) or (
                        user_info.get("tenant_id") if isinstance(user_info, dict) else None
                    )
                    user_id = getattr(user_info, "user_id", None) or (
                        user_info.get("user_id") if isinstance(user_info, dict) else None
                    )
                    if tenant_id:
                        span.set_attribute("tenant.id", str(tenant_id))
                    if user_id:
                        span.set_attribute("user.id", str(user_id))
                except Exception:  # noqa: BLE001
                    pass

            # Inject the active context into a carrier so downstream
            # proxy code (ServiceProxy._build_headers) can grab the
            # traceparent without re-extracting from OTel itself. We
            # write to request.state so the proxy reads it from the
            # same place it reads request.state.request_id.
            carrier: dict[str, str] = {}
            try:
                inject(carrier)
            except Exception:  # noqa: BLE001
                pass
            tp = carrier.get("traceparent")
            ts = carrier.get("tracestate")
            if tp:
                request.state.traceparent = tp
            if ts:
                request.state.tracestate = ts

            try:
                response = await call_next(request)
            except Exception as exc:
                # Record the exception, mark span ERROR, re-raise so
                # FastAPI / Starlette exception handlers still produce
                # the right HTTP response.
                try:
                    span.record_exception(exc)
                    span.set_status(Status(StatusCode.ERROR, str(exc)))
                except Exception:  # noqa: BLE001
                    pass
                raise

            try:
                span.set_attribute("http.status_code", response.status_code)
                if response.status_code >= 500:
                    span.set_status(Status(StatusCode.ERROR))
                else:
                    span.set_status(Status(StatusCode.OK))
            except Exception:  # noqa: BLE001
                pass

            return response


__all__ = ["OTelInboundMiddleware"]
