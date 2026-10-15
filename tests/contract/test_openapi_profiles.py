"""Profile boundaries must never convert public drift into a green release gate."""
from __future__ import annotations

import copy
import json

import pytest

from scripts import snapshot_gateway_openapi as snapshot
from scripts.harness import openapi_contract_gate as gate
from scripts.harness import openapi_profiles as profiles


@pytest.fixture
def published():
    return json.loads(gate.DEFAULT_BASELINE.read_text())


def test_actual_python_export_is_compatible(published):
    from src.main import create_app
    ownership = profiles.load_ownership(published)
    current = create_app().openapi()
    profiles.check_python_ownership(current, ownership)
    assert not gate.compare_specs(profiles.project(published, "python-hybrid", ownership), current)
    assert gate.compare_specs(published, current)  # The full mixed mismatch is not hidden.
    assert profiles.project(published, "native", ownership) == published


@pytest.mark.parametrize("mutation", ["path", "operation", "scheme", "auth", "response"])
def test_true_python_regressions_remain_red(published, mutation):
    baseline = profiles.project(published, "python-hybrid", profiles.load_ownership(published))
    current = copy.deepcopy(baseline)
    path = next(iter(current["paths"]))
    method = next(m for m in current["paths"][path] if m in gate.HTTP_METHODS)
    op = current["paths"][path][method]
    if mutation == "path":
        del current["paths"][path]
    elif mutation == "operation":
        op["operationId"] = "changed"
    elif mutation == "scheme":
        baseline["components"]["securitySchemes"]["FixtureBearer"] = {"type": "http", "scheme": "bearer"}
        current["components"]["securitySchemes"]["FixtureBearer"] = {"type": "apiKey", "name": "key", "in": "header"}
    elif mutation == "auth":
        op["security"] = [{"Unexpected": []}]
    else:
        op["responses"] = {}
    assert gate.compare_specs(baseline, current)


@pytest.mark.parametrize("mutation", ["duplicate", "stale", "unknown", "proof", "security_overlap"])
def test_ownership_invalidity_fails_closed(published, tmp_path, mutation):
    ownership = profiles.load_ownership(published)
    if mutation == "duplicate":
        ownership["native_only"].append(ownership["native_only"][0])
    elif mutation == "stale":
        ownership["native_only"][0]["operations"]["get"] = "changed"
    elif mutation == "unknown":
        ownership["surprise"] = True
    elif mutation == "proof":
        ownership["native_only"][0]["source_evidence"] = ["nonexistent route"]
    else:
        path = next(p for p in published["paths"] if p not in {e["path"] for e in ownership["native_only"]})
        method = next(m for m in published["paths"][path] if m in gate.HTTP_METHODS)
        published["paths"][path][method]["security"] = [{"NativeBearerAuth": []}]
    path = tmp_path / "ownership.json"
    path.write_text(json.dumps(ownership))
    with pytest.raises(ValueError):
        profiles.load_ownership(published, path)


def test_python_native_route_overlap_rejected(published):
    with pytest.raises(ValueError, match="overlaps"):
        profiles.check_python_ownership(published, profiles.load_ownership(published))


def test_unknown_profile_rejected(published):
    with pytest.raises(ValueError, match="unknown"):
        profiles.project(published, "typo", profiles.load_ownership(published))


@pytest.mark.parametrize("profile", ["native", "published"])
def test_missing_native_candidate_fails_with_evidence(tmp_path, profile):
    out = tmp_path / "evidence.json"
    assert gate.run_real_gate(gate.DEFAULT_BASELINE, out, profile) == 2
    report = json.loads(out.read_text())
    assert report["result"] == "error"
    assert report["native_runtime_verified"] is False
    assert "independent" in report["error"]


def test_published_self_comparison_rejected(tmp_path):
    assert gate.run_real_gate(gate.DEFAULT_BASELINE, tmp_path / "evidence.json", "native", gate.DEFAULT_BASELINE) == 2


def test_native_comparison_keeps_native_operations(published, tmp_path):
    del published["paths"]["/api/v1/scheduled-tasks"]
    candidate = tmp_path / "native.json"
    candidate.write_text(json.dumps(published))
    out = tmp_path / "evidence.json"
    assert gate.run_real_gate(gate.DEFAULT_BASELINE, out, "native", candidate) == 1
    assert any("/api/v1/scheduled-tasks" in v for v in json.loads(out.read_text())["violations"])


def test_unsafe_python_overwrite_rejected(tmp_path):
    before = gate.DEFAULT_BASELINE.read_bytes()
    for output in [gate.DEFAULT_BASELINE, tmp_path / "alias.json"]:
        if output != gate.DEFAULT_BASELINE:
            output.symlink_to(gate.DEFAULT_BASELINE)
        with pytest.raises(SystemExit) as exc:
            snapshot.main(["--profile", "python-hybrid", "--output", str(output)])
        assert exc.value.code == 2
    assert gate.DEFAULT_BASELINE.read_bytes() == before


def test_safe_actual_python_candidate_export(tmp_path):
    before = gate.DEFAULT_BASELINE.read_bytes()
    output = tmp_path / "python.json"
    assert snapshot.main(["--profile", "python-hybrid", "--output", str(output)]) == 0
    assert json.loads(output.read_text())["paths"]
    assert gate.DEFAULT_BASELINE.read_bytes() == before


def test_determinism_drift_remains_red():
    assert gate._operation_id_drift({"GET /x": "stable"}, {"GET /x": "changed"})


def test_cannot_reclassify_python_route_as_native_to_hide_drift(published, tmp_path):
    ownership = profiles.load_ownership(published)
    ownership["python_paths"].append(ownership["native_only"][0]["path"])
    path = tmp_path / "ownership.json"
    path.write_text(json.dumps(ownership))
    with pytest.raises(ValueError, match="overlapping"):
        profiles.load_ownership(published, path)


def test_unknown_published_path_needs_explicit_ownership(published):
    published["paths"]["/new-unowned"] = {"get": {"operationId": "new", "responses": {"200": {}}}}
    with pytest.raises(ValueError, match="unknown"):
        profiles.load_ownership(published)


def test_matching_candidate_is_not_native_producer_evidence(published, tmp_path):
    candidate = tmp_path / "candidate.json"
    candidate.write_text(json.dumps(published))
    out = tmp_path / "evidence.json"
    assert gate.run_real_gate(gate.DEFAULT_BASELINE, out, "native", candidate) == 2
    report = json.loads(out.read_text())
    assert report["comparison_result"] == "pass"
    assert report["result"] == "error"
    assert "producer" in report["error"]
    assert gate.run_real_gate(gate.DEFAULT_BASELINE, out, "published", candidate) == 0
    assert json.loads(out.read_text())["native_runtime_verified"] is False
