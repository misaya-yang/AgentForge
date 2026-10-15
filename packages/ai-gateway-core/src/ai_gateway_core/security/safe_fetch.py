"""SSRF-safe HTTP fetcher — shared by every callsite that takes a
user-controlled URL.

## Threat model

User passes a URL → server fetches it → bytes/effects flow back to user
(or to an attacker-chosen target). Without guards an attacker can:

- Hit AWS metadata (`169.254.169.254`) → IAM credential theft
- Reach internal services on private networks (`10.0.0.0/8`,
  `192.168.0.0/16`, `172.16.0.0/12`)
- Reach localhost / loopback
- Use **DNS rebinding** to bypass naive prefix/IP checks: the same domain
  resolves to a public IP at validation time, then to a private IP at
  connection time
- Bypass via redirect (302 → http://10.x.x.x)
- Inflate downloads past any "max bytes" check that runs after full read

## Defense

- Resolve host once via `getaddrinfo`, **pin the safe IP**, connect via the
  pinned IP with the original hostname in the `Host` header (and SNI for
  HTTPS). This eliminates the DNS-rebinding race entirely — the connection
  uses the IP we validated.
- Reject any IP that's `is_private | is_loopback | is_link_local |
  is_reserved | is_multicast | is_unspecified`. Includes IPv6 link-local
  (`fe80::/10`) and IPv4-mapped IPv6 (`::ffff:0:0/96`).
- Multi-record hosts: **all** addresses must pass — one bad IP rejects the
  whole host. Defense against multi-record DNS where some records are
  public and some are private.
- Manual redirect: each hop independently re-resolved + re-validated.
  Use ``urljoin`` so relative redirects (``next.png``, ``../x.png``,
  ``//host/x``) parse correctly.
- Streaming with cumulative byte cap — abort at first chunk that crosses
  the limit, not after full download.

## Public API

- ``safe_fetch(url, *, max_bytes, max_redirects, timeout) -> bytes``:
  download with all guards. Use for GET-and-read-body flows (image
  reference, knowledge ingestion, etc.).
- ``validate_callback_url(url) -> None``: validate-only (no fetch). Use
  before posting to a user-supplied webhook. The actual POST then uses
  a normal httpx client with ``follow_redirects=False`` so redirects
  can't escape the validated host.
- ``is_safe_destination(host, port) -> tuple[bool, str | None]``: low-level
  check used internally; exposed for tests.
- ``SafeFetchError``: raised on any guard violation. ``status_code`` field
  carries the HTTP status appropriate for the wrapping API (default 400).
"""

from __future__ import annotations

import dataclasses
import ipaddress
import logging
import os
import socket
import ssl
from collections.abc import Mapping
from typing import Any
from urllib.parse import urljoin, urlparse

import httpx

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Defaults — tuned for image fetching. KS / callback callsites can override.
# ---------------------------------------------------------------------------

DEFAULT_MAX_BYTES = 8 * 1024 * 1024      # 8 MB
DEFAULT_MAX_REDIRECTS = 3
DEFAULT_TIMEOUT_SECONDS = 30.0


# ---------------------------------------------------------------------------
# Errors
# ---------------------------------------------------------------------------


class SafeFetchError(Exception):
    """Raised on SSRF / size / scheme / redirect violations.

    ``status_code`` is the HTTP status the caller should return (most
    callers want 400 for client errors, occasionally 403 for explicit
    deny). Default 400.
    """

    def __init__(self, message: str, *, status_code: int = 400) -> None:
        super().__init__(message)
        self.status_code = status_code


# ---------------------------------------------------------------------------
# Destination validation
# ---------------------------------------------------------------------------


def _is_unsafe_ip(addr: str) -> bool:
    """Reject any address in private/loopback/link-local/reserved/multicast/
    unspecified. Also unwraps IPv4-mapped IPv6 (``::ffff:1.2.3.4``) to check
    the underlying IPv4 — a common bypass."""
    try:
        ip = ipaddress.ip_address(addr)
    except ValueError:
        # Unparseable — refuse
        return True

    # IPv4-mapped IPv6 → check the embedded v4
    if isinstance(ip, ipaddress.IPv6Address) and ip.ipv4_mapped is not None:
        ip = ip.ipv4_mapped

    return (
        ip.is_private
        or ip.is_loopback
        or ip.is_link_local
        or ip.is_reserved
        or ip.is_multicast
        or ip.is_unspecified
    )


