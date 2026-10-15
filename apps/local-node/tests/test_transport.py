from __future__ import annotations

import io
import hashlib
import hmac
import json
import time
from dataclasses import replace
from email.message import Message
from pathlib import Path
from typing import Any, Mapping

import pytest

from local_node.credentials import TestOnlyInsecureFileCredentialStore
from local_node.errors import CapabilityDenied
from local_node.identity import DeviceCredential, DeviceIdentity
from local_node.models import ActionStatus, canonical_json
from local_node.outbox import ReceiptOutbox
from local_node.service import OutboundControlPlane
from local_node.transport import (
    ClaimReplayGuard,
    DeviceOutboundRunner,
    DispatchOutcome,
    ExchangeResponse,
    HttpsJsonTransport,
    PairingRedemption,
    TransportProtocolError,
    TransportUnavailable,
    action_to_wire,
)


class FakeDispatcher:
    def __init__(self) -> None:
        self.connected = 0
        self.dispatched: list[str] = []
        self.cancelled: list[str] = []
        self.stopped = 0
        self.disconnected = 0

    def connect(self) -> None:
        self.connected += 1

    def dispatch(self, *, action, normalized_arguments):
        self.dispatched.append(action.action_id)
        return DispatchOutcome(
            action.action_id,
            ActionStatus.SUCCEEDED,
            {"arguments_digest": action.arguments_digest, "secret": "not-retained"},
        )

    def cancel(self, action_id: str) -> DispatchOutcome:
        self.cancelled.append(action_id)
        return DispatchOutcome(action_id, ActionStatus.CANCELLED)

    def emergency_stop(self) -> tuple[str, ...]:
        self.stopped += 1
        return ("active-action",)

    def disconnect(self) -> tuple[str, ...]:
        self.disconnected += 1
        return ()


class ScriptedTransport:
    kind = "loopback-test"

    def __init__(self, responses: list[ExchangeResponse]) -> None:
        self.responses = responses
        self.requests = []
        self.closed = False

    def redeem_pairing(self, *, endpoint, redemption):
        raise AssertionError("pairing redemption is not used in this test")

    def exchange(self, *, endpoint, credential, request):
        self.requests.append(request)
        return self.responses.pop(0)

    def close(self) -> None:
        self.closed = True


class PairingTransport(ScriptedTransport):
    def __init__(self, credential: DeviceCredential) -> None:
        super().__init__([])
        self.credential = credential
        self.redemption = None

    def redeem_pairing(self, *, endpoint, redemption):
        self.redemption = redemption
        return self.credential


class TestPairingSigner:
    __test__ = False
    algorithm = "test-hmac-sha256-not-production"
    public_key = "test-public-key"

    def __init__(self) -> None:
        self.key = b"pairing-proof-test-key"

    def sign(self, payload: bytes) -> str:
        return hmac.new(self.key, payload, hashlib.sha256).hexdigest()


def identity_and_credential(tmp_path: Path):
    store = TestOnlyInsecureFileCredentialStore(tmp_path / "test-credentials")
    identity = DeviceIdentity.load_or_create(tmp_path / "identity", credential_store=store)
    credential = DeviceCredential(identity.device_id, "opaque-server-credential", time.time() + 60)
    return identity, store, credential


def signed_action_for_device(action_factory, platform_signature_verifier, device_id: str):
    arguments = {"message": "hello"}
    action = action_factory("test.echo", arguments, "target-v1")
    action = replace(action, device_id=device_id)
    return replace(
        action,
        platform_signature=platform_signature_verifier.sign(action.canonical_signed_payload()),
    ), arguments


