"""Export and compare complete public OpenAPI operation objects for ARC-01.

The original ARC-01 snapshots retained only path, method, operationId and
response-code names. That view missed public drift in descriptions, tags,
parameters, request bodies and response schemas. This exporter keeps the
flattened route walk as a supporting witness, but its authoritative view is
the complete operation object emitted by FastAPI.

``sdk/openapi.json`` is the published baseline. A deliberate difference is
accepted only when its exact published/current values and rationale appear in
``INTENTIONAL_PUBLIC_OPERATION_DELTAS``; regenerating a snapshot is not an
approval mechanism.
"""

from __future__ import annotations

import argparse
import copy
import difflib
import json
from pathlib import Path
from typing import Any

HTTP_METHODS = frozenset({"get", "post", "put", "delete", "patch", "options", "head"})

# The 23 operations whose handlers moved from src/api/v1/assistant.py into the
# six ARC-01 route modules. Keeping this list explicit makes adding, removing
# or renaming a public handler a reviewed contract decision.
ARC01_PUBLIC_OPERATION_KEYS = frozenset(
    {
        "GET /api/v1/assistant/artifacts/{artifact_id}",
        "GET /api/v1/assistant/artifacts/{artifact_id}/download",
        "GET /api/v1/assistant/config",
        "GET /api/v1/assistant/datasets",
        "GET /api/v1/assistant/metrics/tenant",
        "GET /api/v1/assistant/models",
        "GET /api/v1/assistant/policies",
        "GET /api/v1/assistant/runs/{run_id}",
        "GET /api/v1/assistant/sessions",
        "GET /api/v1/assistant/sessions/{session_id}",
        "GET /api/v1/assistant/sessions/{session_id}/artifacts",
        "GET /api/v1/assistant/sessions/{session_id}/history",
        "GET /api/v1/assistant/sessions/{session_id}/metrics",
        "GET /api/v1/assistant/tools",
        "POST /api/v1/assistant/approvals/{approval_id}",
        "POST /api/v1/assistant/artifacts",
        "POST /api/v1/assistant/chat",
        "POST /api/v1/assistant/chat/stream",
        "POST /api/v1/assistant/runs/{run_id}/resume",
        "POST /api/v1/assistant/sessions",
        "POST /api/v1/assistant/tasks/{task_id}/cancel",
        "DELETE /api/v1/assistant/artifacts/{artifact_id}",
        "DELETE /api/v1/assistant/sessions/{session_id}",
    }
)

# Exact, reviewable exceptions to sdk/openapi.json. Broad field or operation
# allowlists are forbidden: a new delta must name both values and its source.
INTENTIONAL_PUBLIC_OPERATION_DELTAS: dict[str, dict[str, dict[str, str]]] = {
    "POST /api/v1/assistant/tasks/{task_id}/cancel": {
        "description": {
            "published": "Authenticate at the edge and cancel in the owning Assistant process.",
            "current": "Map the V1 task identifier to the owning Runtime run/turn interrupt.",
            "reason": (
                "fe2e1b88 completed the single Rust Agent Runtime cutover; the public wording "
                "must no longer claim that an Assistant process owns cancellation"
            ),
            "source_commit": "fe2e1b88",
        }
    }
}


def _walk(routes: Any, out: list[dict[str, Any]], prefix: str = "") -> None:
    from fastapi.routing import _IncludedRouter

    for route in routes:
        if isinstance(route, _IncludedRouter):
            inner = getattr(route, "original_router", None)
            if inner is not None:
                ctx = getattr(route, "include_context", None)
                include_prefix = (getattr(ctx, "prefix", "") or "") if ctx is not None else ""
                _walk(inner.routes, out, prefix + include_prefix)
            continue
        if not hasattr(route, "methods"):
            continue
        out.append(
            {
                "path": prefix + route.path,
                "methods": sorted(route.methods or []),
                "operation_id": getattr(route, "operation_id", None),
                "status_code": getattr(route, "status_code", None),
                "responses": sorted(
                    {str(code) for code in (getattr(route, "responses", None) or {})}
                ),
            }
        )


def operation_key(path: str, method: str) -> str:
    return f"{method.upper()} {path}"