def is_safe_destination(host: str, port: int) -> tuple[bool, str | None]:
    """Resolve ``host`` and confirm every returned address is public/routable.

    Returns ``(ok, reason_or_pinned_ip)``: when ok=True the second element
    is the first safe IP to pin to. When ok=False it's a human-readable
    reason. Multi-record hosts must have ALL addresses safe.
    """
    if not host:
        return False, "missing host"
    try:
        infos = socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)
    except socket.gaierror as e:
        return False, f"DNS resolution failed: {e}"
    if not infos:
        return False, "DNS returned no addresses"

    pinned: str | None = None
    for info in infos:
        addr = info[4][0]
        if _is_unsafe_ip(addr):
            return False, f"{host} resolves to disallowed address {addr}"
        if pinned is None:
            pinned = addr
    return True, pinned


def _split_url(url: str) -> tuple[str, str, int, str]:
    """Returns ``(scheme, host, port, full_url)`` after scheme + host
    validation. Raises ``SafeFetchError`` on bad URLs."""
    parsed = urlparse(url)
    if parsed.scheme not in ("http", "https"):
        raise SafeFetchError("URL must be http(s)")
    host = parsed.hostname
    if not host:
        raise SafeFetchError("URL missing host")
    port = parsed.port or (443 if parsed.scheme == "https" else 80)
    return parsed.scheme, host, port, url


# ---------------------------------------------------------------------------
# DNS-pinned transport
# ---------------------------------------------------------------------------


class _DNSPinnedTransport(httpx.AsyncHTTPTransport):
    """Custom httpx transport that connects to a fixed (pinned) IP regardless
    of DNS state at connection time.

    The class is constructed once per fetch with a single ``(host, ip)``
    binding. Httpx's resolver is bypassed via the ``local_address`` mechanism
    isn't quite right — instead we patch ``socket.getaddrinfo`` for the
    lifetime of the request scoped through a hook.

    Implementation: subclass overrides ``handle_async_request`` to swap the
    request's URL host with the pinned IP, while preserving the original
    hostname in the ``Host`` header (and the SNI is set via
    ``server_hostname`` in the SSL context the underlying connection pool
    uses for HTTPS — httpx reads it from the URL host by default, so we
    pass it explicitly).
    """

    def __init__(self, pinned_host: str, pinned_ip: str, **kwargs):
        # Use a custom SSL context that verifies cert against the original
        # hostname, not the IP we connected to.
        verify = kwargs.pop("verify", True)
        if verify is True:
            ssl_context = ssl.create_default_context()
        elif verify is False:
            ssl_context = ssl.create_default_context()
            ssl_context.check_hostname = False
            ssl_context.verify_mode = ssl.CERT_NONE
        else:
            ssl_context = verify  # caller supplied SSLContext
        super().__init__(verify=ssl_context, **kwargs)
        self._pinned_host = pinned_host
        self._pinned_ip = pinned_ip
        self._is_v6 = ":" in pinned_ip

    async def handle_async_request(self, request: httpx.Request) -> httpx.Response:
        # Swap URL host with the pinned IP so socket connect uses our
        # validated address. Keep Host header set to the original hostname
        # so virtual-host routing + cert validation still work.
        original_host = request.url.host
        new_host = f"[{self._pinned_ip}]" if self._is_v6 else self._pinned_ip
        request.url = request.url.copy_with(host=new_host)
        request.headers["Host"] = original_host
        # SNI on HTTPS: httpx derives server_hostname from the URL host,
        # which we just set to the IP. We override by setting the
        # ``X-SNI-Host`` extension... but httpx doesn't read that.
        # Workaround: use ``request.extensions["sni_hostname"]`` (httpx 0.27+).
        request.extensions = dict(request.extensions or {})
        request.extensions["sni_hostname"] = original_host
        return await super().handle_async_request(request)


# ---------------------------------------------------------------------------
# safe_fetch — the main entry point
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class SafeFetchResponse:
    """Rich result from ``safe_fetch_with_response``.

    Use this when the caller needs more than just bytes — e.g. ``web_fetch``
    tool needs the final URL after redirects, the response status code, and
    the content-type header to decide HTML vs JSON vs text extraction.
    """

    body: bytes
    final_url: str
    status_code: int
    headers: dict[str, str]
    encoding: str | None = None

    @property
    def content_type(self) -> str:
        return self.headers.get("content-type", "") or ""


