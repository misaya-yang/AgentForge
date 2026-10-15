from __future__ import annotations

import copy
import json
import runpy
from pathlib import Path
from typing import Any

from fastapi.routing import APIRoute

from src.api.v1._assistant_routes import artifacts, catalog, chat, metrics, runs, sessions
from src.main import app

ROOT = Path(__file__).resolve().parents[2]
EXPORTER = runpy.run_path(
    str(ROOT / "reports/contracts/arc-01/arc01-export-routes.py"),
    run_name="arc01_exporter",
)
ARC01_PUBLIC_OPERATION_KEYS: frozenset[str] = EXPORTER["ARC01_PUBLIC_OPERATION_KEYS"]


def _split_router_operation_keys() -> set[str]:
    keys: set[str] = set()
    for module in (artifacts, catalog, chat, metrics, runs, sessions):
        for route in module.router.routes:
            if not isinstance(route, APIRoute):
                continue
            for method in route.methods:
                keys.add(f"{method} /api/v1/assistant{route.path}")
    return keys


def test_arc01_operation_inventory_is_explicit_and_complete() -> None:
    assert _split_router_operation_keys() == ARC01_PUBLIC_OPERATION_KEYS


def test_arc01_complete_public_operations_match_published_sdk() -> None:
    published = json.loads((ROOT / "sdk/openapi.json").read_text(encoding="utf-8"))
    compare = EXPORTER["compare_arc01_to_published"]

    failures = compare(app.openapi(), published)

    assert failures == [], "\n\n".join(failures)


def test_arc01_export_contains_full_operation_objects() -> None:
    exported: dict[str, Any] = EXPORTER["export"]()

    assert exported["schema"] == "arc01-public-operation-contract/v2"
    operations = exported["arc01_public_operations"]
    assert set(operations) == ARC01_PUBLIC_OPERATION_KEYS

    chat_operation = operations["POST /api/v1/assistant/chat"]
    assert {
        "tags",
        "summary",
        "description",
        "operationId",
        "requestBody",
        "responses",
    } <= chat_operation.keys()
    assert isinstance(chat_operation["responses"]["200"]["content"], dict)


def test_intentional_delta_manifest_is_exact_and_narrow() -> None:
    manifest = EXPORTER["INTENTIONAL_PUBLIC_OPERATION_DELTAS"]

    assert set(manifest) == {"POST /api/v1/assistant/tasks/{task_id}/cancel"}
    assert set(manifest["POST /api/v1/assistant/tasks/{task_id}/cancel"]) == {"description"}
    delta = manifest["POST /api/v1/assistant/tasks/{task_id}/cancel"]["description"]
    assert delta["source_commit"] == "fe2e1b88"
    assert delta["published"] != delta["current"]
    assert delta["reason"]


def test_unapproved_nonstructural_operation_drift_fails_closed() -> None:
    published = json.loads((ROOT / "sdk/openapi.json").read_text(encoding="utf-8"))
    current = copy.deepcopy(app.openapi())
    current["paths"]["/api/v1/assistant/models"]["get"]["tags"] = ["not-assistant"]

    failures = EXPORTER["compare_arc01_to_published"](current, published)

    assert any(
        failure.startswith("unapproved public operation drift for GET /api/v1/assistant/models:")
        for failure in failures
    )
