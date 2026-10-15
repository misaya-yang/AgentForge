from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def _load_module():
    script_path = ROOT / "scripts/new/gateway_preflight.py"
    spec = importlib.util.spec_from_file_location("gateway_preflight", script_path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def test_gateway_preflight_passes_static_security_checks():
    mod = _load_module()
    payload = mod.run_preflight(ROOT, require_helm=False)

    assert payload["status"] == "ok"
    names = {check["name"] for check in payload["checks"]}
    assert "auth_whitelist_metrics" in names
    assert "ingress_metrics_exposed" in names
    assert "helm_secret_defaults" in names


def test_gateway_preflight_cli_emits_json():
    mod = _load_module()
    exit_code = mod.main(["--root", str(ROOT)])
    assert exit_code == 0
