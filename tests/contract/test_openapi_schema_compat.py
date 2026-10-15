from __future__ import annotations

import json
from pathlib import Path

from scripts.harness.openapi_contract_gate import compare_specs
from scripts.harness.openapi_profiles import load_ownership, project
from src.main import create_app

BASELINE = Path(__file__).resolve().parents[2] / "sdk" / "openapi.json"


def _parameters_by_key(operation: dict) -> dict[tuple[str, str], dict]:
    result = {}
    for param in operation.get("parameters") or []:
        result[(param.get("name", ""), param.get("in", ""))] = param
    return result


def test_current_openapi_keeps_existing_paths_and_parameters_compatible() -> None:
    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    baseline = project(baseline, "python-hybrid", load_ownership(baseline))
    current = create_app().openapi()
    assert not compare_specs(baseline, current)

    baseline_paths = baseline.get("paths") or {}
    current_paths = current.get("paths") or {}

    missing_paths = sorted(set(baseline_paths) - set(current_paths))
    assert not missing_paths

    for path, baseline_methods in baseline_paths.items():
        current_methods = current_paths[path]
        missing_methods = sorted(set(baseline_methods) - set(current_methods))
        assert not missing_methods, f"{path} missing methods {missing_methods}"

        for method, baseline_operation in baseline_methods.items():
            current_operation = current_methods[method]
            baseline_params = _parameters_by_key(baseline_operation)
            current_params = _parameters_by_key(current_operation)
            assert set(baseline_params) <= set(current_params), f"{method.upper()} {path}"
            for key, baseline_param in baseline_params.items():
                baseline_schema = baseline_param.get("schema") or {}
                current_schema = current_params[key].get("schema") or {}
                assert current_schema.get("type") == baseline_schema.get("type"), (
                    method,
                    path,
                    key,
                )


def test_published_openapi_contains_the_actual_v2_runtime_routes() -> None:
    published = json.loads(BASELINE.read_text(encoding="utf-8"))
    app_paths = create_app().openapi().get("paths", {})
    v2_paths = {
        "/api/v2/agent/threads",
        "/api/v2/agent/threads/{thread_id}",
        "/api/v2/agent/threads/{thread_id}/turns",
        "/api/v2/agent/threads/{thread_id}/turns/{turn_id}:interrupt",
        "/api/v2/agent/threads/{thread_id}/events",
    }
    assert v2_paths <= set(app_paths)
    for path in v2_paths:
        assert published["paths"][path] == app_paths[path]