def public_operation_objects(
    openapi: dict[str, Any],
    *,
    keys: frozenset[str] | None = None,
) -> dict[str, dict[str, Any]]:
    """Return deterministic complete operation objects, optionally scoped."""
    operations: dict[str, dict[str, Any]] = {}
    for path, item in openapi.get("paths", {}).items():
        for method, operation in item.items():
            if method not in HTTP_METHODS:
                continue
            key = operation_key(path, method)
            if keys is not None and key not in keys:
                continue
            operations[key] = copy.deepcopy(operation)
    return dict(sorted(operations.items()))


def _json_diff(expected: dict[str, Any], actual: dict[str, Any]) -> str:
    expected_lines = json.dumps(expected, indent=2, sort_keys=True).splitlines()
    actual_lines = json.dumps(actual, indent=2, sort_keys=True).splitlines()
    return "\n".join(
        difflib.unified_diff(expected_lines, actual_lines, fromfile="published", tofile="current")
    )


def compare_arc01_to_published(
    current_openapi: dict[str, Any], published_openapi: dict[str, Any]
) -> list[str]:
    """Compare all fields of every ARC-01 operation with exact delta handling."""
    current = public_operation_objects(current_openapi, keys=ARC01_PUBLIC_OPERATION_KEYS)
    published = public_operation_objects(published_openapi, keys=ARC01_PUBLIC_OPERATION_KEYS)
    failures: list[str] = []

    for label, operations in (("current", current), ("published", published)):
        missing = sorted(ARC01_PUBLIC_OPERATION_KEYS - operations.keys())
        if missing:
            failures.append(f"{label} OpenAPI is missing ARC-01 operations: {missing}")

    common = ARC01_PUBLIC_OPERATION_KEYS & current.keys() & published.keys()
    for key in sorted(common):
        actual = copy.deepcopy(current[key])
        expected = copy.deepcopy(published[key])
        for field, delta in INTENTIONAL_PUBLIC_OPERATION_DELTAS.get(key, {}).items():
            # A published snapshot can consume an approved historical delta.
            # Exact equality needs no exception; unequal values remain bound
            # to both sides of the original explicit allowance below.
            if expected.get(field) == actual.get(field):
                continue
            if expected.get(field) != delta["published"]:
                failures.append(
                    f"{key} intentional delta manifest has stale published {field!r}: "
                    f"{expected.get(field)!r} != {delta['published']!r}"
                )
            if actual.get(field) != delta["current"]:
                failures.append(
                    f"{key} intentional delta manifest has stale current {field!r}: "
                    f"{actual.get(field)!r} != {delta['current']!r}"
                )
            expected.pop(field, None)
            actual.pop(field, None)
        if expected != actual:
            failures.append(
                f"unapproved public operation drift for {key}:\n{_json_diff(expected, actual)}"
            )
    return failures


def export() -> dict[str, Any]:
    from src.main import app

    route_rows: list[dict[str, Any]] = []
    _walk(app.routes, route_rows)
    route_rows.sort(key=lambda row: (row["path"], row["methods"]))

    openapi = app.openapi()
    return {
        "schema": "arc01-public-operation-contract/v2",
        "routes": route_rows,
        "public_operations": public_operation_objects(openapi),
        "arc01_public_operations": public_operation_objects(
            openapi, keys=ARC01_PUBLIC_OPERATION_KEYS
        ),
        "intentional_public_operation_deltas": INTENTIONAL_PUBLIC_OPERATION_DELTAS,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", nargs="?", default="tmp/api-routes.json")
    parser.add_argument(
        "--check-sdk",
        action="store_true",
        help="compare complete ARC-01 operation objects with sdk/openapi.json without writing",
    )
    args = parser.parse_args(argv)

    if args.check_sdk:
        from src.main import app

        published = json.loads(Path("sdk/openapi.json").read_text(encoding="utf-8"))
        failures = compare_arc01_to_published(app.openapi(), published)
        if failures:
            print("\n\n".join(failures))
            return 1
        print(
            f"ARC-01 OpenAPI contract matches {len(ARC01_PUBLIC_OPERATION_KEYS)} published "
            "operation objects (1 exact intentional field delta)"
        )
        return 0

    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(export(), indent=1, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
