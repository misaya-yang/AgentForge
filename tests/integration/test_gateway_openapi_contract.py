"""Gateway OpenAPI contract test."""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

import httpx
import pytest

BASELINE_PATH = Path(__file__).resolve().parents[2] / "sdk" / "openapi.json"
GATEWAY_BASE_URL = os.getenv("GATEWAY_BASE_URL", "http://localhost:8080").rstrip("/")


def _load_live_spec() -> dict[str, Any] | None:
    try:
        with httpx.Client(timeout=5.0, trust_env=False) as client:
            response = client.get(f"{GATEWAY_BASE_URL}/openapi.json")
            if response.status_code == 200 and isinstance(response.json(), dict):
                return response.json()
    except Exception:
        return None
    return None


def _required_for(spec: dict[str, Any], path: str, method: str) -> set[str]:
    operation = spec.get("paths", {}).get(path, {}).get(method.lower()) or {}
    content = (operation.get("requestBody") or {}).get("content") or {}
    schema = (content.get("application/json") or {}).get("schema") or {}
    ref = schema.get("$ref")
    if isinstance(ref, str) and ref.startswith("#/components/schemas/"):
        schema = spec.get("components", {}).get("schemas", {}).get(ref.rsplit("/", 1)[-1], schema)
    return set(schema.get("required") or [])


@pytest.mark.integration
def test_gateway_openapi_superset_of_published_snapshot() -> None:
    if not BASELINE_PATH.exists():
        pytest.skip(f"Gateway OpenAPI snapshot missing: {BASELINE_PATH}")
    baseline = json.loads(BASELINE_PATH.read_text(encoding="utf-8"))
    current = _load_live_spec()
    if current is None:
        pytest.skip("Gateway OpenAPI unavailable; run against the Gateway")
    removed = set(baseline.get("paths", {})) - set(current.get("paths", {}))
    assert not removed, f"Gateway OpenAPI drift: paths removed: {sorted(removed)}"
    drifted: list[str] = []
    for path, operations in baseline.get("paths", {}).items():
        for method in operations:
            if method.startswith("x-"):
                continue
            lost = _required_for(baseline, path, method) - _required_for(current, path, method)
            if lost:
                drifted.append(f"{method.upper()} {path}: lost required {sorted(lost)}")
    assert not drifted, "Gateway OpenAPI drift:\n  " + "\n  ".join(drifted)
