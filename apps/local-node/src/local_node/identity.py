"""Device identity and short-lived, one-use pairing challenges."""

from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
import secrets
import time
from dataclasses import dataclass, field
from pathlib import Path

from .credentials import (
    CredentialStorageStatus,
    SecureCredentialStore,
)
from .errors import PairingError


def _token(size: int = 32) -> str:
    return base64.urlsafe_b64encode(secrets.token_bytes(size)).decode("ascii").rstrip("=")


@dataclass(frozen=True, slots=True)
class DeviceIdentity:
    device_id: str
    key: bytes = field(repr=False)
    metadata_path: Path = field(repr=False)
    credential_storage: CredentialStorageStatus

    @classmethod
    def load_or_create(
        cls,
        state_dir: Path,
        *,
        credential_store: SecureCredentialStore | None = None,
    ) -> "DeviceIdentity":
        if (
            credential_store is None
            or credential_store.status is CredentialStorageStatus.UNAVAILABLE
        ):
            raise PairingError(
                "native secure credential storage is unavailable; pairing is disabled"
            )
        state_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        os.chmod(state_dir, 0o700)
        metadata_path = state_dir / "device-identity.json"
        if metadata_path.exists():
            if metadata_path.is_symlink():
                raise PairingError("device identity cannot be a symlink")
            payload = json.loads(metadata_path.read_text(encoding="utf-8"))
            if "key" in payload:
                raise PairingError(
                    "legacy plaintext device identity is not accepted; secure re-pairing is required"
                )
            credential_ref = payload.get("credential_ref")
            if not isinstance(credential_ref, str):
                raise PairingError("device identity credential reference is invalid")
            if payload.get("credential_storage") != credential_store.status.value:
                raise PairingError("device identity credential storage does not match")
            key = credential_store.load(credential_ref)
            if key is None:
                raise PairingError("device identity credential is unavailable")
            if len(key) != 32:
                raise PairingError("device identity key has an invalid length")
            expected_device_id = "dev_" + hashlib.sha256(key).hexdigest()[:24]
            if payload.get("device_id") != expected_device_id:
                raise PairingError("device identity metadata does not match its credential")
            os.chmod(metadata_path, 0o600)
            return cls(expected_device_id, key, metadata_path, credential_store.status)

        key = secrets.token_bytes(32)
        device_id = "dev_" + hashlib.sha256(key).hexdigest()[:24]
        credential_ref = "device_identity_" + _token(18)
        credential_store.store(credential_ref, key)
        payload = {
            "device_id": device_id,
            "credential_ref": credential_ref,
            "credential_storage": credential_store.status.value,
        }
        fd = os.open(metadata_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        try:
            os.write(fd, json.dumps(payload, separators=(",", ":")).encode())
            os.fsync(fd)
        finally:
            os.close(fd)
        return cls(device_id, key, metadata_path, credential_store.status)

    def sign(self, payload: bytes) -> str:
        return hmac.new(self.key, payload, hashlib.sha256).hexdigest()


@dataclass(frozen=True, slots=True)
class PairingChallenge:
    challenge_id: str
    secret: str = field(repr=False)
    expires_at: float


@dataclass(frozen=True, slots=True)
class DeviceCredential:
    device_id: str
    credential: str = field(repr=False)
    expires_at: float


class PairingManager:
    """In-memory challenge broker; challenges are never serialized or logged."""

    def __init__(self, identity: DeviceIdentity, *, max_ttl_seconds: int = 300) -> None:
        self.identity = identity
        self.max_ttl_seconds = max_ttl_seconds
        self._challenges: dict[str, tuple[bytes, float]] = {}

    def issue(self, *, ttl_seconds: int = 120) -> PairingChallenge:
        if ttl_seconds <= 0 or ttl_seconds > self.max_ttl_seconds:
            raise PairingError("invalid pairing challenge lifetime")
        challenge_id = "pair_" + _token(12)
        secret = _token()
        expires_at = time.time() + ttl_seconds
        self._challenges[challenge_id] = (hashlib.sha256(secret.encode()).digest(), expires_at)
        return PairingChallenge(challenge_id, secret, expires_at)

    def redeem(
        self,
        challenge_id: str,
        secret: str,
        *,
        credential_ttl_seconds: int = 3600,
    ) -> DeviceCredential:
        stored = self._challenges.pop(challenge_id, None)
        if stored is None:
            raise PairingError("pairing challenge missing or already consumed")
        expected, expires_at = stored
        if time.time() >= expires_at:
            raise PairingError("pairing challenge expired")
        if not hmac.compare_digest(expected, hashlib.sha256(secret.encode()).digest()):
            raise PairingError("pairing challenge invalid")
        expiry = time.time() + credential_ttl_seconds
        nonce = _token(18)
        body = f"{self.identity.device_id}.{int(expiry)}.{nonce}"
        credential = body + "." + self.identity.sign(body.encode())
        return DeviceCredential(self.identity.device_id, credential, expiry)

    def validate_credential(self, credential: DeviceCredential) -> bool:
        if credential.device_id != self.identity.device_id or time.time() >= credential.expires_at:
            return False
        parts = credential.credential.rsplit(".", 1)
        if len(parts) != 2:
            return False
        body, signature = parts
        body_parts = body.split(".")
        if len(body_parts) != 3 or body_parts[0] != self.identity.device_id:
            return False
        try:
            signed_expiry = int(body_parts[1])
        except ValueError:
            return False
        if time.time() >= signed_expiry or abs(signed_expiry - credential.expires_at) > 1:
            return False
        return hmac.compare_digest(signature, self.identity.sign(body.encode()))
