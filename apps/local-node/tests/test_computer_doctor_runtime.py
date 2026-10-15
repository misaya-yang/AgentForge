from __future__ import annotations

import time
from dataclasses import replace

import pytest

from local_node.computer import (
    ComputerAction,
    ComputerController,
    ComputerObservation,
    ComputerScope,
    HealthStatus,
    MacOSComputerDriver,
)
from local_node.credentials import TestOnlyInsecureFileCredentialStore
from local_node.doctor import CapabilityDoctor
from local_node.errors import CapabilityDenied, DriverUnavailable, StaleTargetError
from local_node.grants import DirectoryGrantStore
from local_node.identity import DeviceIdentity
from local_node.ledger import ActionLedger
from local_node.processes import ProcessPolicy, ProcessRunner
from local_node.service import LocalNodeRuntime, LocalServiceBinding, OutboundControlPlane


class FakeBackend:
    name = "fake-cua"

    def __init__(self):
        self.counter = 0
        self.stopped = False

    def observe(self, app_id, window_id):
        self.counter += 1
        return ComputerObservation(
            f"obs-{self.counter}",
            app_id,
            window_id or "window-1",
            None,
            800,
            600,
            None,
            "a11y",
            time.time(),
        )

    def execute(self, action):
        assert not self.stopped

    def stop(self):
        self.stopped = True


def test_macos_driver_is_safely_unavailable_without_driver_or_permissions():
    driver = MacOSComputerDriver(platform_name="Darwin")
    assert driver.doctor().status is HealthStatus.NEEDS_ACTION
    with pytest.raises(DriverUnavailable):
        driver.observe("TextEdit")
    unsupported = MacOSComputerDriver(platform_name="Linux")
    assert unsupported.doctor().status is HealthStatus.UNSUPPORTED


def test_computer_action_requires_fresh_observation_and_bound_approval(
    tmp_path, action_factory, platform_signature_verifier, trusted_local_approval_verifier
):
    backend = FakeBackend()
    driver = MacOSComputerDriver(
        backend,
        accessibility_probe=lambda: True,
        screen_recording_probe=lambda: True,
        platform_name="Darwin",
    )
    ledger = ActionLedger(
        tmp_path / "ledger.sqlite",
        platform_signature_verifier=platform_signature_verifier,
        trusted_local_approval_verifier=trusted_local_approval_verifier,
    )
    controller = ComputerController(driver, ComputerScope(frozenset({"TextEdit"})), ledger)
    lease = controller.acquire("TextEdit", session_id="session-a")
    observation = controller.observe(lease.token)
    computer_action = ComputerAction("click", observation.observation_id, {"x": 20, "y": 30})
    args = {
        "lease_id": lease.lease_id,
        "kind": "click",
        "observation_id": observation.observation_id,
        "arguments": {"x": 20, "y": 30},
    }
    action = action_factory(
        "app.control",
        args,
        observation.digest,
        capability_lease_id=lease.lease_id,
        resource_refs=(lease.lease_id, observation.app_id, observation.window_id),
        tool_name="local_app_control",
        operation="app.control",
    )
    result = controller.execute(lease.token, computer_action, action)
    assert result.status == "succeeded"
    assert result.after_observation_id != observation.observation_id
    with pytest.raises(StaleTargetError):
        controller.execute(
            lease.token,
            ComputerAction("click", observation.observation_id, {"x": 20, "y": 30}),
            replace(action, action_id="action-2", idempotency_key="idem-2"),
        )
    assert controller.emergency_stop()
    assert backend.stopped


def test_computer_lease_is_bound_to_platform_session(
    tmp_path,
    action_factory,
    platform_signature_verifier,
    trusted_local_approval_verifier,
):
    backend = FakeBackend()
    driver = MacOSComputerDriver(
        backend,
        accessibility_probe=lambda: True,
        screen_recording_probe=lambda: True,
        platform_name="Darwin",
    )
    ledger = ActionLedger(
        tmp_path / "session-ledger.sqlite",
        platform_signature_verifier=platform_signature_verifier,
        trusted_local_approval_verifier=trusted_local_approval_verifier,
    )
    controller = ComputerController(driver, ComputerScope(frozenset({"TextEdit"})), ledger)
    lease = controller.acquire("TextEdit", session_id="session-a")
    observation = controller.observe(lease.token)
    computer_action = ComputerAction("click", observation.observation_id, {"x": 20, "y": 30})
    args = {
        "lease_id": lease.lease_id,
        "kind": "click",
        "observation_id": observation.observation_id,
        "arguments": {"x": 20, "y": 30},
    }
    action = action_factory(
        "app.control",
        args,
        observation.digest,
        capability_lease_id=lease.lease_id,
        resource_refs=(lease.lease_id, observation.app_id, observation.window_id),
        tool_name="local_app_control",
        operation="app.control",
    )
    wrong_session = replace(action, session_id="session-b")

    with pytest.raises(CapabilityDenied, match="another session"):
        controller.execute(lease.token, computer_action, wrong_session)
    assert ledger.get(action.action_id) is None


