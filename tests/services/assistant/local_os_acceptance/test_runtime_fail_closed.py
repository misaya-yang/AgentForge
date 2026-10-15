"""Offline runtime safety tests for OS-A09/A12/A16/A17/A23."""

from __future__ import annotations

import os
from pathlib import Path

import pytest
from local_node.computer import (
    ComputerController,
    ComputerScope,
    HealthStatus,
    MacOSComputerDriver,
)
from local_node.errors import BoundaryViolation, DriverUnavailable, ProcessPolicyError
from local_node.grants import DirectoryGrantStore
from local_node.ledger import ActionLedger
from local_node.models import ActionStatus
from local_node.processes import ProcessPolicy, ProcessRequest, ProcessRunner
from local_node.service import LocalServiceBinding, OutboundControlPlane


def test_local_listener_is_loopback_only() -> None:
    for unsafe in ("0.0.0.0", "::", "192.168.1.20", "localhost"):
        with pytest.raises(BoundaryViolation):
            LocalServiceBinding(host=unsafe).validate()

    LocalServiceBinding(host="127.0.0.1").validate()
    LocalServiceBinding(host="::1").validate()


def test_outbound_control_plane_requires_tls_and_never_embeds_credentials() -> None:
    for unsafe in (
        "http://control.example.test",
        "ws://control.example.test",
        "https://user:password@control.example.test",
        "https://control.example.test/#secret",
    ):
        with pytest.raises(BoundaryViolation):
            OutboundControlPlane(unsafe).validate()

    OutboundControlPlane("wss://control.example.test/local-node").validate()


def test_computer_driver_is_unavailable_without_real_backend_and_permissions() -> None:
    driver = MacOSComputerDriver(platform_name="Darwin")

    assert driver.doctor().status is HealthStatus.NEEDS_ACTION
    with pytest.raises(DriverUnavailable):
        driver.observe("com.example.TextEditor")


def test_non_macos_driver_reports_unsupported_instead_of_fake_success() -> None:
    driver = MacOSComputerDriver(platform_name="Linux")

    health = driver.doctor()
    assert health.status is HealthStatus.UNSUPPORTED
    assert health.accessibility is HealthStatus.UNSUPPORTED
    assert health.screen_recording is HealthStatus.UNSUPPORTED


def test_process_network_deny_fails_closed_without_sandbox(
    tmp_path: Path,
    local_action_factory,
    platform_signature_verifier,
    trusted_local_approval_verifier,
) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    grants = DirectoryGrantStore()
    grant = grants.issue(
        workspace,
        frozenset({"read"}),
        tenant_id="tenant-a",
        user_id="user-a",
    )
    ledger = ActionLedger(
        tmp_path / "process-ledger.sqlite3",
        platform_signature_verifier=platform_signature_verifier,
        trusted_local_approval_verifier=trusted_local_approval_verifier,
    )
    executable = Path("/usr/bin/true")
    if not executable.exists():
        executable = Path("/bin/true")
    policy = ProcessPolicy(allowed_executables=frozenset({executable}))
    runner = ProcessRunner(grants, policy, ledger)
    request = ProcessRequest(
        argv=(str(executable),),
        grant_id=grant.grant_id,
        network_policy="deny",
    )
    action = local_action_factory(
        capability="process.run",
        tool_name="local_process_run",
        operation="process.run",
        normalized_arguments=request.normalized_arguments(),
        capability_lease_id=grant.grant_id,
        resource_refs=(grant.grant_id, request.cwd),
    )

    try:
        with pytest.raises(ProcessPolicyError, match="sandbox"):
            runner.run(request, action)
        assert ledger.get(action.action_id) is None
    finally:
        ledger.close()