async def safe_fetch(
    url: str,
    *,
    max_bytes: int = DEFAULT_MAX_BYTES,
    max_redirects: int = DEFAULT_MAX_REDIRECTS,
    timeout: float = DEFAULT_TIMEOUT_SECONDS,
    allowed_hosts: tuple[str, ...] | None = None,
) -> bytes:
    """Fetch ``url`` body bytes with full SSRF + size guards.

    - ``max_bytes`` — abort streaming once cumulative bytes cross this
    - ``max_redirects`` — at most this many manual hops; each re-validated
    - ``timeout`` — wall-clock cap for the whole transaction
    - ``allowed_hosts`` — optional hostname-suffix allowlist (e.g.
      ``("amazonaws.com", "googleapis.com")``). When set, **every hop** must
      end with one of these suffixes. ``None`` means no host allowlist —
      validation is purely IP-based via DNS resolution.

    Raises ``SafeFetchError`` on any violation.

    For callers needing the final URL / status / headers (e.g. content-type
    sniffing), use :func:`safe_fetch_with_response` instead.
    """
    response = await safe_fetch_with_response(
        url,
        max_bytes=max_bytes,
        max_redirects=max_redirects,
        timeout=timeout,
        allowed_hosts=allowed_hosts,
    )
    return response.body


async def safe_fetch_with_response(
    url: str,
    *,
    max_bytes: int = DEFAULT_MAX_BYTES,
    max_redirects: int = DEFAULT_MAX_REDIRECTS,
    timeout: float = DEFAULT_TIMEOUT_SECONDS,
    allowed_hosts: tuple[str, ...] | None = None,
) -> SafeFetchResponse:
    """Like ``safe_fetch`` but returns a ``SafeFetchResponse`` with the body,
    the final URL after manual redirects, status code, and headers."""
    if max_bytes <= 0:
        raise ValueError("max_bytes must be positive")

    current = url
    seen: set[str] = set()
    for _hop in range(max_redirects + 1):
        if current in seen:
            raise SafeFetchError("redirect loop detected")
        seen.add(current)

        scheme, host, port, _ = _split_url(current)

        if allowed_hosts is not None and not _host_matches(host, allowed_hosts):
            raise SafeFetchError(
                f"destination host {host!r} not in allowlist",
                status_code=400,
            )

        ok, info = is_safe_destination(host, port)
        if not ok:
            raise SafeFetchError(
                f"destination rejected: {info}",
                status_code=400,
            )
        pinned_ip = info  # safe IP picked by the validator

        transport = _DNSPinnedTransport(host, pinned_ip or host)
        try:
            async with httpx.AsyncClient(
                transport=transport,
                timeout=timeout,
                follow_redirects=False,
            ) as client, client.stream("GET", current) as resp:
                if 300 <= resp.status_code < 400:
                    location = resp.headers.get("location")
                    if not location:
                        raise SafeFetchError(
                            f"redirect {resp.status_code} without Location header"
                        )
                    current = urljoin(current, location)
                    continue

                resp.raise_for_status()
                buf = bytearray()
                async for chunk in resp.aiter_bytes():
                    buf.extend(chunk)
                    if len(buf) > max_bytes:
                        raise SafeFetchError(
                            f"payload exceeds {max_bytes // (1024 * 1024)} MB cap",
                            status_code=400,
                        )
                return SafeFetchResponse(
                    body=bytes(buf),
                    final_url=current,
                    status_code=resp.status_code,
                    headers=dict(resp.headers),
                    encoding=resp.encoding,
                )
        except httpx.HTTPError as exc:
            raise SafeFetchError(f"fetch failed: {exc}") from exc
        finally:
            await transport.aclose()

    raise SafeFetchError(f"exceeded {max_redirects} redirects")


def _host_matches(host: str, suffixes: tuple[str, ...]) -> bool:
    """Suffix match — ``foo.amazonaws.com`` matches suffix ``"amazonaws.com"``.
    Empty / mismatched returns False."""
    h = host.lower().rstrip(".")
    for suf in suffixes:
        s = suf.lower().lstrip(".")
        if h == s or h.endswith("." + s):
            return True
    return False


# ---------------------------------------------------------------------------
# validate_callback_url — for POST webhooks where we don't read the body
# ---------------------------------------------------------------------------


