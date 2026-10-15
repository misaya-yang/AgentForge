from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

from scripts.rust.resource_lock import (
    LOCK_ROOT_NAME,
    REDACTED,
    LockError,
    ResourceLock,
    force_release,
    parse_runtime_identity,
    redact_command,
    run_locked,
)

ROOT = Path(__file__).resolve().parents[2]


def _git_repo(tmp_path: Path, name: str = "repo") -> Path:
    repo = tmp_path / name
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.email", "tests@example.invalid"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.name", "Rust Lock Tests"], cwd=repo, check=True)
    (repo / "README.md").write_text("fixture\n", encoding="utf-8")
    subprocess.run(["git", "add", "README.md"], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-qm", "fixture"], cwd=repo, check=True)
    return repo


def _owner_paths(lock: ResourceLock) -> tuple[Path, Path]:
    return lock.mutex_dir / "owner.json", lock.resource_dir / "owner.json"


def _rewrite_owner(lock: ResourceLock, **updates: object) -> dict[str, object]:
    mutex_owner, resource_owner = _owner_paths(lock)
    owner = json.loads(mutex_owner.read_text(encoding="utf-8"))
    owner.update(updates)
    payload = json.dumps(owner, indent=2, sort_keys=True) + "\n"
    mutex_owner.write_text(payload, encoding="utf-8")
    resource_owner.write_text(payload, encoding="utf-8")
    return owner


def _dead_pid() -> int:
    process = subprocess.Popen([sys.executable, "-c", "pass"])
    process.wait(timeout=5)
    return process.pid


def test_lock_receipt_heartbeat_and_release(tmp_path: Path) -> None:
    repo = _git_repo(tmp_path)
    lock = ResourceLock(
        "rust-build",
        command=["build", "runtime"],
        timeout_seconds=30,
        expected_end_condition="local build finishes",
        cwd=repo,
    )

    owner = lock.acquire()
    before = owner["heartbeat_time"]
    time.sleep(0.002)
    lock.heartbeat()
    mutex_owner, resource_owner = _owner_paths(lock)
    refreshed = json.loads(resource_owner.read_text(encoding="utf-8"))

    assert json.loads(mutex_owner.read_text(encoding="utf-8")) == refreshed
    assert refreshed["heartbeat_time"] > before
    assert refreshed["resource"] == "rust-build"
    assert refreshed["hostname"]
    assert refreshed["pid"] == os.getpid()
    assert refreshed["worktree_path"] == str(repo.resolve())
    assert refreshed["git_head"] == subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=repo, text=True
    ).strip()
    assert refreshed["command"] == ["build", "runtime"]
    assert refreshed["timeout_seconds"] == 30
    assert refreshed["expected_end_condition"] == "local build finishes"

    lock.release()
    assert not lock.mutex_dir.exists()
    assert not lock.resource_dir.exists()


def test_receipt_redacts_secrets_while_child_receives_original_command(tmp_path: Path) -> None:
    repo = _git_repo(tmp_path)
    output = tmp_path / "observed.json"
    probe = ResourceLock(
        "rust-build",
        command=["placeholder"],
        timeout_seconds=5,
        expected_end_condition="observation is written",
        cwd=repo,
    )
    owner_path = probe.resource_dir / "owner.json"
    program = (
        "import json,sys; from pathlib import Path; "
        "owner=json.loads(Path(sys.argv[1]).read_text()); "
        "Path(sys.argv[2]).write_text(json.dumps({'argv':sys.argv[3:], 'receipt':owner['command']}))"
    )
    command = [
        sys.executable,
        "-c",
        program,
        str(owner_path),
        str(output),
        "OPENAI_API_KEY=sk-live-secret",
        "--access-token",
        "token-secret",
        "--database-password=password-secret",
        "https://user:password@runtime.invalid/path",
    ]
    lock = ResourceLock(
        "rust-build",
        command=command,
        timeout_seconds=5,
        expected_end_condition="observation is written",
        cwd=repo,
    )

    assert run_locked(lock, command, heartbeat_seconds=0.1) == 0
    observed = json.loads(output.read_text(encoding="utf-8"))
    encoded_receipt = json.dumps(observed["receipt"])

    assert observed["argv"][-5:] == command[-5:]
    assert "sk-live-secret" not in encoded_receipt
    assert "token-secret" not in encoded_receipt
    assert "password-secret" not in encoded_receipt
    assert "user:password" not in encoded_receipt
    assert REDACTED in encoded_receipt
    assert redact_command(command) == observed["receipt"]


