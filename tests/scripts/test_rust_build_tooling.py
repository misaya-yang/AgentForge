from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

from scripts.harness.rust_changed_crate_gate import cargo_environment, cargo_test_command

ROOT = Path(__file__).resolve().parents[2]


def _init_git(repo: Path) -> None:
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.email", "tests@example.invalid"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.name", "Rust Build Tests"], cwd=repo, check=True)
    subprocess.run(["git", "add", "."], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-qm", "fixture"], cwd=repo, check=True)


def _fixture_repo(tmp_path: Path, *, locked: bool = True) -> tuple[Path, Path]:
    repo = tmp_path / "repo"
    source = tmp_path / "source"
    (repo / "scripts").mkdir(parents=True)
    shutil.copytree(ROOT / "scripts/rust", repo / "scripts/rust")
    harness = repo / "scripts/harness"
    harness.mkdir()
    for name in (
        "build_agent_runtime_image.sh",
        "build_agent_capability_worker_image.sh",
        "build_agent_runtime_source_image.sh",
    ):
        path = harness / name
        path.write_text("#!/usr/bin/env bash\nexit 99\n", encoding="utf-8")
        path.chmod(0o755)
    (harness / "agent_runtime_supply_chain.py").write_text(
        "raise SystemExit(98)\n", encoding="utf-8"
    )
    deploy = repo / "deploy/agent-runtime-source"
    deploy.mkdir(parents=True)
    cargo_line = (
        "RUN cargo build --locked -p fixture\n" if locked else "RUN cargo build -p fixture\n"
    )
    for name in ("Dockerfile.runtime", "Dockerfile.capability-worker", "Dockerfile"):
        (deploy / name).write_text(cargo_line, encoding="utf-8")
    (deploy / "lock.json").write_text("{}\n", encoding="utf-8")
    _init_git(repo)

    source.mkdir(parents=True)
    (source / "README.md").write_text("controlled source\n", encoding="utf-8")
    _init_git(source)
    return repo, source


def _dry_run(repo: Path, source: Path, *extra: str, minimum_memory: str = "1"):
    environment = {
        **os.environ,
        "AI_PLATFORM_AGENT_RUNTIME_SOURCE": str(source),
        "AI_PLATFORM_RUST_MIN_AVAILABLE_MEMORY_MB": minimum_memory,
        "AI_PLATFORM_RUST_MIN_FREE_DISK_MB": "1",
    }
    return subprocess.run(
        ["bash", "scripts/rust/build-update.sh", "--dry-run", *extra],
        cwd=repo,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
        timeout=20,
    )


def _install_source_locked_build_fakes(repo: Path) -> None:
    lock = {
        "release_state": "local_source_locked",
        "oci": {
            "artifacts": {
                artifact: {
                    "candidate_start_allowed": False,
                    "image_digest": None,
                    "image_ref": None,
                }
                for artifact in ("agent_runtime", "capability_worker", "app_server")
            }
        },
    }
    (repo / "deploy/agent-runtime-source/lock.json").write_text(json.dumps(lock), encoding="utf-8")
    supply_chain = repo / "scripts/harness/agent_runtime_supply_chain.py"
    supply_chain.write_text(
        """from __future__ import annotations

import json
import os
import sys
from pathlib import Path

repo = Path(sys.argv[sys.argv.index("--repo-root") + 1])
lock_path = Path(sys.argv[sys.argv.index("--lock") + 1])
command = sys.argv[1]
artifact = None
if command == "validate" and "--require-artifact" in sys.argv:
    artifact = sys.argv[sys.argv.index("--require-artifact") + 1]
if command not in {"validate", "verify-local-images"}:
    raise SystemExit(2)
step = f"validate:{artifact or 'source'}" if command == "validate" else command
with (repo / "build-trace.log").open("a", encoding="utf-8") as trace:
    trace.write(f"{step}\\n")
if os.environ.get("FAKE_SUPPLY_CHAIN_FAIL_STEP") == step:
    raise SystemExit(1)
lock = json.loads(lock_path.read_text(encoding="utf-8"))
artifacts = [artifact] if artifact is not None else []
if command == "verify-local-images":
    artifacts = ["agent_runtime", "capability_worker"]
for selected_artifact in artifacts:
    selected = lock["oci"]["artifacts"][selected_artifact]
    if not selected["candidate_start_allowed"] or not selected["image_digest"]:
        raise SystemExit(1)
""",
        encoding="utf-8",
    )
    artifacts = {
        "build_agent_runtime_image.sh": "agent_runtime",
        "build_agent_capability_worker_image.sh": "capability_worker",
        "build_agent_runtime_source_image.sh": "app_server",
    }
    for name, artifact in artifacts.items():
        path = repo / "scripts/harness" / name
        path.write_text(
            f"""#!/usr/bin/env bash
set -euo pipefail
repo_root="$(git rev-parse --show-toplevel)"
python3 - "$repo_root" <<'PY'
import json
import sys
from pathlib import Path

repo = Path(sys.argv[1])
lock_path = repo / "deploy/agent-runtime-source/lock.json"
lock = json.loads(lock_path.read_text(encoding="utf-8"))
artifact = {artifact!r}
selected = lock["oci"]["artifacts"][artifact]
selected["candidate_start_allowed"] = True
selected["image_digest"] = "sha256:" + "1" * 64
selected["image_ref"] = "local-image://" + selected["image_digest"]
lock["release_state"] = "local_image_locked"
lock_path.write_text(json.dumps(lock), encoding="utf-8")
with (repo / "build-trace.log").open("a", encoding="utf-8") as trace:
    trace.write(f"build:{{artifact}}\\n")
PY
""",
            encoding="utf-8",
        )
        path.chmod(0o755)
    subprocess.run(["git", "add", "."], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-qm", "install source-lock fakes"], cwd=repo, check=True)


def test_changed_crate_command_is_locked_and_forces_one_job(tmp_path: Path) -> None:
    manifest = tmp_path / "Cargo.toml"

    package = cargo_test_command("cargo", "crate-a", manifest)
    workspace = cargo_test_command("cargo", "@workspace", manifest)

    assert package == [
        "cargo",
        "test",
        "--locked",
        "-p",
        "crate-a",
        "--manifest-path",
        str(manifest),
    ]
    assert workspace == [
        "cargo",
        "test",
        "--locked",
        "--workspace",
        "--manifest-path",
        str(manifest),
    ]
    assert cargo_environment()["CARGO_BUILD_JOBS"] == "1"


def test_all_local_build_scripts_default_to_one_job() -> None:
    expected = {
        "build_agent_runtime_image.sh": (
            'cargo_jobs="${AI_PLATFORM_AGENT_RUNTIME_CARGO_JOBS:-${CARGO_BUILD_JOBS:-1}}"'
        ),
        "build_agent_runtime_source_image.sh": (
            'cargo_jobs="${AI_PLATFORM_AGENT_RUNTIME_CARGO_JOBS:-${CARGO_BUILD_JOBS:-1}}"'
        ),
        "build_agent_capability_worker_image.sh": (
            'cargo_jobs="${AI_PLATFORM_CAPABILITY_WORKER_CARGO_JOBS:-${CARGO_BUILD_JOBS:-1}}"'
        ),
    }
    for name, declaration in expected.items():
        assert declaration in (ROOT / "scripts/harness" / name).read_text(encoding="utf-8")


def test_build_update_dry_run_is_locked_local_only_and_calls_real_entries(tmp_path: Path) -> None:
    repo, source = _fixture_repo(tmp_path)

    result = _dry_run(repo, source, "--artifact", "all")

    assert result.returncode == 0, result.stderr
    assert "jobs=1 cargo_mode=--locked" in result.stdout
    assert "LOCAL-ONLY" in result.stdout
    assert "does not publish or claim multi-arch artifacts" in result.stdout
    assert "build_agent_runtime_image.sh" in result.stdout
    assert "build_agent_capability_worker_image.sh" in result.stdout
    assert "--require-artifact agent_runtime" in result.stdout
    assert "--require-artifact capability_worker" in result.stdout
    source_validation = result.stdout.index("agent_runtime_supply_chain.py validate")
    runtime_build = result.stdout.index("build_agent_runtime_image.sh")
    runtime_validation = result.stdout.index("--require-artifact agent_runtime")
    worker_validation = result.stdout.index("--require-artifact capability_worker")
    pair_validation = result.stdout.index("agent_runtime_supply_chain.py verify-local-images")
    assert source_validation < runtime_build < runtime_validation < worker_validation < pair_validation
    assert "DRY RUN" in result.stdout
    assert not (repo / ".git/ai-gateway-locks/.low-memory").exists()
    assert not (repo / ".git/ai-gateway-locks/rust-build").exists()


def test_build_update_builds_before_requiring_empty_source_lock_artifacts(tmp_path: Path) -> None:
    repo, source = _fixture_repo(tmp_path)
    _install_source_locked_build_fakes(repo)
    environment = {
        **os.environ,
        "AI_PLATFORM_AGENT_RUNTIME_SOURCE": str(source),
        "AI_PLATFORM_RUST_MIN_AVAILABLE_MEMORY_MB": "1",
        "AI_PLATFORM_RUST_MIN_FREE_DISK_MB": "1",
    }

    result = subprocess.run(
        ["bash", "scripts/rust/build-update.sh", "--artifact", "all"],
        cwd=repo,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
        timeout=20,
    )

    assert result.returncode == 0, result.stderr
    assert (repo / "build-trace.log").read_text(encoding="utf-8").splitlines() == [
        "validate:source",
        "build:agent_runtime",
        "build:capability_worker",
        "validate:agent_runtime",
        "validate:capability_worker",
        "verify-local-images",
    ]
    updated = json.loads(
        (repo / "deploy/agent-runtime-source/lock.json").read_text(encoding="utf-8")
    )
    assert updated["release_state"] == "local_image_locked"
    assert all(
        updated["oci"]["artifacts"][artifact]["candidate_start_allowed"]
        for artifact in ("agent_runtime", "capability_worker")
    )


@pytest.mark.parametrize(
    ("failed_step", "expected_trace"),
    (
        ("validate:source", ["validate:source"]),
        (
            "verify-local-images",
            [
                "validate:source",
                "build:agent_runtime",
                "build:capability_worker",
                "validate:agent_runtime",
                "validate:capability_worker",
                "verify-local-images",
            ],
        ),
    ),
)
def test_build_update_stops_on_source_or_pair_verification_failure(
    tmp_path: Path, failed_step: str, expected_trace: list[str]
) -> None:
    repo, source = _fixture_repo(tmp_path)
    _install_source_locked_build_fakes(repo)
    result = subprocess.run(
        ["bash", "scripts/rust/build-update.sh", "--artifact", "all"],
        cwd=repo,
        env={
            **os.environ,
            "AI_PLATFORM_AGENT_RUNTIME_SOURCE": str(source),
            "AI_PLATFORM_RUST_MIN_AVAILABLE_MEMORY_MB": "1",
            "AI_PLATFORM_RUST_MIN_FREE_DISK_MB": "1",
            "FAKE_SUPPLY_CHAIN_FAIL_STEP": failed_step,
        },
        capture_output=True,
        text=True,
        check=False,
        timeout=20,
    )

    assert result.returncode != 0
    assert "Rust build completed" not in result.stdout
    assert (repo / "build-trace.log").read_text(encoding="utf-8").splitlines() == expected_trace


def test_build_update_allows_only_explicit_documentation_and_receipt_wip(
    tmp_path: Path,
) -> None:
    repo, source = _fixture_repo(tmp_path)
    allowed = (
        repo / "docs/harness/runtime-and-secrets.md",
        repo / "reports/receipts/ARC-02B.json",
        repo / "deploy/runbooks/convergence/HANDOFF.md",
        repo / "deploy/runbooks/convergence/loop-state.json",
        repo / "deploy/runbooks/convergence/work-packages.yml",
        repo / "deploy/runbooks/convergence/receipts/ARC-02B.yml",
    )
    for path in allowed:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("wip\n", encoding="utf-8")

    result = _dry_run(repo, source, "--artifact", "runtime")

    assert result.returncode == 0, result.stderr


def test_build_update_rejects_dirty_rust_and_build_inputs(tmp_path: Path) -> None:
    dirty_paths = (
        "rust/agent-runtime-overlay/kernel-rs/src/lib.rs",
        "deploy/agent-runtime-source/lock.json",
        "scripts/harness/build_agent_runtime_image.sh",
    )
    for index, relative in enumerate(dirty_paths):
        repo, source = _fixture_repo(tmp_path / str(index))
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        payload = path.read_text(encoding="utf-8") + "dirty\n" if path.exists() else "dirty\n"
        path.write_text(payload, encoding="utf-8")

        result = _dry_run(repo, source, "--artifact", "runtime")

        assert result.returncode != 0
        assert relative in result.stderr
        assert "dirty non-documentation/build-input paths" in result.stderr


def test_deploy_build_separates_rust_build_from_integration_runtime_lock() -> None:
    makefile = (ROOT / "Makefile").read_text(encoding="utf-8")
    target = makefile.split("\ndeploy-build:", 1)[1].split("\ndeploy-cn:", 1)[0]
    deploy = (ROOT / "scripts/new/deploy.sh").read_text(encoding="utf-8")

    rust_build = target.index("scripts/rust/build-update.sh --artifact all")
    integration_lock = target.index("--resource integration-runtime")
    deploy_command = target.index("bash $(SCRIPTS)/deploy.sh")
    assert rust_build < integration_lock < deploy_command
    assert "--resource rust-build" not in target
    assert "AI_PLATFORM_RUST_IMAGES_PREBUILT=1" in target
    assert "AI_PLATFORM_RUST_IMAGES_PREBUILT:-0" in deploy
    assert 'AGENT_CAPABILITY_WORKER_IMAGE="$(agent_capability_worker_image_tag)"' in deploy


def test_build_update_fails_closed_on_resource_and_locked_entrypoint_preflight(
    tmp_path: Path,
) -> None:
    low_resource_repo, low_resource_source = _fixture_repo(tmp_path / "low")
    low_resource = _dry_run(
        low_resource_repo,
        low_resource_source,
        "--artifact",
        "runtime",
        minimum_memory="999999999",
    )
    assert low_resource.returncode != 0
    assert "below required" in low_resource.stderr
    assert not (low_resource_repo / ".git/ai-gateway-locks/.low-memory").exists()

    unlocked_repo, unlocked_source = _fixture_repo(tmp_path / "unlocked", locked=False)
    unlocked = _dry_run(unlocked_repo, unlocked_source, "--artifact", "runtime")
    assert unlocked.returncode != 0
    assert "not Cargo --locked" in unlocked.stderr
    assert not (unlocked_repo / ".git/ai-gateway-locks/.low-memory").exists()


def test_rust_shell_entrypoints_are_syntax_valid() -> None:
    scripts = [
        ROOT / "scripts/rust/locks.sh",
        ROOT / "scripts/rust/build-update.sh",
        ROOT / "scripts/harness/build_agent_runtime_image.sh",
        ROOT / "scripts/harness/build_agent_runtime_source_image.sh",
        ROOT / "scripts/harness/build_agent_capability_worker_image.sh",
        ROOT / "scripts/new/deploy.sh",
    ]
    for script in scripts:
        result = subprocess.run(
            ["bash", "-n", str(script)], capture_output=True, text=True, check=False
        )
        assert result.returncode == 0, f"{script}: {result.stderr}"
