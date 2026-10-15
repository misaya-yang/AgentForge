"""Idempotent OpenTelemetry SDK bootstrap for the gateway + microservices.

Each service calls ``init_tracing(service_name)`` exactly once during
startup (lifespan / startup event). Subsequent calls are no-ops so test
runs that import multiple services into one process don't end up with a
zoo of nested ``TracerProvider`` instances.

Endpoint resolution
-------------------
The OTLP/gRPC endpoint comes from (in priority order):

1. The ``otlp_endpoint`` parameter (overrides env entirely).
2. The ``OTEL_EXPORTER_OTLP_ENDPOINT`` environment variable.

If both are unset OR equal to the empty string, ``init_tracing`` runs the
"no-op exporter" path — it still installs a ``TracerProvider`` with the
right service-name resource so ``opentelemetry.trace.get_tracer(...)``
yields working tracers (and downstream code that calls
``inject(traceparent_carrier)`` keeps producing a valid traceparent
header). Spans are recorded in-process and dropped at end-of-span; this
is what tests, local dev, and unmonitored deploys want.

Sampler
-------
Top-level sampler is ``ParentBasedTraceIdRatio(sampler_ratio)`` so:

- A request that arrives WITH a sampled traceparent stays sampled.
- A request that arrives unsampled stays unsampled.
- A root span (no inbound traceparent) samples at ``sampler_ratio``.

Default 1.0 keeps dev/test predictable; production should turn this down
in the env (``OTEL_TRACES_SAMPLER_ARG``) or via the optional argument.
"""

from __future__ import annotations

import logging
import os
import threading
from typing import Optional

logger = logging.getLogger(__name__)

# Module-level guard so ``init_tracing`` is safe to call multiple times.
# Two startup paths import this from different services in the test
# suite, and uvicorn ``--reload`` re-imports the module on file changes.
_INIT_LOCK = threading.Lock()
_INITIALIZED = False
_INITIALIZED_SERVICE_NAME: Optional[str] = None


def is_initialized() -> bool:
    """Return ``True`` if ``init_tracing`` has already been called.

    Tests use this to skip the OTel setup when they want to install their
    own ``InMemorySpanExporter`` against the global ``TracerProvider``.
    """
    return _INITIALIZED


