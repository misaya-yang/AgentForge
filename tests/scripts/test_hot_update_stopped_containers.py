"""Exercise hot-update against command stubs; never invokes a Docker daemon."""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest


def _fixture(tmp_path: Path) -> dict[str, str]:
    scripts = tmp_path / "scripts/new"
    scripts.mkdir(parents=True)
    (scripts / "hot-update.sh").write_text(Path("scripts/new/hot-update.sh").read_text())
    (scripts / "common.sh").write_text("""set -euo pipefail
PROJECT_ROOT="$TEST_PROJECT_ROOT"
ENV_FILE="$PROJECT_ROOT/unused.env"
load_env() { :; }
require_docker() { :; }
require_env_file() { :; }
assert_compose_owner() { :; }
assert_runtime_release_unit_locked() { :; }
agent_runtime_image_tag() { echo runtime:locked; }
agent_capability_worker_image_tag() { echo worker:locked; }
get_compose_cmd() { echo docker compose; }
gateway_container() { echo gateway; }
knowledge_container() { echo knowledge; }
knowledge_worker_container() { echo knowledge-worker; }
frontend_container() { echo frontend; }
agent_runtime_container() { echo runtime; }
agent_capability_worker_container() { echo capability-worker; }
log_step() { :; }
log_info() { :; }
log_warn() { :; }
log_success() { :; }
log_error() { echo "$1" >&2; }
wait_for_healthy() { echo '["health"]' >> "$TEST_CALLS"; }
""")
    for directory in [
        "src",
        "config",
        "database",
        "packages/ai-gateway-core/src/ai_gateway_core",
        "packages/ai-gateway-contracts/src/ai_gateway_contracts",
        "apps/knowledge-service/src/knowledge_service",
        "web/dist",
        "web/docker-entrypoint.d",
    ]:
        (tmp_path / directory).mkdir(parents=True)
    catalog = (
        tmp_path
        / "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/platform_catalog_v1.json"
    )
    catalog.parent.mkdir(parents=True)
    catalog.write_text("{}")
    (tmp_path / "web/docker-entrypoint.d/40-runtime-config.sh").write_text("#!/bin/sh\nexit 0\n")
    (tmp_path / "web/docker-entrypoint.d/40-runtime-config.sh").chmod(0o644)
    (
        tmp_path / "packages/ai-gateway-contracts/src/ai_gateway_contracts/database_revision.py"
    ).write_text("EPOCH = 2\n")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    docker = bin_dir / "docker"
    docker.write_text("""#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ["TEST_CALLS"], "a") as log:
    log.write(json.dumps(args) + "\\n")
if args[0] == "inspect":
    if args[-1] == os.environ.get("TEST_MISSING"): sys.exit(1)
    if "{{.State.Running}}" in args: print("true" if os.environ.get("TEST_RUNNING") == "1" else "false")
    elif "{{.Image}}" in args: print("sha256:" + "a" * 64)
    sys.exit(0)
if args[0] == "image":
    print("sha256:" + "a" * 64)
    sys.exit(0)
if args[0] == "run":
    assert "--env" not in args and "--env-file" not in args
    assert args[args.index("--network") + 1] == "none"
    assert args[args.index("--entrypoint") + 1] == "python"
    print("invalid" if os.environ.get("TEST_BAD_SITE") else "/usr/local/lib/python3.11/site-packages")
    sys.exit(0)
if args[0] == "exec":
    assert os.environ.get("TEST_RUNNING") == "1", "cannot exec a stopped application"
    if "python" in args: print("/usr/local/lib/python3.11/site-packages")
    sys.exit(0)
if args[0] == "cp":
    if args[-1] == "frontend:/docker-entrypoint.d":
        source = pathlib.Path(args[1].removesuffix("/.")) / "40-runtime-config.sh"
        assert source.stat().st_mode & 0o111, "nginx entrypoint must be executable before copying"
    sys.exit(0)
if args[0] == "compose": sys.exit(0)
raise SystemExit("unexpected command")
""")
    docker.chmod(0o755)
    pnpm = bin_dir / "pnpm"
    pnpm.write_text("#!/bin/sh\nexit 0\n")
    pnpm.chmod(0o755)
    return {
        "PATH": str(bin_dir) + os.pathsep + os.environ["PATH"],
        "TEST_PROJECT_ROOT": str(tmp_path),
        "TEST_CALLS": str(tmp_path / "calls"),
    }


def _run(tmp_path, env, *args):
    result = subprocess.run(
        ["bash", str(tmp_path / "scripts/new/hot-update.sh"), *args],
        env=env,
        text=True,
        capture_output=True,
        timeout=25,
        check=False,
    )
    calls_path = tmp_path / "calls"
    calls = (
        [json.loads(line) for line in calls_path.read_text().splitlines()]
        if calls_path.exists()
        else []
    )
    return result, calls


@pytest.mark.parametrize("running", [False, True])
def test_hot_update_copies_shared_contracts_to_every_python_service_before_restart(
    tmp_path, running
):
    env = _fixture(tmp_path)
    if running:
        env["TEST_RUNNING"] = "1"
    result, calls = _run(tmp_path, env, "--all")
    assert result.returncode == 0, result.stderr
    copies = [call for call in calls if call[0] == "cp"]
    contracts = [call for call in copies if "ai_gateway_contracts" in call[1]]
    assert {call[-1].split(":")[0] for call in contracts} == {
        "gateway",
        "knowledge",
        "knowledge-worker",
    }
    assert all(call[-1].endswith("site-packages/ai_gateway_contracts") for call in contracts)
    assert any(call[-1] == "frontend:/docker-entrypoint.d" for call in copies)
    first_restart = next(
        i for i, call in enumerate(calls) if call[0] == "compose" and "restart" in call
    )
    assert all(i < first_restart for i, call in enumerate(calls) if call[0] == "cp")
    restart = calls[first_restart]
    assert all(
        service in restart
        for service in ["gateway", "knowledge-service", "knowledge-worker", "frontend"]
    )
    if running:
        assert not any(call[0] == "run" for call in calls)
    else:
        probes = [call for call in calls if call[0] == "run"]
        assert len(probes) == 3
        assert all("sha256:" + "a" * 64 in call for call in probes)
        assert not any(call[0] == "exec" for call in calls)


def test_copy_only_keeps_stopped_services_stopped_and_does_not_probe_app_health(tmp_path):
    env = _fixture(tmp_path)
    result, calls = _run(tmp_path, env, "--all", "--no-restart")
    assert result.returncode == 0, result.stderr
    assert any(call[0] == "cp" for call in calls)
    assert not any(call[0] in {"exec", "compose", "health"} for call in calls)


@pytest.mark.parametrize("failure", ["TEST_MISSING", "TEST_BAD_SITE"])
def test_missing_container_or_invalid_site_packages_fails_before_start(tmp_path, failure):
    env = _fixture(tmp_path)
    env[failure] = "gateway" if failure == "TEST_MISSING" else "1"
    result, calls = _run(tmp_path, env, "--python")
    assert result.returncode != 0
    assert not any(call[0] in {"compose", "cp"} for call in calls)
