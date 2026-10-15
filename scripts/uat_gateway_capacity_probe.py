#!/usr/bin/env python
from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
from dataclasses import dataclass
from typing import Any

import httpx

HEADER_NAMES = (
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
    "retry-after",
    "x-gateway-capacity-key",
    "x-gateway-queue-wait-ms",
    "x-request-id",
)


@dataclass
class ProbeResult:
    method: str
    path: str
    status_code: int
    headers_present: dict[str, bool]
    body_preview: str


def _headers() -> dict[str, str]:
    headers: dict[str, str] = {}
    token = os.environ.get("GATEWAY_TOKEN")
    api_key = os.environ.get("GATEWAY_API_KEY")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    if api_key:
        headers["X-API-Key"] = api_key
    return headers


def _redact(value: str) -> str:
    token = os.environ.get("GATEWAY_TOKEN")
    api_key = os.environ.get("GATEWAY_API_KEY")
    for secret in (token, api_key):
        if secret:
            value = value.replace(secret, "[redacted]")
    return value


async def _request(client: httpx.AsyncClient, method: str, path: str, json_body: Any = None) -> ProbeResult:
    response = await client.request(method, path, json=json_body)
    preview = _redact(response.text[:500])
    lower_headers = {key.lower(): value for key, value in response.headers.items()}
    return ProbeResult(
        method=method,
        path=path,
        status_code=response.status_code,
        headers_present={name: name in lower_headers for name in HEADER_NAMES},
        body_preview=preview,
    )


async def run(args: argparse.Namespace) -> int:
    if args.allow_mutation is False and not args.readonly:
        return 3
    if args.validate_fixtures:
        missing = []
        if args.require_admin_jwt and not os.environ.get("GATEWAY_ADMIN_JWT"):
            missing.append("GATEWAY_ADMIN_JWT")
        if args.require_non_admin_jwt and not os.environ.get("GATEWAY_NON_ADMIN_JWT"):
            missing.append("GATEWAY_NON_ADMIN_JWT")
        if args.require_api_key and not os.environ.get("GATEWAY_API_KEY"):
            missing.append("GATEWAY_API_KEY")
        if missing:
            print(json.dumps({"fixture_status": "missing", "missing": missing}, indent=2))
            return 2

    async with httpx.AsyncClient(
        base_url=args.base_url.rstrip("/"),
        headers=_headers(),
        timeout=httpx.Timeout(30.0, connect=10.0),
    ) as client:
        results = [
            await _request(client, "GET", "/api/v1/config/status"),
            await _request(client, "GET", "/api/v1/config/rate-limits"),
            await _request(client, "GET", f"/api/v1/config/services/{args.service_id}/config"),
        ]

        sem = asyncio.Semaphore(args.concurrency)

        async def proxy_probe(index: int) -> ProbeResult:
            async with sem:
                return await _request(
                    client,
                    "POST",
                    f"/api/v1/proxy/{args.service_id}/{args.path}",
                    {"limit": 5, "probe": index},
                )

        results.extend(await asyncio.gather(*(proxy_probe(i) for i in range(args.concurrency))))

    payload = {
        "base_url": args.base_url,
        "service_id": args.service_id,
        "readonly": args.readonly,
        "results": [result.__dict__ for result in results],
    }
    print(json.dumps(payload, indent=2, sort_keys=True))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Read-only Gateway capacity acceptance probe.")
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--service-id", default="local-2024-agent")
    parser.add_argument("--concurrency", type=int, default=4)
    parser.add_argument("--path", default="assistants/search")
    parser.add_argument("--readonly", action="store_true")
    parser.add_argument("--allow-mutation", action="store_true")
    parser.add_argument("--validate-fixtures", action="store_true")
    parser.add_argument("--require-admin-jwt", action="store_true")
    parser.add_argument("--require-non-admin-jwt", action="store_true")
    parser.add_argument("--require-api-key", action="store_true")
    args = parser.parse_args()
    return asyncio.run(run(args))


if __name__ == "__main__":
    sys.exit(main())
