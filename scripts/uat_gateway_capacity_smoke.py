#!/usr/bin/env python
from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
from collections import Counter
from typing import Any

import httpx

SECRET_MARKERS = ("api_key", "_api_key", "authorization", "bearer ", "cookie", "auth_token")


def _headers() -> dict[str, str]:
    headers: dict[str, str] = {}
    token = os.environ.get("GATEWAY_TOKEN") or os.environ.get("GATEWAY_ADMIN_JWT")
    api_key = os.environ.get("GATEWAY_API_KEY")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    if api_key:
        headers["X-API-Key"] = api_key
    return headers


def _contains_raw_secret(text: str) -> bool:
    lowered = text.lower()
    return any(marker in lowered for marker in SECRET_MARKERS)


async def _send(
    client: httpx.AsyncClient,
    service_id: str,
    index: int,
    stream: bool,
    path: str,
) -> tuple[int | str, dict[str, str], str]:
    if stream:
        endpoint = f"/api/v1/proxy/{service_id}/runs/stream"
        body: dict[str, Any] = {
            "input": {"messages": [{"role": "user", "content": f"capacity smoke {index}"}]},
            "stream_mode": ["updates"],
        }
    else:
        endpoint = f"/api/v1/proxy/{service_id}/{path}"
        body = {"limit": 5, "probe": index}
    try:
        response = await client.post(endpoint, json=body)
        return response.status_code, dict(response.headers), response.text[:1000]
    except Exception as exc:
        return "connection_error", {}, str(exc)


async def run(args: argparse.Namespace) -> int:
    headers = _headers()
    if not headers:
        print("GATEWAY_TOKEN/GATEWAY_ADMIN_JWT or GATEWAY_API_KEY is required", file=sys.stderr)
        return 2

    sem = asyncio.Semaphore(args.concurrency)
    stream_cutoff = int(args.requests * max(min(args.stream_ratio, 1.0), 0.0))

    async with httpx.AsyncClient(
        base_url=args.base_url.rstrip("/"),
        headers=headers,
        timeout=httpx.Timeout(60.0, connect=10.0),
    ) as client:
        async def worker(index: int) -> tuple[int | str, dict[str, str], str]:
            async with sem:
                return await _send(
                    client,
                    args.service_id,
                    index,
                    stream=index < stream_cutoff,
                    path=args.path,
                )

        results = await asyncio.gather(*(worker(index) for index in range(args.requests)))

    counter: Counter[str] = Counter()
    unexpected: list[dict[str, Any]] = []
    capacity_headers = 0
    for status, headers_map, body in results:
        if status == "connection_error":
            counter["connection_error"] += 1
            unexpected.append({"status": status, "body": body})
            continue
        status_int = int(status)
        if 200 <= status_int < 300:
            counter["2xx"] += 1
        elif status_int == 429:
            counter["429"] += 1
        elif status_int == 503:
            counter["503"] += 1
        else:
            counter[str(status_int)] += 1
            unexpected.append({"status": status_int, "body": body[:300]})
        if status_int >= 500 and status_int != 503:
            unexpected.append({"status": status_int, "body": body[:300]})
        if _contains_raw_secret(body):
            unexpected.append({"status": status_int, "body": "secret marker detected"})
        if any(key.lower() == "x-gateway-capacity-key" for key in headers_map):
            capacity_headers += 1

    payload = {
        "base_url": args.base_url,
        "service_id": args.service_id,
        "requests": args.requests,
        "concurrency": args.concurrency,
        "stream_ratio": args.stream_ratio,
        "counts": dict(counter),
        "capacity_header_responses": capacity_headers,
        "unexpected": unexpected[:10],
    }
    print(json.dumps(payload, indent=2, sort_keys=True))

    if unexpected:
        return 1
    if args.expect_capacity_denial and counter["429"] + counter["503"] == 0:
        return 2
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Gateway UAT capacity smoke test.")
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--service-id", default="local-2024-agent")
    parser.add_argument("--concurrency", type=int, default=20)
    parser.add_argument("--requests", type=int, default=60)
    parser.add_argument("--stream-ratio", type=float, default=0.0)
    parser.add_argument("--path", default="assistants/search")
    parser.add_argument("--expect-capacity-denial", action="store_true")
    args = parser.parse_args()
    return asyncio.run(run(args))


if __name__ == "__main__":
    sys.exit(main())
