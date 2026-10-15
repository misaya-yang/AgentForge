#!/usr/bin/env python3
"""Execute ARC-08 integration/fresh/rollback gates without placeholder passes."""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import os
import re
import secrets
import subprocess
import sys
import time
from pathlib import Path
from typing import Any
from urllib.parse import quote, urlsplit, urlunsplit

try:
    from scripts.release.release_evidence import (
        ReleaseEvidenceError,
        validate_integration_receipt,
    )
except ModuleNotFoundError:  # direct ``python scripts/release/...`` Make target
    from release_evidence import (  # type: ignore[no-redef]
        ReleaseEvidenceError,
        validate_integration_receipt,
    )

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SPEC = ROOT / "deploy/release/integration-gates.json"
SCHEMA = "ai-platform/integration-gates/v1"
SKIP = re.compile(r"\b(?:SKIP(?:PED)?|NOT APPLICABLE)\b", re.I)
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
ENV_NAME = re.compile(r"^[A-Z][A-Z0-9_]*$")
FRESH_DATABASE_PREFIX = "arc08_fresh_"
FRESH_DATABASE_NAME = re.compile(r"^arc08_fresh_[0-9a-f]{12}_[0-9a-f]{12}$")


class IntegrationGateError(RuntimeError):
    pass


def _load(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise IntegrationGateError(f"{label} unreadable: {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise IntegrationGateError(f"{label} must be an object")
    return value


def load_spec(path: Path) -> dict[str, Any]:
    spec = _load(path, "integration gate manifest")
    if spec.get("schema_version") != SCHEMA or not isinstance(spec.get("gates"), dict):
        raise IntegrationGateError("unsupported integration gate manifest")
    gates = spec["gates"]
    for gate_id, gate in gates.items():
        if not isinstance(gate, dict) or gate.get("tier") not in {"L2", "L3"}:
            raise IntegrationGateError(f"invalid integration gate: {gate_id}")
        if not isinstance(gate.get("required_env"), list) or not isinstance(
            gate.get("steps"), list
        ):
            raise IntegrationGateError(f"gate requires required_env/steps lists: {gate_id}")
        required_env = gate["required_env"]
        if not all(isinstance(name, str) and ENV_NAME.fullmatch(name) for name in required_env):
            raise IntegrationGateError(f"invalid required environment name: {gate_id}")
        fresh = gate.get("fresh_database")
        if fresh is not None:
            if (
                gate["tier"] != "L3"
                or not isinstance(fresh, dict)
                or set(fresh)
                != {
                    "admin_dsn_env",
                    "migrator_dsn_env",
                }
            ):
                raise IntegrationGateError(f"invalid fresh database contract: {gate_id}")
            fresh_env = [fresh["admin_dsn_env"], fresh["migrator_dsn_env"]]
            if not all(
                isinstance(name, str) and ENV_NAME.fullmatch(name) for name in fresh_env
            ) or not set(fresh_env).issubset(required_env):
                raise IntegrationGateError(
                    f"fresh database DSNs must be explicit required environment names: {gate_id}"
                )
        external = gate.get("external_receipt")
        if external is not None:
            if not isinstance(external, dict) or set(external) != {
                "env",
                "gate",
                "commands",
            }:
                raise IntegrationGateError(f"invalid external receipt contract: {gate_id}")
            receipt_env = external["env"]
            receipt_gate = external["gate"]
            receipt_commands = external["commands"]
            if (
                not isinstance(receipt_env, str)
                or ENV_NAME.fullmatch(receipt_env) is None
                or receipt_env not in required_env
                or not isinstance(receipt_gate, str)
                or not receipt_gate
                or not isinstance(receipt_commands, list)
                or not receipt_commands
                or not all(
                    isinstance(command, list)
                    and command
                    and all(isinstance(value, str) and value for value in command)
                    for command in receipt_commands
                )
            ):
                raise IntegrationGateError(f"invalid external receipt contract: {gate_id}")
        ids: set[str] = set()
        for step in gate["steps"]:
            if not isinstance(step, dict) or not isinstance(step.get("id"), str):
                raise IntegrationGateError(f"invalid gate step: {gate_id}")
            if step["id"] in ids or bool(step.get("command")) == bool(step.get("gate")):
                raise IntegrationGateError(f"duplicate/ambiguous gate step: {gate_id}:{step['id']}")
            ids.add(step["id"])
            if "gate" in step and step["gate"] not in gates:
                raise IntegrationGateError(f"unknown nested gate: {step['gate']}")
            if fresh is not None and "gate" in step:
                raise IntegrationGateError(
                    f"fresh database gate cannot delegate its lifecycle: {gate_id}"
                )
            if "command" in step and (
                not isinstance(step["command"], list)
                or not step["command"]
                or not all(isinstance(value, str) and value for value in step["command"])
            ):
                raise IntegrationGateError(f"invalid command: {gate_id}:{step['id']}")
    _check_cycles(gates)
    return spec


def _check_cycles(gates: dict[str, Any]) -> None:
    def visit(gate_id: str, stack: tuple[str, ...]) -> None:
        if gate_id in stack:
            raise IntegrationGateError(f"integration gate cycle: {' -> '.join((*stack, gate_id))}")
        for step in gates[gate_id]["steps"]:
            if "gate" in step:
                visit(step["gate"], (*stack, gate_id))

    for gate_id in gates:
        visit(gate_id, ())


def _source_sha(root: Path) -> str:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=root, capture_output=True, text=True, check=False
    )
    return result.stdout.strip() if result.returncode == 0 else "unknown"


def _release_identity(root: Path) -> tuple[str | None, str | None]:
    raw = os.environ.get("AI_PLATFORM_COMPATIBILITY_MANIFEST_PATH")
    if not raw:
        return None, None
    manifest = _load(Path(raw), "runtime compatibility manifest")
    if manifest.get("status") not in {"release_candidate", "released"}:
        raise IntegrationGateError("runtime compatibility manifest is not candidate/released")
    release_id = manifest.get("release_id")
    source = manifest.get("source")
    if not isinstance(release_id, str) or not isinstance(source, dict):
        raise IntegrationGateError("runtime compatibility manifest identity is incomplete")
    source_sha = source.get("git_sha")
    if not isinstance(source_sha, str) or HEX_40.fullmatch(source_sha) is None:
        raise IntegrationGateError("runtime compatibility source identity is incomplete")
    current_sha = _source_sha(root)
    if current_sha != source_sha:
        raise IntegrationGateError(
            "current checkout does not match runtime compatibility source Git SHA"
        )
    dirty = subprocess.run(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if dirty.returncode != 0 or dirty.stdout:
        raise IntegrationGateError("release integration requires a clean source checkout")
    return release_id, source_sha


def _missing_env(names: list[str]) -> list[str]:
    missing: list[str] = []
    for name in names:
        value = os.environ.get(name, "")
        if not value or (name.endswith("_AUTHORIZED") and value != "1"):
            missing.append(name)
    return missing


def _write(path: Path, receipt: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _default_receipt_path(root: Path, gate_id: str, source_sha: str) -> Path:
    if HEX_40.fullmatch(source_sha) is None:
        raise IntegrationGateError("cannot bind default receipt path to a candidate Git SHA")
    if re.fullmatch(r"[a-z0-9][a-z0-9-]*", gate_id) is None:
        raise IntegrationGateError(f"unsafe integration gate id for receipt path: {gate_id!r}")
    return root / "reports/release/integration-gates" / source_sha / f"{gate_id}.json"


def _fresh_database_name(source_sha: str) -> str:
    if HEX_40.fullmatch(source_sha) is None:
        raise IntegrationGateError("fresh database requires an exact candidate Git SHA")
    return f"{FRESH_DATABASE_PREFIX}{source_sha[:12]}_{secrets.token_hex(6)}"


def _validate_fresh_database_name(database_name: str) -> None:
    if FRESH_DATABASE_NAME.fullmatch(database_name) is None:
        raise IntegrationGateError("refusing non-generated fresh database name")


def _dsn_for_database(dsn: str, database_name: str) -> str:
    _validate_fresh_database_name(database_name)
    parts = urlsplit(dsn)
    if (
        parts.scheme not in {"postgres", "postgresql"}
        or not parts.netloc
        or not parts.path.startswith("/")
        or len(parts.path) <= 1
        or parts.fragment
    ):
        raise IntegrationGateError(
            "fresh database DSN must be a PostgreSQL URL with an explicit database"
        )
    return urlunsplit(
        (parts.scheme, parts.netloc, f"/{quote(database_name, safe='')}", parts.query, "")
    )


async def _fresh_database_action(action: str, database_name: str, admin_dsn: str) -> None:
    """Create/drop only a runner-generated scratch DB; never the singleton main DB."""
    _validate_fresh_database_name(database_name)
    try:
        import asyncpg
    except ModuleNotFoundError as exc:  # executed through ``uv --extra database``
        raise IntegrationGateError("asyncpg is unavailable for fresh database control") from exc
    role_prefix = os.environ.get("AI_GATEWAY_ROLE_PREFIX", "ai_gateway_")
    if re.fullmatch(r"[a-z][a-z0-9_]{0,20}_", role_prefix) is None:
        raise IntegrationGateError("unsafe database role prefix")
    owner_role = f"{role_prefix}owner"
    conn = await asyncpg.connect(
        admin_dsn,
        server_settings={"application_name": f"ai_platform_{action}_{database_name}"},
    )
    try:
        current = str(await conn.fetchval("SELECT current_database()"))
        if current == database_name:
            raise IntegrationGateError("admin DSN points at the fresh database target")
        if action == "create":
            exists = await conn.fetchval(
                "SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)",
                database_name,
            )
            if exists:
                raise IntegrationGateError("generated fresh database name already exists")
            await conn.execute(
                f'CREATE DATABASE "{database_name}" OWNER "{owner_role}" TEMPLATE template0'
            )
        elif action == "drop":
            await conn.execute(f'DROP DATABASE IF EXISTS "{database_name}" WITH (FORCE)')
        else:
            raise IntegrationGateError(f"unknown fresh database action: {action}")
    finally:
        await conn.close()
    print(f"fresh database {action}: {database_name}")


def _execute(command: list[str], *, root: Path, env: dict[str, str]) -> tuple[int, str]:
    try:
        process = subprocess.run(
            command,
            cwd=root,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as exc:
        return 2, f"command unavailable: {type(exc).__name__}"
    output = f"{process.stdout}\n{process.stderr}".strip()
    if output:
        print(output)
    return process.returncode, output


def _fresh_control(
    action: str,
    *,
    root: Path,
    env: dict[str, str],
    database_name: str,
    admin_dsn_env: str,
) -> tuple[int, str]:
    control_env = env.copy()
    control_env["AI_PLATFORM_FRESH_DATABASE_ADMIN_DSN"] = env[admin_dsn_env]
    command = [
        "uv",
        "run",
        "--extra",
        "database",
        "python",
        str(Path(__file__).resolve()),
        "--fresh-database-action",
        action,
        "--fresh-database-name",
        database_name,
    ]
    return _execute(command, root=root, env=control_env)


def _step_record(
    step_id: str,
    command: list[str],
    exit_code: int,
    output: str,
    started: float,
) -> dict[str, Any]:
    return {
        "id": step_id,
        "command": command,
        "exit_code": exit_code,
        "duration_seconds": round(time.monotonic() - started, 3),
        "skip_markers": len(SKIP.findall(output)),
        "output_sha256": hashlib.sha256(output.encode()).hexdigest(),
    }


def _verify_external_receipt(
    contract: dict[str, Any],
    *,
    release_id: str | None,
    source_sha: str,
) -> str:
    """Validate fresh-machine evidence before local database checks run."""

    path = Path(os.environ[contract["env"]])
    if path.is_symlink() or not path.is_file():
        raise IntegrationGateError("external fresh-runtime receipt is unavailable")
    receipt = _load(path, "external fresh-runtime receipt")
    try:
        validate_integration_receipt(
            receipt,
            expected_gate=contract["gate"],
            release_id=release_id,
            source_git_sha=source_sha,
            require_pass=True,
        )
    except ReleaseEvidenceError as exc:
        raise IntegrationGateError("external fresh-runtime receipt is invalid") from exc
    executed = {tuple(step["command"]) for step in receipt["steps"]}
    missing = [command for command in contract["commands"] if tuple(command) not in executed]
    if missing:
        raise IntegrationGateError(
            "external fresh-runtime receipt omits required quickstart checks"
        )
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _failure(record: dict[str, Any]) -> tuple[str, int] | None:
    if record["exit_code"] == 0 and record["skip_markers"] == 0:
        return None
    if record["skip_markers"]:
        return "blocked", 1
    return ("fail", 1) if record["exit_code"] == 1 else ("blocked", 2)


def run_gate(
    gate_id: str,
    spec: dict[str, Any],
    *,
    root: Path,
    dry_run: bool,
    receipt_path: Path,
    identity: tuple[str | None, str | None] | None = None,
) -> int:
    gate = spec["gates"].get(gate_id)
    if not isinstance(gate, dict):
        raise IntegrationGateError(f"unknown integration gate: {gate_id}")
    release_id, manifest_source_sha = identity or _release_identity(root)
    source_sha = manifest_source_sha or _source_sha(root)
    receipt: dict[str, Any] = {
        "schema_version": "ai-platform/integration-gate-receipt/v1",
        "gate": gate_id,
        "tier": gate["tier"],
        "release_id": release_id,
        "source_git_sha": source_sha,
        "result": "blocked",
        "unexpected_skips": 0,
        "steps": [],
    }
    missing = _missing_env(gate["required_env"])
    if missing:
        receipt["blockers"] = [f"missing required environment name: {name}" for name in missing]
        _write(receipt_path, receipt)
        print(json.dumps(receipt, indent=2, sort_keys=True))
        return 2
    if dry_run:
        receipt["result"] = "dry-run"
        receipt["steps"] = [
            {"id": step["id"], "command": step.get("command"), "gate": step.get("gate")}
            for step in gate["steps"]
        ]
        if gate.get("fresh_database") is not None:
            receipt["fresh_environment"] = {
                "kind": "scratch-postgresql-database",
                "database_name": "generated-at-runtime",
                "cleanup_result": "not-run",
            }
        _write(receipt_path, receipt)
        print(json.dumps(receipt, indent=2, sort_keys=True))
        return 0

    fresh = gate.get("fresh_database")
    if isinstance(fresh, dict):
        return _run_fresh_gate(
            gate,
            receipt,
            root=root,
            receipt_path=receipt_path,
            source_sha=source_sha,
            fresh=fresh,
        )

    started = time.monotonic()
    for step in gate["steps"]:
        step_started = time.monotonic()
        if "gate" in step:
            nested_path = receipt_path.with_name(f"{receipt_path.stem}-{step['gate']}.json")
            exit_code = run_gate(
                step["gate"],
                spec,
                root=root,
                dry_run=False,
                receipt_path=nested_path,
                identity=(release_id, manifest_source_sha),
            )
            output = nested_path.read_text(encoding="utf-8") if nested_path.is_file() else ""
            command = ["integration-gate", step["gate"]]
        else:
            command = step["command"]
            exit_code, output = _execute(command, root=root, env=os.environ.copy())
        record = _step_record(step["id"], command, exit_code, output, step_started)
        receipt["steps"].append(record)
        receipt["unexpected_skips"] += record["skip_markers"]
        failure = _failure(record)
        if failure is not None:
            receipt["result"], return_code = failure
            receipt["duration_seconds"] = round(time.monotonic() - started, 3)
            _write(receipt_path, receipt)
            return return_code
    receipt["result"] = "pass"
    receipt["duration_seconds"] = round(time.monotonic() - started, 3)
    _write(receipt_path, receipt)
    print(json.dumps(receipt, indent=2, sort_keys=True))
    return 0


def _run_fresh_gate(
    gate: dict[str, Any],
    receipt: dict[str, Any],
    *,
    root: Path,
    receipt_path: Path,
    source_sha: str,
    fresh: dict[str, str],
) -> int:
    """Run fresh authority checks against one generated DB and always remove it."""
    started = time.monotonic()
    database_name = _fresh_database_name(source_sha)
    admin_env = fresh["admin_dsn_env"]
    migrator_env = fresh["migrator_dsn_env"]
    command_env = os.environ.copy()
    scratch_dsn = _dsn_for_database(command_env[migrator_env], database_name)
    command_env.update(
        {
            "AI_GATEWAY_DATABASE_MIGRATOR_DSN": scratch_dsn,
            "DATABASE_URL": scratch_dsn,
            "GATEWAY_DATABASE__DSN": scratch_dsn,
            "AI_PLATFORM_FRESH_DATABASE_NAME": database_name,
        }
    )
    receipt["fresh_environment"] = {
        "kind": "scratch-postgresql-database",
        "database_name": database_name,
        "cleanup_result": "pending",
    }
    outcome: tuple[str, int] | None = None

    external = gate.get("external_receipt")
    if isinstance(external, dict):
        verify_started = time.monotonic()
        try:
            output = _verify_external_receipt(
                external,
                release_id=receipt.get("release_id"),
                source_sha=source_sha,
            )
            verify_exit = 0
        except IntegrationGateError:
            output = "external fresh-runtime receipt validation failed"
            verify_exit = 2
        verify_record = _step_record(
            "fresh-runtime-receipt",
            ["verify-external-receipt", str(external["gate"])],
            verify_exit,
            output,
            verify_started,
        )
        receipt["steps"].append(verify_record)
        outcome = _failure(verify_record)
        if outcome is not None:
            receipt["result"], return_code = outcome
            receipt["duration_seconds"] = round(time.monotonic() - started, 3)
            _write(receipt_path, receipt)
            return return_code

    setup_started = time.monotonic()
    setup_exit, setup_output = _fresh_control(
        "create",
        root=root,
        env=command_env,
        database_name=database_name,
        admin_dsn_env=admin_env,
    )
    setup_record = _step_record(
        "fresh-database-create",
        ["fresh-database", "create", database_name],
        setup_exit,
        setup_output,
        setup_started,
    )
    receipt["steps"].append(setup_record)
    receipt["unexpected_skips"] += setup_record["skip_markers"]
    outcome = _failure(setup_record)

    if outcome is None:
        for step in gate["steps"]:
            step_started = time.monotonic()
            exit_code, output = _execute(step["command"], root=root, env=command_env)
            record = _step_record(step["id"], step["command"], exit_code, output, step_started)
            receipt["steps"].append(record)
            receipt["unexpected_skips"] += record["skip_markers"]
            outcome = _failure(record)
            if outcome is not None:
                break

    cleanup_started = time.monotonic()
    cleanup_exit, cleanup_output = _fresh_control(
        "drop",
        root=root,
        env=command_env,
        database_name=database_name,
        admin_dsn_env=admin_env,
    )
    cleanup_record = _step_record(
        "fresh-database-drop",
        ["fresh-database", "drop", database_name],
        cleanup_exit,
        cleanup_output,
        cleanup_started,
    )
    receipt["steps"].append(cleanup_record)
    receipt["unexpected_skips"] += cleanup_record["skip_markers"]
    cleanup_failure = _failure(cleanup_record)
    receipt["fresh_environment"]["cleanup_result"] = (
        "pass" if cleanup_failure is None else "blocked"
    )
    if cleanup_failure is not None:
        outcome = ("blocked", 2)

    if outcome is None:
        receipt["result"] = "pass"
        return_code = 0
    else:
        receipt["result"], return_code = outcome
    receipt["duration_seconds"] = round(time.monotonic() - started, 3)
    _write(receipt_path, receipt)
    if return_code == 0:
        print(json.dumps(receipt, indent=2, sort_keys=True))
    return return_code


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--gate")
    mode.add_argument("--fresh-database-action", choices=("create", "drop"))
    parser.add_argument("--fresh-database-name")
    parser.add_argument("--spec", type=Path, default=DEFAULT_SPEC)
    parser.add_argument("--repo-root", type=Path, default=ROOT)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--receipt", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.fresh_database_action:
            if not args.fresh_database_name:
                raise IntegrationGateError("fresh database action requires an exact name")
            admin_dsn = os.environ.get("AI_PLATFORM_FRESH_DATABASE_ADMIN_DSN")
            if not admin_dsn:
                raise IntegrationGateError("fresh database admin DSN is missing")
            asyncio.run(
                _fresh_database_action(
                    args.fresh_database_action,
                    args.fresh_database_name,
                    admin_dsn,
                )
            )
            return 0
        root = args.repo_root.resolve()
        spec = load_spec(args.spec)
        identity = _release_identity(root)
        source_sha = identity[1] or _source_sha(root)
        receipt = args.receipt or _default_receipt_path(root, args.gate, source_sha)
        return run_gate(
            args.gate,
            spec,
            root=root,
            dry_run=args.dry_run,
            receipt_path=receipt,
            identity=identity,
        )
    except IntegrationGateError as exc:
        print(f"INTEGRATION GATE ERROR: {exc}", file=sys.stderr)
        return 2
    except Exception as exc:  # never leak a DSN from a driver exception
        if args.fresh_database_action:
            print(
                f"INTEGRATION GATE ERROR: fresh database action failed ({type(exc).__name__})",
                file=sys.stderr,
            )
            return 2
        raise


if __name__ == "__main__":
    raise SystemExit(main())
