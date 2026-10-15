"""Direct coverage for the live client-IP resolver (src/core/client_ip.py).

The resolver previously had no dedicated test of its own; its behavior was
exercised only through the now-removed ``AuthMiddleware._get_client_ip``.
These tests pin the trusted-proxy contract directly.
"""

from __future__ import annotations

from src.core.client_ip import get_client_ip

_TRUSTED = ["127.0.0.0/8", "::1/128"]


def test_untrusted_direct_client_ignores_forwarded_for() -> None:
    """A non-trusted peer cannot spoof X-Forwarded-For."""
    ip = get_client_ip(
        {"X-Forwarded-For": "10.0.0.1, 10.0.0.2"},
        "192.168.1.1",
        trusted_proxy_cidrs=_TRUSTED,
    )
    assert ip == "192.168.1.1"


def test_untrusted_direct_client_ignores_real_ip() -> None:
    """A non-trusted peer cannot spoof X-Real-IP."""
    ip = get_client_ip(
        {"X-Real-IP": "10.0.0.5"},
        "192.168.1.1",
        trusted_proxy_cidrs=_TRUSTED,
    )
    assert ip == "192.168.1.1"


def test_trusted_proxy_honors_rightmost_untrusted_forwarded() -> None:
    """A trusted proxy's X-Forwarded-For yields the rightmost untrusted hop."""
    ip = get_client_ip(
        {"X-Forwarded-For": "10.0.0.1, 10.0.0.2"},
        "127.0.0.1",
        trusted_proxy_cidrs=_TRUSTED,
    )
    assert ip == "10.0.0.2"


def test_trusted_proxy_honors_real_ip() -> None:
    ip = get_client_ip(
        {"X-Real-IP": "10.0.0.5"},
        "127.0.0.1",
        trusted_proxy_cidrs=_TRUSTED,
    )
    assert ip == "10.0.0.5"


def test_fallback_to_direct_client() -> None:
    ip = get_client_ip({}, "192.168.1.1", trusted_proxy_cidrs=_TRUSTED)
    assert ip == "192.168.1.1"


def test_missing_direct_host_falls_back_to_unknown() -> None:
    assert get_client_ip({}, None, trusted_proxy_cidrs=_TRUSTED) == "unknown"
