#!/usr/bin/env python3
"""Run the immutable Agent Studio AS-00 through AS-08 regression manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = ROOT / "tests/fixtures/agent-studio/regression_manifest.json"
DEFAULT_OUTPUT = ROOT / "reports/agent-studio/agent-studio-regression-v1-result.json"
REPORT_PREFIXES = ("reports/", "web/playwright-report/", "web/test-results/")
REPORT_EXCLUDE_PATHSPECS = tuple(f":(exclude){prefix}**" for prefix in REPORT_PREFIXES)
SKIP_SUMMARY_PATTERN = re.compile(r"(?im)(?<![\w.])([1-9]\d*)\s+skipped(?:\s|,|$)")


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _load_manifest(path: Path) -> dict[str, Any]:
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if manifest.get("schema_version") != "agent-studio-regression/v1":
        raise ValueError("unsupported Agent Studio regression manifest schema")
    gates = manifest.get("gates")
    if not isinstance(gates, list) or not gates:
        raise ValueError("manifest gates must be a non-empty list")
    seen: set[str] = set()
    for gate in gates:
        if not isinstance(gate, dict):
            raise ValueError("every manifest gate must be an object")
        key = f"{gate.get('phase')}:{gate.get('id')}"
        if key in seen:
            raise ValueError(f"duplicate manifest gate: {key}")
        seen.add(key)
        if gate.get("required") is not True:
            raise ValueError(f"manifest gate is not required: {key}")
        if not all(
            isinstance(gate.get(field), str) and gate[field]
            for field in ("phase", "id", "cwd", "command")
        ):
            raise ValueError(f"manifest gate is incomplete: {key}")
        cwd = (ROOT / gate["cwd"]).resolve()
        if cwd != ROOT and ROOT not in cwd.parents:
            raise ValueError(f"manifest gate leaves repository: {key}")
    return manifest


def _git_bytes(*args: str) -> bytes:
    result = subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True)
    return result.stdout


def _workspace_hash() -> str:
    """Hash the compatible source/test build while excluding generated evidence."""

    digest = hashlib.sha256()
    digest.update(_git_bytes("rev-parse", "HEAD"))
    digest.update(
        _git_bytes(
            "diff",
            "--binary",
            "HEAD",
            "--",
            ".",
            *REPORT_EXCLUDE_PATHSPECS,
        )
    )
    untracked = _git_bytes("ls-files", "--others", "--exclude-standard", "-z")
    for raw_path in sorted(path for path in untracked.split(b"\0") if path):
        relative = raw_path.decode("utf-8", errors="surrogateescape")
        if relative.startswith(REPORT_PREFIXES):
            continue
        candidate = ROOT / relative
        if not candidate.is_file():
            continue
        digest.update(raw_path)
        digest.update(b"\0")
        digest.update(candidate.read_bytes())
    return digest.hexdigest()


def _reported_skip_count(output: str) -> int:
    """Return the largest non-zero test-runner skip summary in output."""

    return max((int(match) for match in SKIP_SUMMARY_PATTERN.findall(output)), default=0)


def _run_gate(gate: dict[str, Any], log_dir: Path) -> dict[str, Any]:
    key = f"{gate['phase'].lower()}-{gate['id']}"
    log_path = log_dir / f"{key}.log"
    env = os.environ.copy()
    env.setdefault("COMPOSE_PARALLEL_LIMIT", "1")
    # This aggregate is intentionally a low-resource, deterministic release
    # gate. Playwright treats CI as a single-worker environment, which avoids
    # spawning several Chromium renderers alongside the local Docker stack.
    env.setdefault("CI", "1")
    env.setdefault("GATEWAY_BASE_URL", "http://127.0.0.1:8080")
    local_no_proxy = "localhost,127.0.0.1,::1"
    for key in ("NO_PROXY", "no_proxy"):
        current = env.get(key, "")
        entries = [item.strip() for item in current.split(",") if item.strip()]
        for local_host in local_no_proxy.split(","):
            if local_host not in entries:
                entries.append(local_host)
        env[key] = ",".join(entries)
    # Local live acceptance uses the ignored dedicated E2E account. Read it
    # only for this process and never print or persist its values. CI and
    # operators can continue to provide the equivalent environment variables.
    credential_path = ROOT / "web/.playwright/e2e-user.json"
    if credential_path.is_file() and not (
        env.get("ASSISTANT_E2E_USER1_EMAIL") and env.get("ASSISTANT_E2E_PASSWORD")
    ):
        try:
            credentials = json.loads(credential_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            credentials = {}
        email = credentials.get("email") if isinstance(credentials, dict) else None
        password = credentials.get("password") if isinstance(credentials, dict) else None
        if isinstance(email, str) and isinstance(password, str) and email and password:
            env["ASSISTANT_E2E_USER1_EMAIL"] = email
            env["ASSISTANT_E2E_PASSWORD"] = password
    started_at = _now()
    started = time.monotonic()
    print(f"\n[Agent Studio] START {gate['phase']}:{gate['id']}", flush=True)
    with log_path.open("w", encoding="utf-8") as log:
        process = subprocess.Popen(
            gate["command"],
            cwd=ROOT / gate["cwd"],
            env=env,
            executable="/bin/bash",
            shell=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        assert process.stdout is not None
        for line in process.stdout:
            sys.stdout.write(line)
            log.write(line)
        exit_code = process.wait()
    elapsed = round(time.monotonic() - started, 3)
    skipped_count = _reported_skip_count(log_path.read_text(encoding="utf-8"))
    status = "passed" if exit_code == 0 and skipped_count == 0 else "failed"
    failure_reason = None
    if skipped_count:
        failure_reason = f"required gate reported {skipped_count} skipped test(s)"
    elif exit_code != 0:
        failure_reason = f"gate command exited with status {exit_code}"
    print(
        f"[Agent Studio] {status.upper()} {gate['phase']}:{gate['id']} ({elapsed}s)",
        flush=True,
    )
    return {
        "phase": gate["phase"],
        "id": gate["id"],
        "status": status,
        "exit_code": exit_code,
        "skipped_count": skipped_count,
        "failure_reason": failure_reason,
        "started_at": started_at,
        "completed_at": _now(),
        "elapsed_seconds": elapsed,
        "log": str(log_path.relative_to(ROOT)),
        "log_sha256": _sha256(log_path.read_bytes()),
    }


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--validate-only", action="store_true")
    parser.add_argument("--list", action="store_true", dest="list_gates")
    parser.add_argument("--fail-fast", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = _parse_args()
    manifest_path = args.manifest.resolve()
    manifest = _load_manifest(manifest_path)
    manifest_hash = _sha256(manifest_path.read_bytes())
    gates: list[dict[str, Any]] = manifest["gates"]
    if args.list_gates:
        for gate in gates:
            print(f"{gate['phase']}:{gate['id']}\t{gate['command']}")
        return 0
    print(f"Agent Studio manifest v1: {len(gates)} required gates, sha256:{manifest_hash}")
    if args.validate_only:
        return 0

    source_hash_before = _workspace_hash()
    output_path = args.output.resolve()
    output_path.parent.mkdir(parents=True, exist_ok=True)
    log_dir = output_path.parent / "regression-v1"
    log_dir.mkdir(parents=True, exist_ok=True)
    started_at = _now()
    results: list[dict[str, Any]] = []
    for gate in gates:
        result = _run_gate(gate, log_dir)
        results.append(result)
        if args.fail_fast and result["status"] != "passed":
            break
    source_hash_after = _workspace_hash()
    source_stable = source_hash_before == source_hash_after
    all_ran = len(results) == len(gates)
    passed = all_ran and source_stable and all(item["status"] == "passed" for item in results)
    payload = {
        "schema_version": "agent-studio-regression-result/v1",
        "status": "passed" if passed else "failed",
        "started_at": started_at,
        "completed_at": _now(),
        "manifest": str(manifest_path.relative_to(ROOT)),
        "manifest_sha256": manifest_hash,
        "source_sha256": source_hash_before,
        "source_stable": source_stable,
        "required_gate_count": len(gates),
        "executed_gate_count": len(results),
        "passed_gate_count": sum(item["status"] == "passed" for item in results),
        "failed_gate_count": sum(item["status"] == "failed" for item in results),
        "results": results,
    }
    output_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    print(
        f"\nAgent Studio aggregate {payload['status']}: "
        f"{payload['passed_gate_count']}/{payload['required_gate_count']} gates; "
        f"source_stable={source_stable}; result={output_path.relative_to(ROOT)}"
    )
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
