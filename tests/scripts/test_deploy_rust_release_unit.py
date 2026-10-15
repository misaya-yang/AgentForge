"""Execute deploy orchestration with command stubs; never contacts Docker."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest


def _deploy_fixture(tmp_path: Path) -> dict[str, str]:
    scripts = tmp_path / "scripts"
    (scripts / "new").mkdir(parents=True)
    (scripts / "harness").mkdir()
    (scripts / "deploy").mkdir()
    (scripts / "new/deploy.sh").write_text(Path("scripts/new/deploy.sh").read_text())
    (scripts / "new/common.sh").write_text("""set -euo pipefail
PROJECT_ROOT="$TEST_PROJECT_ROOT"
SCRIPT_DIR="$PROJECT_ROOT/scripts/new"
ENV_FILE="$PROJECT_ROOT/test.env"
load_env() { :; }
require_docker() { :; }
require_env_file() { :; }
log_step() { :; }
log_info() { :; }
log_warn() { :; }
log_error() { echo "$1" >&2; }
log_success() { :; }
get_compose_cmd() { echo "bash $PROJECT_ROOT/compose.sh"; }
agent_runtime_kernel_revision() { echo locked-revision; }
agent_runtime_image_tag() { echo runtime:locked; }
agent_capability_worker_image_tag() { echo worker:locked; }
topology_service_ids() { echo 'gateway agent-runtime agent-capability-worker'; }
topology_service_present() { return 1; }
assert_compose_owner() { echo owner-checked >> "$TEST_CALLS"; }
assert_runtime_release_unit_locked() {
    echo "verified:$1:$2" >> "$TEST_CALLS"
    [ "${TEST_PAIR_FAIL:-0}" = 0 ]
}
wait_for_healthy() { :; }
check_topology_cardinality() { :; }
""")
    (scripts / "deploy/topology_modes.py").write_text("raise SystemExit(0)\n")
    (scripts / "harness/agent_runtime_supply_chain.py").write_text("raise SystemExit(0)\n")
    validator = scripts / "new/validate-env.sh"
    validator.write_text("#!/bin/sh\nexit 0\n")
    validator.chmod(0o755)
    (tmp_path / "compose.sh").write_text('echo "compose:$*" >> "$TEST_CALLS"\n')
    (scripts / "harness/build_agent_runtime_image.sh").write_text(
        'echo runtime-build >> "$TEST_CALLS"\necho AI_PLATFORM_AGENT_RUNTIME_IMAGE_TAG=runtime:built\n'
    )
    (scripts / "harness/build_agent_capability_worker_image.sh").write_text(
        'echo worker-build >> "$TEST_CALLS"\n[ "${TEST_WORKER_FAIL:-0}" = 0 ] || exit 1\necho AGENT_CAPABILITY_WORKER_IMAGE_TAG=worker:built\n'
    )
    return {
        "PATH": os.environ["PATH"],
        "TEST_PROJECT_ROOT": str(tmp_path),
        "TEST_CALLS": str(tmp_path / "calls"),
        "VCS_REF": "fixture",
        "AI_PLATFORM_AGENT_RUNTIME_SOURCE": str(tmp_path),
    }


@pytest.mark.parametrize("prebuilt", [False, True])
def test_deploy_build_uses_a_verified_runtime_worker_pair_before_start(tmp_path, prebuilt):
    env = _deploy_fixture(tmp_path)
    if prebuilt:
        env["AI_PLATFORM_RUST_IMAGES_PREBUILT"] = "1"
    result = subprocess.run(
        ["bash", str(tmp_path / "scripts/new/deploy.sh"), "--app", "--build", "--no-migrate"],
        env=env,
        text=True,
        capture_output=True,
        timeout=10,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    calls = (tmp_path / "calls").read_text().splitlines()
    assert calls[0] == "owner-checked"
    if prebuilt:
        assert "runtime-build" not in calls and "worker-build" not in calls
        verification = "verified:runtime:locked:worker:locked"
    else:
        assert calls.index("runtime-build") < calls.index("worker-build")
        verification = "verified:runtime:built:worker:built"
    assert calls.index(verification) < next(i for i, line in enumerate(calls) if " up -d " in line)


@pytest.mark.parametrize("failure", ["TEST_WORKER_FAIL", "TEST_PAIR_FAIL"])
def test_worker_build_or_pair_validation_failure_never_starts_or_stops_apps(tmp_path, failure):
    env = _deploy_fixture(tmp_path)
    env[failure] = "1"
    result = subprocess.run(
        ["bash", str(tmp_path / "scripts/new/deploy.sh"), "--app", "--build", "--no-migrate"],
        env=env,
        text=True,
        capture_output=True,
        timeout=10,
        check=False,
    )
    assert result.returncode != 0
    calls = (tmp_path / "calls").read_text()
    assert "worker-build" in calls
    assert " up -d " not in calls and " stop " not in calls


@pytest.mark.parametrize("revision", [None, "locked-revision", "old-revision"])
def test_deploy_exports_the_source_revision_or_rejects_explicit_stale_revision(tmp_path, revision):
    env = _deploy_fixture(tmp_path)
    if revision is not None:
        env["AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION"] = revision
    (tmp_path / "compose.sh").write_text(
        'echo "revision:$AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION compose:$*" >> "$TEST_CALLS"\n'
    )
    result = subprocess.run(
        ["bash", str(tmp_path / "scripts/new/deploy.sh"), "--app", "--build", "--no-migrate"],
        env=env,
        text=True,
        capture_output=True,
        timeout=10,
        check=False,
    )
    calls_path = tmp_path / "calls"
    calls = calls_path.read_text() if calls_path.exists() else ""
    if revision == "old-revision":
        assert result.returncode == 2
        assert "KERNEL_REVISION does not match" in result.stderr
        assert "runtime-build" not in calls and "compose:" not in calls
    else:
        assert result.returncode == 0, result.stderr
        assert "revision:locked-revision compose:" in calls