def test_runtime_identity_is_strict_and_recursively_redacted(tmp_path: Path) -> None:
    identity = parse_runtime_identity(
        json.dumps(
            {
                "compose_owner": "checkout-a",
                "images": {"runtime": "sha256:abc"},
                "api_token": "must-not-persist",
                "passwordHash": "must-not-persist-either",
                "credentials": ["credential-a"],
                "note": "API_TOKEN=inline-secret --password flag-secret",
                "endpoint": "https://owner:password@runtime.invalid/v1",
            }
        )
    )

    assert identity == {
        "compose_owner": "checkout-a",
        "images": {"runtime": "sha256:abc"},
        "api_token": REDACTED,
        "passwordHash": REDACTED,
        "credentials": REDACTED,
        "note": f"API_TOKEN={REDACTED} --password {REDACTED}",
        "endpoint": f"https://{REDACTED}@runtime.invalid/v1",
    }
    with pytest.raises(LockError, match="JSON object"):
        parse_runtime_identity('["not", "an", "object"]')
    with pytest.raises(LockError, match="non-finite"):
        parse_runtime_identity('{"rss": NaN}')
    with pytest.raises(LockError, match="duplicate key"):
        parse_runtime_identity('{"runtime": "a", "runtime": "b"}')

    repo = _git_repo(tmp_path, "identity-repo")
    lock = ResourceLock(
        "integration-runtime",
        command=["status"],
        timeout_seconds=30,
        expected_end_condition="status captured",
        runtime_identity=identity,
        cwd=repo,
    )
    owner = lock.acquire()
    try:
        assert owner["runtime_identity"] == identity
        assert "must-not-persist" not in json.dumps(owner)
    finally:
        lock.release()


def test_runtime_identity_cli_option_writes_only_redacted_metadata(tmp_path: Path) -> None:
    repo = _git_repo(tmp_path)
    output = tmp_path / "runtime-identity.json"
    owner_path = repo / ".git" / LOCK_ROOT_NAME / "integration-runtime" / "owner.json"
    program = (
        "import json,sys; from pathlib import Path; "
        "owner=json.loads(Path(sys.argv[1]).read_text()); "
        "Path(sys.argv[2]).write_text(json.dumps(owner['runtime_identity']))"
    )
    command = [
        str(ROOT / "scripts/rust/locks.sh"),
        "run",
        "--resource",
        "integration-runtime",
        "--timeout-seconds",
        "5",
        "--heartbeat-seconds",
        "0.1",
        "--expected-end-condition",
        "identity captured",
        "--runtime-identity-json",
        json.dumps(
            {
                "compose_owner": "checkout-a",
                "credential": "runtime-secret",
            }
        ),
        "--",
        sys.executable,
        "-c",
        program,
        str(owner_path),
        str(output),
    ]

    result = subprocess.run(command, cwd=repo, capture_output=True, text=True, check=False)
    identity = json.loads(output.read_text(encoding="utf-8"))

    assert result.returncode == 0, result.stderr
    assert identity == {"compose_owner": "checkout-a", "credential": REDACTED}
    assert "runtime-secret" not in output.read_text(encoding="utf-8")


def test_low_memory_resources_are_mutually_exclusive(tmp_path: Path) -> None:
    repo = _git_repo(tmp_path)
    first = ResourceLock(
        "integration-runtime",
        command=["integration"],
        timeout_seconds=30,
        expected_end_condition="integration finishes",
        cwd=repo,
    )
    second = ResourceLock(
        "rust-build",
        command=["build"],
        timeout_seconds=30,
        expected_end_condition="build finishes",
        cwd=repo,
    )
    first.acquire()
    try:
        with pytest.raises(LockError, match="lock is busy"):
            second.acquire()
    finally:
        first.release()


def test_separate_worktrees_share_the_git_common_lock(tmp_path: Path) -> None:
    repo = _git_repo(tmp_path)
    other = tmp_path / "other-worktree"
    subprocess.run(
        ["git", "worktree", "add", "-q", "-b", "fixture-other", str(other)],
        cwd=repo,
        check=True,
    )
    first = ResourceLock(
        "rust-build",
        command=["build"],
        timeout_seconds=30,
        expected_end_condition="build finishes",
        cwd=repo,
    )
    contender = ResourceLock(
        "integration-runtime",
        command=["integration"],
        timeout_seconds=30,
        expected_end_condition="integration finishes",
        cwd=other,
    )

    first.acquire()
    try:
        assert first.git_common_dir == contender.git_common_dir
        with pytest.raises(LockError, match="lock is busy"):
            contender.acquire()
    finally:
        first.release()


