#!/usr/bin/env python
"""Read-only Gateway deployment preflight for open-source releases.

Checks static Helm/templates and gateway security defaults. Never mutates
remote services and never prints secrets. All strict gates are opt-in via CLI
flags so local quickstart is not blocked by default.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any


@dataclass(frozen=True)
class CheckResult:
    name: str
    status: str
    message: str


def _pass(name: str, message: str) -> CheckResult:
    return CheckResult(name=name, status="pass", message=message)


def _warn(name: str, message: str) -> CheckResult:
    return CheckResult(name=name, status="warn", message=message)


def _fail(name: str, message: str) -> CheckResult:
    return CheckResult(name=name, status="fail", message=message)


def check_static_security(root: Path) -> list[CheckResult]:
    checks: list[CheckResult] = []
    main_py = (root / "src/main.py").read_text(encoding="utf-8")
    ingress = (root / "deploy/helm/ai-gateway/templates/ingress.yaml").read_text(encoding="utf-8")
    secret = (root / "deploy/helm/ai-gateway/templates/secret.yaml").read_text(encoding="utf-8")

    whitelist = main_py.split("whitelist_paths=[", 1)[1].split("]", 1)[0]
    if '"/metrics"' in whitelist:
        checks.append(_fail("auth_whitelist_metrics", "/metrics is still auth-whitelisted"))
    else:
        checks.append(_pass("auth_whitelist_metrics", "/metrics is not auth-whitelisted"))

    if "path: /metrics" in ingress:
        checks.append(_fail("ingress_metrics_exposed", "ingress still routes /metrics publicly"))
    else:
        checks.append(_pass("ingress_metrics_exposed", "ingress does not expose /metrics"))

    insecure_defaults = (
        'default "change-me"' in secret
        or 'default "change-me-in-production"' in secret
    )
    if insecure_defaults:
        checks.append(
            _fail(
                "helm_secret_defaults",
                "Helm secret template still has insecure defaults",
            )
        )
    else:
        checks.append(
            _pass(
                "helm_secret_defaults",
                "Helm secret template requires explicit values",
            )
        )

    return checks


def check_helm(chart_dir: Path, *, require_helm: bool = False) -> list[CheckResult]:
    helm = shutil.which("helm")
    if not helm:
        status = _fail if require_helm else _warn
        return [status("helm_available", "helm binary not found; static checks still ran")]

    lint = subprocess.run(
        [helm, "lint", str(chart_dir)],
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    if lint.returncode == 0:
        return [_pass("helm_lint", "helm lint passed")]
    return [_fail("helm_lint", (lint.stderr or lint.stdout).strip()[:500])]


def run_preflight(
    root: Path,
    *,
    chart_dir: Path | None = None,
    require_helm: bool = False,
) -> dict[str, Any]:
    chart = chart_dir or (root / "deploy/helm/ai-gateway")
    checks = [
        *check_static_security(root),
        *check_helm(chart, require_helm=require_helm),
    ]
    failures = [c for c in checks if c.status == "fail"]
    warnings = [c for c in checks if c.status == "warn"]
    return {
        "schema_version": "gateway-preflight/v1",
        "status": "fail" if failures else "ok",
        "summary": {
            "passed": sum(1 for c in checks if c.status == "pass"),
            "warnings": len(warnings),
            "failures": len(failures),
        },
        "checks": [asdict(c) for c in checks],
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[2],
        help="Repository root (defaults to project root).",
    )
    parser.add_argument(
        "--chart-dir",
        type=Path,
        default=None,
        help="Helm chart directory (defaults to deploy/helm/ai-gateway).",
    )
    parser.add_argument(
        "--require-helm",
        action="store_true",
        help="Fail when helm is unavailable or helm lint fails.",
    )
    args = parser.parse_args(argv)
    payload = run_preflight(args.root, chart_dir=args.chart_dir, require_helm=args.require_helm)
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0 if payload["status"] == "ok" else 1


if __name__ == "__main__":
    raise SystemExit(main())
