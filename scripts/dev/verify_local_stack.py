#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request

REQUIRED_PATHS = {
    "/api/v1/auth/login",
    "/api/v1/auth/me",
    "/api/v1/assistant/chat/stream",
    "/api/v1/sessions",
}


def fetch_json(url: str) -> dict:
    with urllib.request.urlopen(url, timeout=10) as response:
        body = response.read().decode("utf-8")
    return json.loads(body)


def main() -> int:
    parser = argparse.ArgumentParser(description="Verify that a running backend is the AI Gateway API.")
    parser.add_argument("--api-url", default=os.getenv("E2E_API_URL", "").rstrip("/"))
    args = parser.parse_args()

    api_url = args.api_url.rstrip("/")
    if not api_url:
        print("Missing API URL. Set E2E_API_URL or pass --api-url.", file=sys.stderr)
        return 2

    try:
        health = fetch_json(f"{api_url}/health")
        openapi = fetch_json(f"{api_url}/openapi.json")
    except urllib.error.HTTPError as exc:
        print(f"Stack verification failed: {exc.code} {exc.reason} at {exc.url}", file=sys.stderr)
        return 1
    except Exception as exc:  # pragma: no cover - operational failure path
        print(f"Stack verification failed: {exc}", file=sys.stderr)
        return 1

    paths = set((openapi.get("paths") or {}).keys())
    missing = sorted(REQUIRED_PATHS - paths)
    if missing:
        print(
            "Target API does not look like ai-gateway. Missing required paths: " + ", ".join(missing),
            file=sys.stderr,
        )
        return 1

    title = (openapi.get("info") or {}).get("title") or "unknown"
    print(json.dumps({
        "status": "ok",
        "title": title,
        "health": health,
        "api_url": api_url,
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