def test_force_release_requires_exact_token_reason_and_live_owner_override(
    tmp_path: Path,
) -> None:
    repo = _git_repo(tmp_path)
    held = ResourceLock(
        "rust-build",
        command=["env", "API_TOKEN=command-secret", "build"],
        timeout_seconds=30,
        expected_end_condition="build finishes",
        cwd=repo,
    )
    owner = held.acquire()

    with pytest.raises(LockError, match="exact owner token"):
        force_release(
            repo,
            resource="rust-build",
            owner_token="wrong-token",
            reason="operator recovery",
            allow_live_owner=True,
        )
    with pytest.raises(LockError, match="non-empty audit reason"):
        force_release(
            repo,
            resource="rust-build",
            owner_token=owner["owner_token"],
            reason="",
            allow_live_owner=True,
        )
    with pytest.raises(LockError, match="--allow-live-owner"):
        force_release(
            repo,
            resource="rust-build",
            owner_token=owner["owner_token"],
            reason="operator recovery",
        )

    result = subprocess.run(
        [
            str(ROOT / "scripts/rust/locks.sh"),
            "force-release",
            "--resource",
            "rust-build",
            "--owner-token",
            owner["owner_token"],
            "--reason",
            "operator recovery API_TOKEN=reason-secret",
            "--allow-live-owner",
        ],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    summary = json.loads(result.stdout)
    held.acquired = False
    held.owner = None

    assert result.returncode == 0, result.stderr
    assert summary["live_owner_override"] is True
    assert "command-secret" not in result.stdout
    assert "reason-secret" not in result.stdout
    assert not held.mutex_dir.exists()
    assert not held.resource_dir.exists()


def test_force_release_cross_host_and_known_ambiguous_receipt_are_explicit(
    tmp_path: Path,
) -> None:
    repo = _git_repo(tmp_path)
    held = ResourceLock(
        "integration-runtime",
        command=["integration"],
        timeout_seconds=30,
        expected_end_condition="integration finishes",
        cwd=repo,
    )
    owner = held.acquire()
    _rewrite_owner(held, hostname="other-host.invalid")
    _mutex_owner, resource_owner = _owner_paths(held)
    resource_owner.write_text("{not-json\n", encoding="utf-8")

    with pytest.raises(LockError, match="--allow-cross-host"):
        force_release(
            repo,
            resource="integration-runtime",
            owner_token=owner["owner_token"],
            reason="confirmed remote owner is gone",
        )
    summary = force_release(
        repo,
        resource="integration-runtime",
        owner_token=owner["owner_token"],
        reason="confirmed remote owner is gone",
        allow_cross_host=True,
    )
    held.acquired = False
    held.owner = None

    assert summary["cross_host_override"] is True
    assert summary["ambiguous_receipts_removed"] == 1
    assert not held.mutex_dir.exists()
    assert not held.resource_dir.exists()


def test_force_release_refuses_unknown_files_even_with_all_overrides(tmp_path: Path) -> None:
    repo = _git_repo(tmp_path)
    held = ResourceLock(
        "rust-build",
        command=["build"],
        timeout_seconds=30,
        expected_end_condition="build finishes",
        cwd=repo,
    )
    owner = held.acquire()
    unknown = held.resource_dir / "unexpected.data"
    unknown.write_text("do not delete\n", encoding="utf-8")

    with pytest.raises(LockError, match="unknown files"):
        force_release(
            repo,
            resource="rust-build",
            owner_token=owner["owner_token"],
            reason="operator recovery",
            allow_cross_host=True,
            allow_live_owner=True,
        )
    assert unknown.exists()
    unknown.unlink()
    held.release()


def test_only_same_host_dead_and_timed_out_owner_is_reaped(tmp_path: Path) -> None:
    repo = _git_repo(tmp_path)
    abandoned = ResourceLock(
        "rust-build",
        command=["old-build"],
        timeout_seconds=1,
        expected_end_condition="old build finishes",
        cwd=repo,
    )
    abandoned.acquire()
    _rewrite_owner(
        abandoned,
        pid=_dead_pid(),
        heartbeat_time="2000-01-01T00:00:00Z",
    )

    successor = ResourceLock(
        "integration-runtime",
        command=["integration"],
        timeout_seconds=30,
        expected_end_condition="integration finishes",
        cwd=repo,
    )
    successor.acquire()
    try:
        assert successor.resource_dir.name == "integration-runtime"
        assert not (successor.lock_root / "rust-build").exists()
    finally:
        successor.release()


@pytest.mark.parametrize(
    ("updates", "message"),
    [
        (
            {"heartbeat_time": "2000-01-01T00:00:00Z"},
            "owner process is alive",
        ),
        (
            {"pid": _dead_pid(), "heartbeat_time": "2999-01-01T00:00:00Z"},
            "has not exceeded",
        ),
        (
            {
                "pid": _dead_pid(),
                "hostname": "different-host.invalid",
                "heartbeat_time": "2000-01-01T00:00:00Z",
            },
            "cross-host",
        ),
    ],
)
def test_owner_is_not_reaped_without_all_same_host_dead_and_timeout_conditions(
    tmp_path: Path, updates: dict[str, object], message: str
) -> None:
    repo = _git_repo(tmp_path)
    held = ResourceLock(
        "rust-build",
        command=["build"],
        timeout_seconds=3600,
        expected_end_condition="build finishes",
        cwd=repo,
    )
    held.acquire()
    _rewrite_owner(held, **updates)
    contender = ResourceLock(
        "integration-runtime",
        command=["integration"],
        timeout_seconds=30,
        expected_end_condition="integration finishes",
        cwd=repo,
    )

    with pytest.raises(LockError, match=message):
        contender.acquire()
    assert held.mutex_dir.exists()
    assert held.resource_dir.exists()


def test_ambiguous_orphan_resource_is_not_deleted(tmp_path: Path) -> None:
    repo = _git_repo(tmp_path)
    probe = ResourceLock(
        "integration-runtime",
        command=["integration"],
        timeout_seconds=30,
        expected_end_condition="integration finishes",
        cwd=repo,
    )
    orphan = probe.git_common_dir / LOCK_ROOT_NAME / "rust-build"
    orphan.mkdir(parents=True)

    with pytest.raises(LockError, match="refusing ambiguous cleanup"):
        probe.acquire()
    assert orphan.exists()
    assert not probe.mutex_dir.exists()


def test_signal_cleanup_removes_both_receipts(tmp_path: Path) -> None:
    repo = _git_repo(tmp_path)
    command = [
        str(ROOT / "scripts/rust/locks.sh"),
        "run",
        "--resource",
        "rust-build",
        "--timeout-seconds",
        "5",
        "--heartbeat-seconds",
        "0.05",
        "--expected-end-condition",
        "child exits",
        "--",
        sys.executable,
        "-c",
        "import time; time.sleep(30)",
    ]
    process = subprocess.Popen(command, cwd=repo)
    lock_root = repo / ".git" / LOCK_ROOT_NAME
    deadline = time.monotonic() + 5
    while not (lock_root / "rust-build" / "owner.json").exists():
        if process.poll() is not None or time.monotonic() >= deadline:
            process.kill()
            raise AssertionError("lock supervisor did not acquire the resource")
        time.sleep(0.02)

    process.send_signal(signal.SIGTERM)
    assert process.wait(timeout=10) == 128 + signal.SIGTERM
    assert not (lock_root / ".low-memory").exists()
    assert not (lock_root / "rust-build").exists()


def test_heartbeat_failure_kills_child_that_ignores_sigterm_without_timeout_leak(
    tmp_path: Path,
) -> None:
    repo = _git_repo(tmp_path)
    ready = tmp_path / "child-ready"
    command = [
        sys.executable,
        "-c",
        (
            "import pathlib,signal,time; "
            "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
            f"pathlib.Path({str(ready)!r}).write_text('ready'); "
            "time.sleep(30)"
        ),
    ]
    lock = ResourceLock(
        "rust-build",
        command=command,
        timeout_seconds=5,
        expected_end_condition="child exits",
        cwd=repo,
    )

    def failed_heartbeat() -> None:
        deadline = time.monotonic() + 2
        while not ready.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        raise LockError("synthetic heartbeat failure")

    lock.heartbeat = failed_heartbeat  # type: ignore[method-assign]
    started = time.monotonic()
    with pytest.raises(LockError, match="child was terminated"):
        run_locked(lock, command, heartbeat_seconds=0.05)

    assert time.monotonic() - started < 6
    assert not lock.mutex_dir.exists()
    assert not lock.resource_dir.exists()


@pytest.mark.skipif(os.name != "posix", reason="process-group cleanup is POSIX-only")
def test_heartbeat_failure_kills_descendant_process_group(tmp_path: Path) -> None:
    repo = _git_repo(tmp_path)
    descendant_pid_path = tmp_path / "descendant-pid"
    descendant = (
        "import signal,time; "
        "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        "time.sleep(30)"
    )
    command = [
        sys.executable,
        "-c",
        (
            "import pathlib,signal,subprocess,sys,time; "
            "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
            f"child=subprocess.Popen([sys.executable, '-c', {descendant!r}]); "
            f"pathlib.Path({str(descendant_pid_path)!r}).write_text(str(child.pid)); "
            "time.sleep(30)"
        ),
    ]
    lock = ResourceLock(
        "rust-build",
        command=command,
        timeout_seconds=5,
        expected_end_condition="process tree exits",
        cwd=repo,
    )

    def failed_heartbeat() -> None:
        deadline = time.monotonic() + 2
        while not descendant_pid_path.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        raise LockError("synthetic heartbeat failure")

    lock.heartbeat = failed_heartbeat  # type: ignore[method-assign]
    with pytest.raises(LockError, match="child was terminated"):
        run_locked(lock, command, heartbeat_seconds=0.05)

    descendant_pid = int(descendant_pid_path.read_text(encoding="utf-8"))
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        try:
            os.kill(descendant_pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.02)
    else:
        pytest.fail("descendant survived lock-supervisor cleanup")
