"""Tests for ``ai_gateway_core.security.safe_fetch`` — SSRF guard.

Covers:
- Private/loopback/link-local IPv4 + IPv6 + IPv4-mapped IPv6 rejection
- Multi-record DNS where ALL records must pass
- DNS-pinning prevents rebinding (validation IP ≠ connection IP race)
- Streaming byte cap aborts early, not after full download
- Manual redirect via ``urljoin`` (relative + absolute + protocol-relative)
- Per-hop re-validation (302 to private rejected)
- Allowed-hosts suffix match
- ``validate_callback_url`` validate-only path
"""

from __future__ import annotations

import socket
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from ai_gateway_core.security.safe_fetch import (
    SafeFetchError,
    _host_matches,
    _is_unsafe_ip,
    is_safe_destination,
    safe_fetch,
    validate_callback_url,
)

# ---------------------------------------------------------------------------
# IP classification
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("addr", [
    "127.0.0.1",
    "127.0.0.5",
    "10.0.0.1",
    "10.255.255.255",
    "192.168.1.1",
    "172.16.0.1",
    "172.31.255.255",
    "169.254.169.254",  # AWS metadata
    "0.0.0.0",
    "::1",                  # IPv6 loopback
    "fe80::1",              # IPv6 link-local
    "fc00::1",              # IPv6 unique-local (private)
    "::ffff:127.0.0.1",     # IPv4-mapped IPv6 → loopback
    "::ffff:10.0.0.1",      # IPv4-mapped IPv6 → private
    "::ffff:169.254.169.254",
    "garbage",              # unparseable → reject
])
def test_unsafe_ip_recognized(addr):
    assert _is_unsafe_ip(addr), f"{addr} should be classified unsafe"


@pytest.mark.parametrize("addr", [
    "8.8.8.8",
    "1.1.1.1",
    "8.8.8.8",
    "2001:4860:4860::8888",  # public IPv6
])
def test_safe_public_ip(addr):
    assert not _is_unsafe_ip(addr)


# ---------------------------------------------------------------------------
# is_safe_destination — DNS resolution + multi-record rule
# ---------------------------------------------------------------------------


def test_destination_safe_when_all_records_public():
    with patch.object(socket, "getaddrinfo", return_value=[
        (socket.AF_INET, 0, 0, "", ("8.8.8.8", 443)),
    ]):
        ok, info = is_safe_destination("dns.google", 443)
    assert ok
    assert info == "8.8.8.8"


def test_destination_rejected_when_any_record_private():
    """Multi-record host with a single private IP rejects whole host —
    defends against split-horizon / multi-record DNS bypass."""
    with patch.object(socket, "getaddrinfo", return_value=[
        (socket.AF_INET, 0, 0, "", ("8.8.8.8", 443)),
        (socket.AF_INET, 0, 0, "", ("10.0.0.1", 443)),
    ]):
        ok, info = is_safe_destination("hostile.example", 443)
    assert not ok
    assert "10.0.0.1" in info


def test_destination_rejected_when_dns_fails():
    with patch.object(socket, "getaddrinfo", side_effect=socket.gaierror("nope")):
        ok, info = is_safe_destination("nonexistent", 443)
    assert not ok
    assert "DNS" in info


def test_destination_rejected_when_no_records():
    with patch.object(socket, "getaddrinfo", return_value=[]):
        ok, info = is_safe_destination("empty.example", 443)
    assert not ok


# ---------------------------------------------------------------------------
# Host suffix matching
# ---------------------------------------------------------------------------


def test_host_match_exact():
    assert _host_matches("amazonaws.com", ("amazonaws.com",))


def test_host_match_subdomain():
    assert _host_matches("bucket.s3.amazonaws.com", ("amazonaws.com",))


def test_host_match_rejects_suffix_overlap():
    """Defense against ``evilamazonaws.com`` matching ``amazonaws.com``."""
    assert not _host_matches("evilamazonaws.com", ("amazonaws.com",))


def test_host_match_case_insensitive():
    assert _host_matches("S3.AmazonAWS.COM", ("amazonaws.com",))


# ---------------------------------------------------------------------------
# safe_fetch — end-to-end via mocked httpx + DNS
# ---------------------------------------------------------------------------