def make_runner(
    tmp_path,
    *,
    platform_signature_verifier,
    transport,
    commands=(),
):
    identity, store, credential = identity_and_credential(tmp_path)
    if transport == "scripted":
        transport = ScriptedTransport([ExchangeResponse(0, tuple(commands))])
    dispatcher = FakeDispatcher()
    outbox = ReceiptOutbox(tmp_path / "state" / "outbox.sqlite")
    runner = DeviceOutboundRunner(
        endpoint=OutboundControlPlane("https://control.example.test/local-node"),
        identity=identity,
        credential=credential,
        credential_store=store,
        credential_reference="outbound-server-credential",
        pairing_signer=TestPairingSigner(),
        transport=transport,
        platform_signature_verifier=platform_signature_verifier,
        dispatcher=dispatcher,
        doctor=lambda: {
            "computer": {"status": "needs_action"},
            "trusted_local_approval": "ready",
        },
        outbox=outbox,
        replay_guard=ClaimReplayGuard(tmp_path / "state" / "replay.sqlite"),
        allow_test_credentials=True,
    )
    return runner, dispatcher, outbox, transport, identity


def test_signed_claim_dispatches_once_and_uploads_digest_receipt(
    tmp_path, action_factory, platform_signature_verifier
):
    identity, _, _ = identity_and_credential(tmp_path / "pre")
    action, arguments = signed_action_for_device(
        action_factory, platform_signature_verifier, identity.device_id
    )
    # Use the same identity that make_runner constructs by writing to its path
    # before creating the runner.
    runner, dispatcher, outbox, transport, actual_identity = make_runner(
        tmp_path / "runner",
        platform_signature_verifier=platform_signature_verifier,
        transport="scripted",
    )
    action = replace(action, device_id=actual_identity.device_id)
    action = replace(
        action,
        platform_signature=platform_signature_verifier.sign(action.canonical_signed_payload()),
    )
    transport.responses[0] = ExchangeResponse(
        0,
        (
            {
                "kind": "claim",
                "command_id": "command-1",
                "action": action_to_wire(action),
                "normalized_arguments": arguments,
            },
        ),
    )

    assert runner.run_once() == 1
    assert dispatcher.dispatched == [action.action_id]
    pending = outbox.pending()
    assert len(pending) == 1
    assert pending[0].status == "succeeded"
    assert pending[0].result_digest is not None
    assert "not-retained" not in outbox.export_pending_json()

    transport.responses.append(ExchangeResponse(pending[0].sequence, ()))
    assert runner.run_once() == 0
    assert outbox.pending() == ()
    assert len(transport.requests[1].receipts) == 1


def test_tampered_or_replayed_claim_never_redispatches(
    tmp_path, action_factory, platform_signature_verifier
):
    runner, dispatcher, _, transport, identity = make_runner(
        tmp_path,
        platform_signature_verifier=platform_signature_verifier,
        transport="scripted",
    )
    action, arguments = signed_action_for_device(
        action_factory, platform_signature_verifier, identity.device_id
    )
    tampered = replace(action, arguments_digest="0" * 64)
    transport.responses[0] = ExchangeResponse(
        0,
        (
            {
                "kind": "claim",
                "command_id": "tampered",
                "action": action_to_wire(tampered),
                "normalized_arguments": arguments,
            },
        ),
    )
    with pytest.raises(CapabilityDenied):
        runner.run_once()
    assert dispatcher.dispatched == []

    command = {
        "kind": "claim",
        "command_id": "valid",
        "action": action_to_wire(action),
        "normalized_arguments": arguments,
    }
    transport.responses.append(ExchangeResponse(0, (command,)))
    runner.run_once()
    assert dispatcher.dispatched == [action.action_id]
    # Same exact command is a receipt retry, not an action retry.
    transport.responses.append(ExchangeResponse(0, (command,)))
    runner.run_once()
    assert dispatcher.dispatched == [action.action_id]


def signed_control(platform_signature_verifier, *, kind: str, device_id: str, **extra):
    now = time.time()
    value = {
        "kind": kind,
        "command_id": f"command-{kind}",
        "device_id": device_id,
        "nonce": f"nonce-{kind}",
        "issued_at": now,
        "expires_at": now + 60,
        "platform_key_id": platform_signature_verifier.key_id,
        **extra,
    }
    signature = platform_signature_verifier.sign(canonical_json(value).encode("utf-8"))
    return {**value, "platform_signature": signature}