def test_process_never_accepts_secret_like_environment_even_if_allowlisted(
    tmp_path: Path,
    local_action_factory,
    platform_signature_verifier,
    trusted_local_approval_verifier,
) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    grants = DirectoryGrantStore()
    grant = grants.issue(
        workspace,
        frozenset({"read"}),
        tenant_id="tenant-a",
        user_id="user-a",
    )
    ledger = ActionLedger(
        tmp_path / "env-ledger.sqlite3",
        platform_signature_verifier=platform_signature_verifier,
        trusted_local_approval_verifier=trusted_local_approval_verifier,
    )
    executable = Path("/usr/bin/true")
    if not executable.exists():
        executable = Path("/bin/true")
    runner = ProcessRunner(
        grants,
        ProcessPolicy(
            allowed_executables=frozenset({executable}),
            allowed_env_names=frozenset({"OPENAI_API_KEY"}),
            allow_inherited_network=True,
        ),
        ledger,
    )
    request = ProcessRequest(
        argv=(str(executable),),
        grant_id=grant.grant_id,
        env={"OPENAI_API_KEY": "sk-secret-canary"},
        network_policy="inherit",
    )
    action = local_action_factory(
        capability="process.run",
        tool_name="local_process_run",
        operation="process.run",
        normalized_arguments=request.normalized_arguments(),
        capability_lease_id=grant.grant_id,
        resource_refs=(grant.grant_id, request.cwd),
    )

    try:
        with pytest.raises(ProcessPolicyError, match="secret-like"):
            runner.run(request, action)
        assert ledger.get(action.action_id) is None
    finally:
        ledger.close()


def test_process_receipt_does_not_inherit_ambient_host_secret(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    local_action_factory,
    platform_signature_verifier,
    trusted_local_approval_verifier,
) -> None:
    """A harmless env binary proves subprocess inherits only the minimal map."""

    executable = Path("/usr/bin/env")
    if not executable.exists():
        pytest.skip("platform has no /usr/bin/env binary")
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    grants = DirectoryGrantStore()
    grant = grants.issue(
        workspace,
        frozenset({"read"}),
        tenant_id="tenant-a",
        user_id="user-a",
    )
    ledger = ActionLedger(
        tmp_path / "ambient-ledger.sqlite3",
        platform_signature_verifier=platform_signature_verifier,
        trusted_local_approval_verifier=trusted_local_approval_verifier,
    )
    runner = ProcessRunner(
        grants,
        ProcessPolicy(
            allowed_executables=frozenset({executable}),
            allow_inherited_network=True,
        ),
        ledger,
    )
    canary = "sk-ambient-host-canary-123456"
    monkeypatch.setenv("OPENAI_API_KEY", canary)
    request = ProcessRequest(
        argv=(str(executable),),
        grant_id=grant.grant_id,
        network_policy="inherit",
    )
    _cwd, cwd_digest = runner.cwd_snapshot_digest(
        request,
        tenant_id="tenant-a",
        user_id="user-a",
    )
    action = local_action_factory(
        capability="process.run",
        tool_name="local_process_run",
        operation="process.run",
        normalized_arguments=request.normalized_arguments(),
        target_snapshot_digest=cwd_digest,
        approved=True,
        capability_lease_id=grant.grant_id,
        resource_refs=(grant.grant_id, request.cwd),
    )

    try:
        result = runner.run(request, action)
        assert result.status == ActionStatus.SUCCEEDED.value
        assert canary not in result.stdout
        assert "OPENAI_API_KEY" not in result.stdout
        assert set(result.stdout.splitlines()) <= {
            f"PATH={runner.policy.path}",
            "LANG=C.UTF-8",
            "LC_ALL=C.UTF-8",
        }
        assert canary.encode() not in ledger.path.read_bytes()
    finally:
        ledger.close()


def test_computer_controller_never_acquires_lease_when_driver_is_not_ready(
    tmp_path: Path,
    platform_signature_verifier,
    trusted_local_approval_verifier,
) -> None:
    ledger = ActionLedger(
        tmp_path / "computer-ledger.sqlite3",
        platform_signature_verifier=platform_signature_verifier,
        trusted_local_approval_verifier=trusted_local_approval_verifier,
    )
    controller = ComputerController(
        MacOSComputerDriver(platform_name=os.uname().sysname),
        ComputerScope(allowed_apps=frozenset({"com.example.TextEditor"})),
        ledger,
    )
    try:
        with pytest.raises(DriverUnavailable):
            controller.acquire("com.example.TextEditor", session_id="session-a")
    finally:
        ledger.close()