@pytest.mark.parametrize(
    "capability",
    ["computer.click", "app.observe", "screen.observe", "app.submit"],
)
def test_computer_input_accepts_only_platform_app_control_capability(
    tmp_path,
    action_factory,
    platform_signature_verifier,
    trusted_local_approval_verifier,
    capability,
):
    backend = FakeBackend()
    driver = MacOSComputerDriver(
        backend,
        accessibility_probe=lambda: True,
        screen_recording_probe=lambda: True,
        platform_name="Darwin",
    )
    ledger = ActionLedger(
        tmp_path / "ledger.sqlite",
        platform_signature_verifier=platform_signature_verifier,
        trusted_local_approval_verifier=trusted_local_approval_verifier,
    )
    controller = ComputerController(driver, ComputerScope(frozenset({"TextEdit"})), ledger)
    lease = controller.acquire("TextEdit", session_id="session-a")
    observation = controller.observe(lease.token)
    computer_action = ComputerAction("click", observation.observation_id, {"x": 20, "y": 30})
    args = {
        "lease_id": lease.lease_id,
        "kind": "click",
        "observation_id": observation.observation_id,
        "arguments": {"x": 20, "y": 30},
    }
    action = action_factory(
        capability,
        args,
        observation.digest,
        capability_lease_id=lease.lease_id,
        resource_refs=(lease.lease_id, observation.app_id, observation.window_id),
        tool_name="local_app_control",
        operation="app.control",
    )

    with pytest.raises(CapabilityDenied, match="capability mismatch"):
        controller.execute(lease.token, computer_action, action)
    assert ledger.get(action.action_id) is None


def test_doctor_and_disconnect_mark_unresolved_action_unknown(
    tmp_path, action_factory, platform_signature_verifier, trusted_local_approval_verifier
):
    state = tmp_path / "state"
    identity = DeviceIdentity.load_or_create(
        state,
        credential_store=TestOnlyInsecureFileCredentialStore(tmp_path / "test-secrets"),
    )
    grants = DirectoryGrantStore()
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    grants.issue(workspace, frozenset({"read"}), tenant_id="tenant-a", user_id="user-a")
    ledger = ActionLedger(
        state / "ledger.sqlite",
        platform_signature_verifier=platform_signature_verifier,
        trusted_local_approval_verifier=trusted_local_approval_verifier,
    )
    policy = ProcessPolicy()
    process = ProcessRunner(grants, policy, ledger)
    driver = MacOSComputerDriver(platform_name="Darwin")
    computer = ComputerController(driver, ComputerScope(frozenset()), ledger)
    outbound = OutboundControlPlane("wss://control.example.test/node")
    runtime = LocalNodeRuntime(
        ledger,
        process,
        computer,
        control_plane=outbound,
    )
    runtime.connect()
    assert runtime.online
    action = action_factory("test", {"value": 1}, "target", approved=False)
    ledger.begin(action)
    ledger.mark_dispatched(action.action_id)
    ledger.mark_running(action.action_id)
    assert runtime.disconnect() == (action.action_id,)
    assert ledger.get(action.action_id).status.value == "unknown"
    replay = ledger.begin(action)
    assert not replay.created
    assert replay.record.status.value == "unknown"

    report = CapabilityDoctor(
        identity,
        grants,
        ledger,
        policy,
        driver,
        LocalServiceBinding(),
        outbound,
    ).run()
    assert report.ledger == "ready"
    assert report.control_plane == "ready"
    assert report.secure_credential_storage == "test_only_insecure"
    assert report.trusted_local_approval == "ready"
    assert report.process_runner == "needs_action"
    assert report.computer["status"] is HealthStatus.NEEDS_ACTION

    unpaired = CapabilityDoctor(
        None,
        grants,
        ledger,
        policy,
        driver,
        LocalServiceBinding(),
        outbound,
    ).run()
    assert unpaired.device_id is None
    assert unpaired.secure_credential_storage == "unavailable"
    assert unpaired.trusted_local_approval == "ready"