def init_tracing(
    service_name: str,
    *,
    otlp_endpoint: Optional[str] = None,
    sampler_ratio: float = 1.0,
    resource_attrs: Optional[dict[str, str]] = None,
) -> None:
    """Configure the global OTel ``TracerProvider`` for this service.

    Parameters
    ----------
    service_name
        Logical service name. Becomes ``service.name`` resource attribute,
        which all collectors / Tempo / Jaeger / Honeycomb use to group
        spans into the right "service" in their UIs.
    otlp_endpoint
        Optional OTLP/gRPC endpoint (e.g. ``http://otel-collector:4317``).
        When ``None`` falls back to ``OTEL_EXPORTER_OTLP_ENDPOINT``. When
        both are unset OR empty string, no exporter is installed (spans
        are still recorded in-process — useful for tests).
    sampler_ratio
        Root-span sampling ratio for ``ParentBasedTraceIdRatio``. Default
        ``1.0`` (sample everything). Production should pass a smaller
        value, or rely on ``OTEL_TRACES_SAMPLER_ARG`` set in the env.
    resource_attrs
        Extra resource attributes merged on top of ``service.name``. Use
        for ``deployment.environment``, ``service.version`` etc.

    Idempotent
    ----------
    Safe to call multiple times. The second call logs a debug message
    and returns without touching the provider — important because
    pytest collects multiple service apps into one process.
    """
    global _INITIALIZED, _INITIALIZED_SERVICE_NAME

    if _INITIALIZED:
        if _INITIALIZED_SERVICE_NAME != service_name:
            logger.debug(
                "init_tracing called again with service_name=%s "
                "(already initialized as %s); ignoring",
                service_name,
                _INITIALIZED_SERVICE_NAME,
            )
        return

    with _INIT_LOCK:
        if _INITIALIZED:
            return

        # Local imports keep the package's import-time cost zero when a
        # downstream consumer never calls ``init_tracing`` (e.g. a
        # library test that just constructs a ServiceProxy).
        from opentelemetry import trace
        from opentelemetry.sdk.resources import Resource, SERVICE_NAME
        from opentelemetry.sdk.trace import TracerProvider
        from opentelemetry.sdk.trace.sampling import ParentBased, TraceIdRatioBased

        attrs: dict[str, str] = {SERVICE_NAME: service_name}
        if resource_attrs:
            attrs.update(resource_attrs)
        resource = Resource.create(attrs)

        sampler = ParentBased(root=TraceIdRatioBased(sampler_ratio))
        provider = TracerProvider(resource=resource, sampler=sampler)

        # Resolve endpoint: explicit arg overrides env. Both empty/None
        # → "no-op" mode (provider installed, no exporter attached).
        endpoint = otlp_endpoint
        if endpoint is None:
            endpoint = os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT", "")

        if endpoint:
            try:
                from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import (
                    OTLPSpanExporter,
                )
                from opentelemetry.sdk.trace.export import BatchSpanProcessor

                exporter = OTLPSpanExporter(endpoint=endpoint)
                processor = BatchSpanProcessor(exporter)
                provider.add_span_processor(processor)
                logger.info(
                    "OTel tracing initialized: service=%s endpoint=%s sampler_ratio=%s",
                    service_name,
                    endpoint,
                    sampler_ratio,
                )
            except Exception as exc:  # noqa: BLE001
                # Don't crash the service if the exporter URL is bad; a
                # broken collector should not take down the gateway.
                logger.warning(
                    "OTel exporter init failed (service=%s endpoint=%s): %s. "
                    "Falling back to in-process spans.",
                    service_name,
                    endpoint,
                    exc,
                )
        else:
            logger.info(
                "OTel tracing initialized in NO-OP mode: service=%s "
                "(OTEL_EXPORTER_OTLP_ENDPOINT unset; spans stay in-process)",
                service_name,
            )

        trace.set_tracer_provider(provider)

        # Auto-instrument asyncpg so every DB query gets its own client
        # span. Done here (after provider is set) so the instrumentor
        # picks up the right tracer. Wrapped in try/except so a missing
        # asyncpg in some service container doesn't take everything down.
        try:
            from opentelemetry.instrumentation.asyncpg import AsyncPGInstrumentor

            AsyncPGInstrumentor().instrument()
        except Exception as exc:  # noqa: BLE001
            logger.debug("asyncpg instrumentation skipped: %s", exc)

        _INITIALIZED = True
        _INITIALIZED_SERVICE_NAME = service_name


def _reset_for_tests() -> None:
    """Reset module state — for unit tests only.

    Production code never calls this. Tests use it to verify that the
    second call to ``init_tracing`` is a no-op AND to set up a fresh
    provider per test for assertion isolation.

    Also resets the OTel SDK's global "tracer provider already set"
    flag so the next ``init_tracing`` call actually installs a new
    provider (otherwise the SDK refuses with
    "Overriding of current TracerProvider is not allowed").
    """
    global _INITIALIZED, _INITIALIZED_SERVICE_NAME
    with _INIT_LOCK:
        _INITIALIZED = False
        _INITIALIZED_SERVICE_NAME = None

    # Reach into the OTel SDK's "set once" guard so the next call to
    # ``trace.set_tracer_provider`` actually replaces the provider.
    # Production code never hits this branch. The attribute names are
    # stable across recent OTel SDK releases (1.20+).
    try:
        from opentelemetry import trace

        if hasattr(trace, "_TRACER_PROVIDER_SET_ONCE"):
            trace._TRACER_PROVIDER_SET_ONCE._done = False  # type: ignore[attr-defined]
        if hasattr(trace, "_TRACER_PROVIDER"):
            trace._TRACER_PROVIDER = None  # type: ignore[attr-defined]
    except Exception:  # noqa: BLE001
        pass

    # Also un-instrument the asyncpg patch so a second init_tracing
    # call doesn't double-patch (which the instrumentor warns about).
    try:
        from opentelemetry.instrumentation.asyncpg import AsyncPGInstrumentor

        instrumentor = AsyncPGInstrumentor()
        if instrumentor.is_instrumented_by_opentelemetry:
            instrumentor.uninstrument()
    except Exception:  # noqa: BLE001
        pass
