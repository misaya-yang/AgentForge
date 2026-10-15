from __future__ import annotations

import threading
import time
from pathlib import Path

import pytest

from local_node.errors import ProcessPolicyError
from local_node.grants import DirectoryGrantStore
from local_node.ledger import ActionLedger
from local_node.processes import ProcessPolicy, ProcessRequest, ProcessRunner


def _runner(
    tmp_path,
    executable: str,
    platform_signature_verifier,
    trusted_local_approval_verifier,
):
    root = tmp_path / "workspace"
    root.mkdir()
    grants = DirectoryGrantStore()
    grant = grants.issue(
        root,
        frozenset({"read"}),
        tenant_id="tenant-a",
        user_id="user-a",
    )
    ledger = ActionLedger(
        tmp_path / "state" / "ledger.sqlite",
        platform_signature_verifier=platform_signature_verifier,
        trusted_local_approval_verifier=trusted_local_approval_verifier,
    )
    policy = ProcessPolicy(
        allowed_executables=frozenset({Path(executable)}),
        max_timeout_seconds=10,
        allow_inherited_network=True,
    )
    return grant, ledger, ProcessRunner(grants, policy, ledger)


def test_process_uses_argv_clean_environment_and_streams(
    tmp_path,
    action_factory,
    monkeypatch,
    platform_signature_verifier,
    trusted_local_approval_verifier,
):
    env_bin = "/usr/bin/env"
    if not Path(env_bin).exists():
        pytest.skip("env executable unavailable")
    monkeypatch.setenv("OPENAI_API_KEY", "SECRET_CANARY_PROCESS")
    grant, ledger, runner = _runner(
        tmp_path, env_bin, platform_signature_verifier, trusted_local_approval_verifier
    )
    request = ProcessRequest(
        (env_bin,), grant.grant_id, timeout_seconds=5, network_policy="inherit"
    )
    _, target = runner.cwd_snapshot_digest(request, tenant_id="tenant-a", user_id="user-a")
    action = action_factory("process.run", request.normalized_arguments(), target)
    chunks = []
    result = runner.run(
        request, action, on_output=lambda channel, data: chunks.append((channel, data))
    )
    assert result.exit_code == 0
    assert "OPENAI_API_KEY" not in result.stdout
    assert "SECRET_CANARY_PROCESS" not in result.stdout
    assert chunks
    assert ledger.verify_integrity()


def test_unallowlisted_process_and_secret_env_are_denied(
    tmp_path, action_factory, platform_signature_verifier, trusted_local_approval_verifier
):
    executable = "/usr/bin/env"
    if not Path(executable).exists():
        pytest.skip("env executable unavailable")
    grant, _, runner = _runner(
        tmp_path, executable, platform_signature_verifier, trusted_local_approval_verifier
    )
    denied = ProcessRequest(
        ("/bin/echo", "hi"), grant.grant_id, timeout_seconds=5, network_policy="inherit"
    )
    _, target = runner.cwd_snapshot_digest(denied, tenant_id="tenant-a", user_id="user-a")
    action = action_factory("process.run", denied.normalized_arguments(), target)
    with pytest.raises(ProcessPolicyError):
        runner.run(denied, action)


def test_running_process_can_be_cancelled(
    tmp_path, action_factory, platform_signature_verifier, trusted_local_approval_verifier
):
    sleep_bin = "/bin/sleep"
    grant, _, runner = _runner(
        tmp_path, sleep_bin, platform_signature_verifier, trusted_local_approval_verifier
    )
    request = ProcessRequest(
        (sleep_bin, "5"), grant.grant_id, timeout_seconds=5, network_policy="inherit"
    )
    _, target = runner.cwd_snapshot_digest(request, tenant_id="tenant-a", user_id="user-a")
    action = action_factory("process.run", request.normalized_arguments(), target)
    holder = []
    thread = threading.Thread(target=lambda: holder.append(runner.run(request, action)))
    thread.start()
    deadline = time.time() + 2
    while action.action_id not in runner._processes and time.time() < deadline:
        time.sleep(0.01)
    assert runner.cancel(action.action_id)
    thread.join(2)
    assert not thread.is_alive()
    assert holder[0].status == "cancelled"


def test_network_deny_fails_closed_without_sandbox_backend(
    tmp_path, action_factory, platform_signature_verifier, trusted_local_approval_verifier
):
    env_bin = "/usr/bin/env"
    if not Path(env_bin).exists():
        pytest.skip("env executable unavailable")
    grant, _, runner = _runner(
        tmp_path, env_bin, platform_signature_verifier, trusted_local_approval_verifier
    )
    request = ProcessRequest((env_bin,), grant.grant_id, timeout_seconds=5)
    _, target = runner.cwd_snapshot_digest(request, tenant_id="tenant-a", user_id="user-a")
    action = action_factory("process.run", request.normalized_arguments(), target)
    with pytest.raises(ProcessPolicyError, match="sandbox backend"):
        runner.run(request, action)
