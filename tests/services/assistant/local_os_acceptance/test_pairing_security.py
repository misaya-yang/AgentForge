"""Device pairing and credential-boundary tests for OS-A01 and OS-A19."""

from __future__ import annotations

from pathlib import Path

import pytest
from local_node.credentials import TestOnlyInsecureFileCredentialStore
from local_node.errors import PairingError
from local_node.identity import DeviceIdentity, PairingManager


def test_pairing_challenge_is_single_use_and_secret_is_not_repr_visible(tmp_path: Path) -> None:
    identity = DeviceIdentity.load_or_create(
        tmp_path / "identity",
        credential_store=TestOnlyInsecureFileCredentialStore(tmp_path / "test-secrets"),
    )
    manager = PairingManager(identity)
    challenge = manager.issue(ttl_seconds=60)

    assert challenge.secret not in repr(challenge)
    credential = manager.redeem(challenge.challenge_id, challenge.secret)
    assert manager.validate_credential(credential)

    with pytest.raises(PairingError, match="consumed"):
        manager.redeem(challenge.challenge_id, challenge.secret)


def test_wrong_pairing_secret_consumes_challenge_to_prevent_online_guessing(
    tmp_path: Path,
) -> None:
    identity = DeviceIdentity.load_or_create(
        tmp_path / "identity",
        credential_store=TestOnlyInsecureFileCredentialStore(tmp_path / "test-secrets"),
    )
    manager = PairingManager(identity)
    challenge = manager.issue(ttl_seconds=60)

    with pytest.raises(PairingError, match="invalid"):
        manager.redeem(challenge.challenge_id, "attacker-secret")
    with pytest.raises(PairingError, match="consumed"):
        manager.redeem(challenge.challenge_id, challenge.secret)


def test_device_credential_is_bound_to_issuing_device(tmp_path: Path) -> None:
    first = PairingManager(
        DeviceIdentity.load_or_create(
            tmp_path / "first",
            credential_store=TestOnlyInsecureFileCredentialStore(tmp_path / "first-secrets"),
        )
    )
    second = PairingManager(
        DeviceIdentity.load_or_create(
            tmp_path / "second",
            credential_store=TestOnlyInsecureFileCredentialStore(tmp_path / "second-secrets"),
        )
    )
    challenge = first.issue(ttl_seconds=60)
    credential = first.redeem(challenge.challenge_id, challenge.secret)

    assert first.validate_credential(credential)
    assert not second.validate_credential(credential)


def test_device_identity_is_private_and_symlink_identity_is_rejected(tmp_path: Path) -> None:
    identity_dir = tmp_path / "identity"
    store = TestOnlyInsecureFileCredentialStore(tmp_path / "test-secrets")
    identity = DeviceIdentity.load_or_create(identity_dir, credential_store=store)
    assert identity.metadata_path.stat().st_mode & 0o777 == 0o600
    assert identity.key not in repr(identity).encode()
    assert identity.key not in identity.metadata_path.read_bytes()

    real_identity = tmp_path / "real-identity.json"
    real_identity.write_text("{}", encoding="utf-8")
    linked_dir = tmp_path / "linked"
    linked_dir.mkdir()
    (linked_dir / "device-identity.json").symlink_to(real_identity)
    with pytest.raises(PairingError, match="symlink"):
        DeviceIdentity.load_or_create(linked_dir, credential_store=store)


def test_pairing_is_unavailable_without_native_secure_storage(tmp_path: Path) -> None:
    with pytest.raises(PairingError, match="pairing is disabled"):
        DeviceIdentity.load_or_create(tmp_path / "identity")


def test_legacy_plaintext_identity_is_rejected_instead_of_silently_migrated(
    tmp_path: Path,
) -> None:
    identity_dir = tmp_path / "legacy"
    identity_dir.mkdir()
    (identity_dir / "device-identity.json").write_text(
        '{"device_id":"dev_legacy","key":"plaintext-secret"}',
        encoding="utf-8",
    )
    store = TestOnlyInsecureFileCredentialStore(tmp_path / "test-secrets")

    with pytest.raises(PairingError, match="legacy plaintext"):
        DeviceIdentity.load_or_create(identity_dir, credential_store=store)
