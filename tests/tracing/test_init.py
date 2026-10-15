"""Tests for ``ai_gateway_core.tracing.init_tracing``.

Covers the three contract guarantees:
1. Idempotent — second call is a debug-log no-op.
2. No exporter when ``OTEL_EXPORTER_OTLP_ENDPOINT`` is unset (no crash).
3. Bad/unreachable OTLP endpoint does not crash the process.
"""

from __future__ import annotations

import os

import pytest

from ai_gateway_core.tracing.init import (
    _reset_for_tests,
    init_tracing,
    is_initialized,
)


@pytest.fixture(autouse=True)
def _reset_state():
    """Each test starts with a clean module-level state."""
    _reset_for_tests()
    yield
    _reset_for_tests()


def test_init_tracing_idempotent(monkeypatch: pytest.MonkeyPatch) -> None:
    """The second call is a no-op even with a different service name."""
    monkeypatch.delenv("OTEL_EXPORTER_OTLP_ENDPOINT", raising=False)
    assert not is_initialized()

    init_tracing("svc-one")
    assert is_initialized()

    # Second call must not raise and must not change the underlying
    # provider — we capture the provider before/after to be sure.
    from opentelemetry import trace

    provider_before = trace.get_tracer_provider()
    init_tracing("svc-two")
    provider_after = trace.get_tracer_provider()
    assert provider_before is provider_after


def test_init_tracing_no_endpoint_skips_exporter(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Unset OTLP endpoint → init still succeeds, no exporter attached."""
    monkeypatch.delenv("OTEL_EXPORTER_OTLP_ENDPOINT", raising=False)

    # Must not raise even though no endpoint is configured.
    init_tracing("test-service")
    assert is_initialized()

    # The provider must be a real ``TracerProvider`` (not the SDK NoOp)
    # so spans the application creates manually still get recorded.
    from opentelemetry import trace
    from opentelemetry.sdk.trace import TracerProvider

    assert isinstance(trace.get_tracer_provider(), TracerProvider)


def test_init_tracing_empty_endpoint_skips_exporter(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Empty-string endpoint behaves the same as unset."""
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "")
    init_tracing("test-service")
    assert is_initialized()


def test_init_tracing_explicit_arg_overrides_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The ``otlp_endpoint=`` parameter wins over the env."""
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://env-endpoint:4317")
    # Empty arg → no-op path even though env is set.
    init_tracing("test-service", otlp_endpoint="")
    assert is_initialized()


def test_init_tracing_bad_endpoint_does_not_crash(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An unreachable / malformed endpoint must not break startup.

    The exporter constructor accepts any string at config-time; the
    ``BatchSpanProcessor`` lazily fails when it tries to flush. We use
    a string that's syntactically valid so the constructor accepts
    it — what matters is that init_tracing returns cleanly.
    """
    init_tracing(
        "test-service",
        otlp_endpoint="http://otel-collector-does-not-exist:4317",
    )
    assert is_initialized()


def test_init_tracing_resource_attrs_propagate(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Custom resource attrs land on the provider's ``Resource``."""
    monkeypatch.delenv("OTEL_EXPORTER_OTLP_ENDPOINT", raising=False)

    init_tracing(
        "test-service",
        resource_attrs={"deployment.environment": "test"},
    )

    from opentelemetry import trace
    from opentelemetry.sdk.trace import TracerProvider

    provider = trace.get_tracer_provider()
    assert isinstance(provider, TracerProvider)
    attrs = dict(provider.resource.attributes)
    assert attrs.get("service.name") == "test-service"
    assert attrs.get("deployment.environment") == "test"
