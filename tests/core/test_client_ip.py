from __future__ import annotations

from src.core.client_ip import get_client_ip


def test_untrusted_peer_cannot_spoof_x_forwarded_for() -> None:
    headers = {"X-Forwarded-For": "198.51.100.9, 10.0.0.5", "X-Real-IP": "198.51.100.10"}

    assert get_client_ip(headers, direct_client_host="203.0.113.44") == "203.0.113.44"


def test_trusted_proxy_uses_rightmost_untrusted_forwarded_ip() -> None:
    headers = {"X-Forwarded-For": "198.51.100.9, 10.0.0.5"}

    assert (
        get_client_ip(
            headers,
            direct_client_host="127.0.0.1",
            trusted_proxy_cidrs=("127.0.0.0/8", "10.0.0.0/8"),
        )
        == "198.51.100.9"
    )


def test_trusted_proxy_can_use_x_real_ip_when_forwarded_for_missing() -> None:
    headers = {"X-Real-IP": "198.51.100.10"}

    assert get_client_ip(headers, direct_client_host="127.0.0.1") == "198.51.100.10"


def test_invalid_forwarded_values_fall_back_to_direct_peer() -> None:
    headers = {"X-Forwarded-For": "not-an-ip, also-bad"}

    assert get_client_ip(headers, direct_client_host="127.0.0.1") == "127.0.0.1"