def test_signed_cancel_and_emergency_stop(tmp_path, platform_signature_verifier):
    runner, dispatcher, _, transport, identity = make_runner(
        tmp_path,
        platform_signature_verifier=platform_signature_verifier,
        transport="scripted",
    )
    transport.responses[0] = ExchangeResponse(
        0,
        (
            signed_control(
                platform_signature_verifier,
                kind="cancel",
                device_id=identity.device_id,
                action_id="action-1",
            ),
            signed_control(
                platform_signature_verifier,
                kind="emergency_stop",
                device_id=identity.device_id,
            ),
        ),
    )
    assert runner.run_once() == 2
    assert dispatcher.cancelled == ["action-1"]
    assert dispatcher.stopped == 1


def test_doctor_is_fail_closed_without_adapter_verifier_or_native_storage(
    tmp_path, platform_signature_verifier
):
    runner, _, _, _, _ = make_runner(
        tmp_path,
        platform_signature_verifier=platform_signature_verifier,
        transport=None,
    )
    runner.platform_signature_verifier = None
    runner.allow_test_credentials = False
    report = runner.doctor()
    assert report.status == "unavailable"
    assert report.transport == "unavailable"
    assert report.platform_signature_verifier == "unavailable"
    assert report.identity_credential_storage == "test_only_insecure"
    assert report.outbound_credential_storage == "test_only_insecure"
    with pytest.raises(TransportUnavailable):
        runner.run_once()


def test_remote_pairing_signs_challenge_and_persists_opaque_credential(
    tmp_path, platform_signature_verifier
):
    identity, store, _ = identity_and_credential(tmp_path)
    credential = DeviceCredential(identity.device_id, "opaque-remote-value", time.time() + 60)
    transport = PairingTransport(credential)
    dispatcher = FakeDispatcher()
    runner = DeviceOutboundRunner(
        endpoint=OutboundControlPlane("https://control.example.test/local-node"),
        identity=identity,
        credential=None,
        credential_store=store,
        credential_reference="remote-credential",
        pairing_signer=TestPairingSigner(),
        transport=transport,
        platform_signature_verifier=platform_signature_verifier,
        dispatcher=dispatcher,
        doctor=lambda: {},
        outbox=ReceiptOutbox(tmp_path / "state" / "outbox.sqlite"),
        replay_guard=ClaimReplayGuard(tmp_path / "state" / "replay.sqlite"),
        allow_test_credentials=True,
    )

    assert (
        runner.redeem_pairing(
            challenge_id="challenge-1",
            user_code="ABCD-1234",
            display_name="Local Mac",
            platform="macos",
            node_version="0.1.0",
            capability_claims=("file.read",),
            permission_snapshot_digest="a" * 64,
        )
        == credential
    )
    assert transport.redemption is not None
    assert transport.redemption.proof_algorithm == "test-hmac-sha256-not-production"
    stored = store.load("remote-credential")
    assert stored is not None and b"opaque-remote-value" in stored

    loaded = DeviceOutboundRunner(
        endpoint=OutboundControlPlane("https://control.example.test/local-node"),
        identity=identity,
        credential=None,
        credential_store=store,
        credential_reference="remote-credential",
        pairing_signer=TestPairingSigner(),
        transport=ScriptedTransport([ExchangeResponse(0)]),
        platform_signature_verifier=platform_signature_verifier,
        dispatcher=dispatcher,
        doctor=lambda: {},
        outbox=ReceiptOutbox(tmp_path / "state" / "outbox-loaded.sqlite"),
        replay_guard=ClaimReplayGuard(tmp_path / "state" / "replay-loaded.sqlite"),
        allow_test_credentials=True,
    )
    assert loaded.credential == credential


class FakeHeaders(Message):
    def get_content_type(self):
        return "application/json"


class FakeResponse(io.BytesIO):
    status = 200
    headers = FakeHeaders()

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