def _build_allowlist(
    explicit: tuple[str, ...] | None = None,
) -> tuple[str, ...]:
    """Build effective allowlist from explicit arg + CALLBACK_URL_ALLOWLIST env."""
    env_raw = os.environ.get("CALLBACK_URL_ALLOWLIST", "").strip()
    env_hosts = tuple(h.strip() for h in env_raw.split(",") if h.strip())
    if explicit:
        return explicit + env_hosts
    return env_hosts


def validate_callback_url(
    url: str,
    *,
    allowed_hosts: tuple[str, ...] | None = None,
) -> str:
    """Validate ``url`` is safe to POST to. Returns the validated URL
    (currently unchanged; reserved for future canonicalization).

    Use this **before** the POST. The POST itself should use
    ``follow_redirects=False`` and a short timeout so a hostile server
    can't redirect us into the private network mid-transaction.

    Whitelist: set ``CALLBACK_URL_ALLOWLIST`` env var to a comma-separated
    list of hosts (e.g. ``internal-api.example.com``). Hosts
    on the allowlist bypass the private/loopback check entirely.

    Note: this is validate-only, so DNS rebinding is still possible
    between this check and the actual POST. For full DNS-rebinding-safe
    POST, use the same DNS-pinned transport pattern as ``safe_fetch``.
    For our threat model (callbacks fire-and-forget, attacker would need
    to time the rebind perfectly), validate-only is acceptable; we keep
    the option open by returning a URL the caller can pass to httpx.
    """
    _, host, port, _ = _split_url(url)

    effective = _build_allowlist(allowed_hosts)
    # Host is on the allowlist → bypass all further checks
    if effective and _host_matches(host, effective):
        return url

    ok, info = is_safe_destination(host, port)
    if not ok:
        raise SafeFetchError(
            f"callback destination rejected: {info}",
            status_code=400,
        )
    return url


async def safe_callback_post(
    url: str,
    *,
    json: dict[str, Any],
    headers: Mapping[str, str] | None = None,
    timeout: float = 10.0,
    allowed_hosts: tuple[str, ...] | None = None,
    allow_env_allowlist: bool = True,
) -> httpx.Response:
    """POST JSON to a user-controlled callback URL with DNS-pinned transport.

    Closes the validate-then-post DNS rebinding race: the hostname is resolved
    and checked once, then the HTTP client connects to that validated IP while
    preserving the original Host header.
    """
    scheme, host, port, _ = _split_url(url)

    effective = _build_allowlist(allowed_hosts) if allow_env_allowlist else tuple(allowed_hosts or ())
    if effective and _host_matches(host, effective):
        async with httpx.AsyncClient(
            timeout=timeout,
            follow_redirects=False,
        ) as client:
            return await client.post(url, json=json, headers=headers)

    ok, pinned_ip = is_safe_destination(host, port)
    if not ok:
        raise SafeFetchError(
            f"callback destination rejected: {pinned_ip}",
            status_code=400,
        )

    transport = _DNSPinnedTransport(host, pinned_ip)
    async with httpx.AsyncClient(
        transport=transport,
        timeout=timeout,
        follow_redirects=False,
    ) as client:
        return await client.post(url, json=json, headers=headers)


async def safe_form_post(
    url: str,
    *,
    data: Mapping[str, Any],
    headers: Mapping[str, str] | None = None,
    timeout: float = 10.0,
) -> httpx.Response:
    """POST form data to a public HTTPS endpoint using a DNS-pinned transport.

    This is intended for OAuth token endpoints. Redirects are never followed,
    and the validated IP is the IP used for the connection, closing the DNS
    rebinding window between validation and request dispatch.
    """

    parsed = urlparse(url)
    try:
        parsed_port = parsed.port
    except ValueError as exc:
        raise SafeFetchError("form POST destination has an invalid port") from exc
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.fragment
    ):
        raise SafeFetchError(
            "form POST destination must be an https URL without userinfo or fragment",
            status_code=400,
        )
    _, host, port, _ = _split_url(url)
    if parsed_port is not None:
        port = parsed_port
    ok, pinned_ip = is_safe_destination(host, port)
    if not ok:
        raise SafeFetchError(
            f"form POST destination rejected: {pinned_ip}",
            status_code=400,
        )
    transport = _DNSPinnedTransport(host, pinned_ip or host)
    try:
        async with httpx.AsyncClient(
            transport=transport,
            timeout=timeout,
            follow_redirects=False,
        ) as client:
            return await client.post(url, data=data, headers=headers)
    except httpx.HTTPError as exc:
        raise SafeFetchError("form POST failed") from exc
    finally:
        await transport.aclose()