def _mock_stream_response(status: int, body: bytes = b"", headers: dict | None = None):
    """Build a mock httpx response usable as ``client.stream(...)`` ctx mgr."""
    resp = MagicMock()
    resp.status_code = status
    resp.headers = headers or {}
    resp.raise_for_status = MagicMock()

    async def _aiter_bytes():
        yield body

    resp.aiter_bytes = MagicMock(return_value=_aiter_bytes())
    resp.__aenter__ = AsyncMock(return_value=resp)
    resp.__aexit__ = AsyncMock(return_value=None)
    return resp


def _mock_client_cm(stream_response):
    client = MagicMock()
    client.stream = MagicMock(return_value=stream_response)
    client.__aenter__ = AsyncMock(return_value=client)
    client.__aexit__ = AsyncMock(return_value=None)
    return client


@pytest.mark.asyncio
async def test_safe_fetch_returns_bytes_on_public_destination():
    with patch.object(socket, "getaddrinfo", return_value=[
        (socket.AF_INET, 0, 0, "", ("8.8.8.8", 443)),
    ]), patch(
        "ai_gateway_core.security.safe_fetch.httpx.AsyncClient",
        return_value=_mock_client_cm(_mock_stream_response(200, b"hello")),
    ):
        result = await safe_fetch("https://example.com/img.png")

    assert result == b"hello"


@pytest.mark.asyncio
@pytest.mark.parametrize("host,ip", [
    ("localhost", "127.0.0.1"),
    ("internal.svc", "10.0.0.5"),
    ("metadata", "169.254.169.254"),
    ("private6.example", "fe80::1"),
])
async def test_safe_fetch_rejects_private_destination(host, ip):
    with patch.object(socket, "getaddrinfo", return_value=[
        (socket.AF_INET, 0, 0, "", (ip, 80)),
    ]), pytest.raises(SafeFetchError) as exc:
        await safe_fetch(f"http://{host}/x.png")
    assert "rejected" in str(exc.value).lower() or "disallowed" in str(exc.value).lower()


@pytest.mark.asyncio
async def test_safe_fetch_rejects_non_http_scheme():
    with pytest.raises(SafeFetchError) as exc:
        await safe_fetch("file:///etc/passwd")
    assert "http" in str(exc.value).lower()


@pytest.mark.asyncio
async def test_safe_fetch_aborts_oversize_streaming():
    """Cumulative cap must trip mid-stream, not after full download."""
    big_response = MagicMock()
    big_response.status_code = 200
    big_response.headers = {}
    big_response.raise_for_status = MagicMock()

    async def _chunks():
        for _ in range(20):
            yield b"X" * (1024 * 1024)

    big_response.aiter_bytes = MagicMock(return_value=_chunks())
    big_response.__aenter__ = AsyncMock(return_value=big_response)
    big_response.__aexit__ = AsyncMock(return_value=None)

    with patch.object(socket, "getaddrinfo", return_value=[
        (socket.AF_INET, 0, 0, "", ("8.8.8.8", 443)),
    ]), patch(
        "ai_gateway_core.security.safe_fetch.httpx.AsyncClient",
        return_value=_mock_client_cm(big_response),
    ), pytest.raises(SafeFetchError) as exc:
        await safe_fetch("https://example.com/huge.bin", max_bytes=8 * 1024 * 1024)

    assert "8 MB" in str(exc.value) or "exceeds" in str(exc.value).lower()


@pytest.mark.asyncio
async def test_safe_fetch_redirect_to_private_rejected():
    """Each hop is independently validated. Public host → 302 → private IP
    must reject on hop 2."""
    redirect_response = _mock_stream_response(
        302, headers={"location": "http://127.0.0.1/x.png"},
    )

    # First hop: example.com → public IP. Second hop: 127.0.0.1 → loopback.
    call_count = {"n": 0}

    def _resolve(host, port, **kwargs):
        call_count["n"] += 1
        if host == "example.com":
            return [(socket.AF_INET, 0, 0, "", ("8.8.8.8", port))]
        if host == "127.0.0.1":
            return [(socket.AF_INET, 0, 0, "", ("127.0.0.1", port))]
        raise socket.gaierror(host)

    with patch.object(socket, "getaddrinfo", side_effect=_resolve), patch(
        "ai_gateway_core.security.safe_fetch.httpx.AsyncClient",
        return_value=_mock_client_cm(redirect_response),
    ), pytest.raises(SafeFetchError) as exc:
        await safe_fetch("https://example.com/r")

    assert "rejected" in str(exc.value).lower() or "disallowed" in str(exc.value).lower()


