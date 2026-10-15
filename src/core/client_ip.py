from __future__ import annotations

import ipaddress
import os
from collections.abc import Mapping
from functools import lru_cache
from typing import Any

from fastapi import Request
from starlette.types import Scope

DEFAULT_TRUSTED_PROXY_CIDRS = ("127.0.0.0/8", "::1/128")


@lru_cache(maxsize=16)
def _trusted_networks(raw_cidrs: str) -> tuple[ipaddress._BaseNetwork, ...]:
    networks: list[ipaddress._BaseNetwork] = []
    for raw in raw_cidrs.split(","):
        cidr = raw.strip()
        if not cidr:
            continue
        try:
            networks.append(ipaddress.ip_network(cidr, strict=False))
        except ValueError:
            continue
    return tuple(networks)


def _configured_trusted_proxy_cidrs() -> str:
    configured = os.getenv("GATEWAY_TRUSTED_PROXY_CIDRS", "").strip()
    if configured:
        return configured
    return ",".join(DEFAULT_TRUSTED_PROXY_CIDRS)


def _parse_ip(value: str | None) -> ipaddress._BaseAddress | None:
    if not value:
        return None
    try:
        return ipaddress.ip_address(value.strip())
    except ValueError:
        return None


def _is_trusted(ip: ipaddress._BaseAddress, trusted_proxy_cidrs: str | None) -> bool:
    cidrs = trusted_proxy_cidrs if trusted_proxy_cidrs is not None else _configured_trusted_proxy_cidrs()
    return any(ip in network for network in _trusted_networks(cidrs))


def _header_get(headers: Mapping[Any, Any], name: str) -> str:
    name_lower = name.lower()
    for raw_name, raw_value in headers.items():
        if isinstance(raw_name, bytes):
            header_name = raw_name.decode("latin-1", errors="ignore").lower()
        else:
            header_name = str(raw_name).lower()
        if header_name != name_lower:
            continue
        if isinstance(raw_value, bytes):
            return raw_value.decode("latin-1", errors="ignore").strip()
        return str(raw_value).strip()
    return ""


def get_client_ip(
    headers: Mapping[Any, Any],
    direct_client_host: str | None,
    *,
    trusted_proxy_cidrs: list[str] | tuple[str, ...] | str | None = None,
) -> str:
    """Resolve client IP without trusting spoofable proxy headers by default."""
    direct_ip = _parse_ip(direct_client_host)
    if direct_ip is None:
        return direct_client_host or "unknown"

    if isinstance(trusted_proxy_cidrs, (list, tuple)):
        trusted_cidrs = ",".join(trusted_proxy_cidrs)
    else:
        trusted_cidrs = trusted_proxy_cidrs

    if not _is_trusted(direct_ip, trusted_cidrs):
        return str(direct_ip)

    forwarded_for = _header_get(headers, "X-Forwarded-For")
    if forwarded_for:
        forwarded_ips = [
            parsed
            for item in forwarded_for.split(",")
            if (parsed := _parse_ip(item.strip())) is not None
        ]
        for forwarded_ip in reversed(forwarded_ips):
            if not _is_trusted(forwarded_ip, trusted_cidrs):
                return str(forwarded_ip)
        if forwarded_ips:
            return str(forwarded_ips[0])

    real_ip = _parse_ip(_header_get(headers, "X-Real-IP"))
    if real_ip is not None:
        return str(real_ip)

    return str(direct_ip)


def get_client_ip_from_request(request: Request) -> str:
    client = getattr(request, "client", None)
    direct_client_host = client.host if client else None
    return get_client_ip(request.headers, direct_client_host)


def get_client_ip_from_scope(scope: Scope) -> str:
    direct_client_host = None
    client = scope.get("client")
    if client:
        direct_client_host = client[0]
    return get_client_ip(dict(scope.get("headers", [])), direct_client_host)