class CapturingOpener:
    def __init__(self, response: Mapping[str, Any]) -> None:
        self.response = response
        self.request = None

    def open(self, request, timeout):
        self.request = request
        return FakeResponse(json.dumps(self.response).encode("utf-8"))


def test_https_adapter_keeps_device_credential_out_of_body():
    opener = CapturingOpener(
        {
            "protocol_version": "ai-platform.local-node.v1",
            "accepted_through_sequence": 0,
            "commands": [],
        }
    )
    transport = HttpsJsonTransport(opener=opener)
    from local_node.transport import ExchangeRequest

    credential = DeviceCredential("device-a", "top-secret-credential", time.time() + 60)
    transport.exchange(
        endpoint=OutboundControlPlane("https://control.example.test/node"),
        credential=credential,
        request=ExchangeRequest("device-a", {}, (), time.time()),
    )
    assert opener.request is not None
    assert opener.request.get_header("Authorization") == "Device top-secret-credential"
    assert b"top-secret-credential" not in opener.request.data

    with pytest.raises(TransportUnavailable):
        transport.exchange(
            endpoint=OutboundControlPlane("wss://control.example.test/node"),
            credential=credential,
            request=ExchangeRequest("device-a", {}, (), time.time()),
        )


def test_https_pairing_response_is_strict_and_redemption_secret_repr_is_safe():
    redemption = PairingRedemption(
        "challenge-1",
        "pairing-secret",
        "device-a",
        "ed25519",
        "public-key",
        "proof",
        "Local Mac",
        "macos",
        "0.1.0",
        ("file.read",),
        "a" * 64,
    )
    assert "pairing-secret" not in repr(redemption)
    opener = CapturingOpener(
        {
            "protocol_version": "ai-platform.local-node.v1",
            "device_id": "device-a",
            "credential": "credential-value",
            "expires_at": time.time() + 60,
            "unexpected": True,
        }
    )
    with pytest.raises(TransportProtocolError):
        HttpsJsonTransport(opener=opener).redeem_pairing(
            endpoint=OutboundControlPlane("https://control.example.test/node"),
            redemption=redemption,
        )


@pytest.mark.parametrize(
    ("content", "expected_status"),
    [
        pytest.param("x" * (2 * 1024 * 1024), "failed", id="ascii_over_budget"),
        pytest.param("中" * 700_000, "failed", id="utf8_over_budget"),
        pytest.param("\x00" * 350_000, "failed", id="json_escape_over_budget"),
        pytest.param("small exact text\n", "succeeded", id="small_read_control"),
    ],
)
def test_file_result_budget_has_durable_terminal_without_replay(
    tmp_path, action_factory, platform_signature_verifier, content, expected_status
):
    from local_node.file_dispatch import ReadOnlyFileActionHandlers
    from local_node.files import LocalFileService
    from local_node.grants import DirectoryGrantStore
    from local_node.ledger import ActionLedger
    from local_node.outbox import OutboxError

    runner, dispatcher, outbox, transport, identity = make_runner(
        tmp_path / "runner",
        platform_signature_verifier=platform_signature_verifier,
        transport="scripted",
    )
    root = tmp_path / "authorized"
    root.mkdir()
    (root / "selected.txt").write_text(content, encoding="utf-8")
    grants = DirectoryGrantStore()
    grant = grants.issue(
        root, frozenset({"read"}), tenant_id="tenant-a", user_id="user-a"
    )
    ledger = ActionLedger(
        tmp_path / "ledger.sqlite", platform_signature_verifier=platform_signature_verifier
    )
    files = ReadOnlyFileActionHandlers(
        LocalFileService(grants, tmp_path / "rollback", ledger)
    )
    arguments = {"grant_id": grant.grant_id, "path": "selected.txt"}
    action = action_factory(
        "file.read", arguments, "selected-target", approved=False,
        capability_lease_id=grant.grant_id, resource_refs=(grant.grant_id, "selected.txt"),
        tool_name="local_file_read", operation="file.read",
    )
    action = replace(action, device_id=identity.device_id)
    action = replace(
        action, platform_signature=platform_signature_verifier.sign(action.canonical_signed_payload())
    )
    command = {
        "kind": "claim", "command_id": "read-command", "action": action_to_wire(action),
        "normalized_arguments": arguments,
    }
    calls = []

    def dispatch(*, action, normalized_arguments):
        calls.append(action.action_id)
        return files.read_file(action, normalized_arguments)

    dispatcher.dispatch = dispatch
    transport.responses = [ExchangeResponse(0, (command,)), ExchangeResponse(0, (command,)), ExchangeResponse(1)]
    try:
        try:
            assert runner.run_once() == 1
        except OutboxError as exc:
            pytest.fail(f"read result budget must produce a terminal receipt: {type(exc).__name__}")
        events = outbox.pending()
        assert len(events) == 1
        assert events[0].status == expected_status
        assert events[0].event_type == f"action.{expected_status}"
        if expected_status == "failed":
            assert events[0].error_code == "local_result_too_large"
            assert events[0].result is None
        else:
            assert events[0].error_code is None
            assert events[0].result["content"] == content
        # Restart the durable receipt and nonce stores before exact retransmission.
        outbox.close()
        runner.replay_guard.close()
        runner.outbox = ReceiptOutbox(tmp_path / "runner/state/outbox.sqlite")
        runner.replay_guard = ClaimReplayGuard(tmp_path / "runner/state/replay.sqlite")
        assert runner.run_once() == 1
        assert calls == [action.action_id]
        assert len(runner.outbox.pending()) == 1
        assert transport.requests[1].receipts[0]["status"] == expected_status
        assert runner.run_once() == 0
        assert runner.outbox.pending() == ()
        assert runner.outbox.acked_through == 1
    finally:
        runner.outbox.close()
        runner.replay_guard.close()
        ledger.close()