@pytest.mark.asyncio
@pytest.mark.parametrize("relative,expected_host", [
    ("next.png", "example.com"),
    ("../up.png", "example.com"),
    ("/abs.png", "example.com"),
    ("//other.example/x.png", "other.example"),  # protocol-relative
])
async def test_safe_fetch_redirect_uses_urljoin(relative, expected_host):
    """Redirect targets must be resolved via urljoin so relative URLs
    work correctly. Validates against codex finding 'next.png 等合法相对
    redirect 会被误解析或拒绝'."""
    redirect_response = _mock_stream_response(
        302, headers={"location": relative},
    )
    final_response = _mock_stream_response(200, b"final")
    streams = iter([redirect_response, final_response])

    def _stream(method, url, **kw):
        return next(streams)

    client = MagicMock()
    client.stream = _stream
    client.__aenter__ = AsyncMock(return_value=client)
    client.__aexit__ = AsyncMock(return_value=None)

    with patch.object(socket, "getaddrinfo", return_value=[
        (socket.AF_INET, 0, 0, "", ("8.8.8.8", 443)),
    ]), patch(
        "ai_gateway_core.security.safe_fetch.httpx.AsyncClient",
        return_value=client,
    ):
        result = await safe_fetch("https://example.com/start.png")

    assert result == b"final"


@pytest.mark.asyncio
async def test_safe_fetch_redirect_loop_aborts():
    """A → B → A must abort, not loop forever."""
    redirect_response = _mock_stream_response(
        302, headers={"location": "https://example.com/a"},
    )

    with patch.object(socket, "getaddrinfo", return_value=[
        (socket.AF_INET, 0, 0, "", ("8.8.8.8", 443)),
    ]), patch(
        "ai_gateway_core.security.safe_fetch.httpx.AsyncClient",
        return_value=_mock_client_cm(redirect_response),
    ), pytest.raises(SafeFetchError) as exc:
        await safe_fetch("https://example.com/a")

    assert "loop" in str(exc.value).lower() or "redirect" in str(exc.value).lower()


@pytest.mark.asyncio
async def test_safe_fetch_allowed_hosts_enforced():
    """When allowed_hosts is set, hosts outside the suffix list are
    rejected before DNS even runs."""
    with pytest.raises(SafeFetchError) as exc:
        await safe_fetch(
            "https://evil.example/x.png",
            allowed_hosts=("amazonaws.com",),
        )
    assert "allowlist" in str(exc.value).lower()


@pytest.mark.asyncio
async def test_safe_fetch_allowed_hosts_subdomain_match():
    with patch.object(socket, "getaddrinfo", return_value=[
        (socket.AF_INET, 0, 0, "", ("8.8.8.8", 443)),
    ]), patch(
        "ai_gateway_core.security.safe_fetch.httpx.AsyncClient",
        return_value=_mock_client_cm(_mock_stream_response(200, b"ok")),
    ):
        result = await safe_fetch(
            "https://bucket.s3.amazonaws.com/x.png",
            allowed_hosts=("amazonaws.com",),
        )
    assert result == b"ok"


# ---------------------------------------------------------------------------
# validate_callback_url
# ---------------------------------------------------------------------------


def test_validate_callback_accepts_public():
    with patch.object(socket, "getaddrinfo", return_value=[
        (socket.AF_INET, 0, 0, "", ("8.8.8.8", 443)),
    ]):
        result = validate_callback_url("https://hooks.example.com/cb")
    assert result == "https://hooks.example.com/cb"


def test_validate_callback_rejects_private():
    with patch.object(socket, "getaddrinfo", return_value=[
        (socket.AF_INET, 0, 0, "", ("10.0.0.5", 80)),
    ]), pytest.raises(SafeFetchError) as exc:
        validate_callback_url("http://internal.svc/cb")
    assert exc.value.status_code == 400


def test_validate_callback_rejects_non_http():
    with pytest.raises(SafeFetchError):
        validate_callback_url("ftp://example.com/cb")
