from __future__ import annotations

import stat

import pytest

from local_node.errors import BoundaryViolation, PairingError
from local_node.credentials import TestOnlyInsecureFileCredentialStore
from local_node.identity import DeviceIdentity, PairingManager
from local_node.service import LocalServiceBinding, OutboundControlPlane


def test_device_identity_is_private_and_stable_with_explicit_test_store(tmp_path):
    store = TestOnlyInsecureFileCredentialStore(tmp_path / "test-secrets")
    identity = DeviceIdentity.load_or_create(tmp_path / "state", credential_store=store)
    loaded = DeviceIdentity.load_or_create(tmp_path / "state", credential_store=store)
    assert identity.device_id == loaded.device_id
    assert identity.key == loaded.key
    assert stat.S_IMODE(identity.metadata_path.stat().st_mode) == 0o600
    assert identity.key.hex() not in repr(identity)
    assert identity.key not in identity.metadata_path.read_bytes()


def test_device_identity_default_is_unavailable_without_native_secure_store(tmp_path):
    with pytest.raises(PairingError, match="secure credential storage is unavailable"):
        DeviceIdentity.load_or_create(tmp_path / "state")
    assert not (tmp_path / "state" / "device-identity.json").exists()


def test_pairing_challenge_is_one_time_and_secret_not_repr(tmp_path):
    identity = DeviceIdentity.load_or_create(
        tmp_path / "state",
        credential_store=TestOnlyInsecureFileCredentialStore(tmp_path / "test-secrets"),
    )
    manager = PairingManager(identity)
    challenge = manager.issue(ttl_seconds=30)
    assert challenge.secret not in repr(challenge)
    credential = manager.redeem(challenge.challenge_id, challenge.secret)
    assert credential.credential not in repr(credential)
    assert manager.validate_credential(credential)
    with pytest.raises(PairingError):
        manager.redeem(challenge.challenge_id, challenge.secret)


@pytest.mark.parametrize("host", ["0.0.0.0", "192.168.1.10", "localhost"])
def test_listener_rejects_non_literal_or_non_loopback_hosts(host):
    with pytest.raises(BoundaryViolation):
        LocalServiceBinding(host, 8765).validate()


def test_listener_accepts_loopback_and_outbound_requires_tls():
    LocalServiceBinding("127.0.0.1", 8765).validate()
    LocalServiceBinding("::1", 8765).validate()
    OutboundControlPlane("wss://control.example.test/node").validate()
    with pytest.raises(BoundaryViolation):
        OutboundControlPlane("ws://control.example.test/node").validate()
    with pytest.raises(BoundaryViolation):
        OutboundControlPlane("https://user:secret@example.test/node").validate()