def test_oversized_non_read_result_keeps_side_effect_unknown(
    tmp_path, action_factory, platform_signature_verifier
):
    from local_node.outbox import OutboxError

    runner, dispatcher, outbox, transport, identity = make_runner(
        tmp_path, platform_signature_verifier=platform_signature_verifier, transport="scripted"
    )
    action, arguments = signed_action_for_device(
        action_factory, platform_signature_verifier, identity.device_id
    )
    calls = []

    def dispatch(*, action, normalized_arguments):
        calls.append(action.action_id)
        return DispatchOutcome(action.action_id, ActionStatus.SUCCEEDED, {"data": "x" * (2 * 1024 * 1024)})

    dispatcher.dispatch = dispatch
    command = {"kind": "claim", "command_id": "large-command", "action": action_to_wire(action), "normalized_arguments": arguments}
    transport.responses = [ExchangeResponse(0, (command,)), ExchangeResponse(0, (command,))]
    try:
        try:
            runner.run_once()
        except OutboxError as exc:
            pytest.fail(f"oversized side-effect result must retain an unknown receipt: {type(exc).__name__}")
        assert outbox.pending()[0].status == "unknown"
        assert outbox.pending()[0].error_code == "local_result_too_large"
        assert outbox.pending()[0].result is None
        runner.run_once()
        assert calls == [action.action_id]
    finally:
        outbox.close()
        runner.replay_guard.close()


def test_terminal_append_does_not_hide_other_outbox_errors(
    tmp_path, platform_signature_verifier, monkeypatch
):
    from local_node.outbox import OutboxError

    runner, _, outbox, _, _ = make_runner(
        tmp_path, platform_signature_verifier=platform_signature_verifier, transport="scripted"
    )
    calls = []

    def fail_append(**kwargs):
        calls.append(kwargs)
        raise OutboxError("storage unavailable")

    monkeypatch.setattr(outbox, "append", fail_append)
    try:
        with pytest.raises(OutboxError, match="storage unavailable"):
            runner._append_terminal(
                command_id="command-a", action_id="action-a", status=ActionStatus.SUCCEEDED,
                result={"small": "result"}, error_code=None, retain_result=True,
            )
        assert len(calls) == 1
    finally:
        outbox.close()
        runner.replay_guard.close()
