"""Gateway issuer and verifier for the Rust Capability Contract V2 lease.

The signed payload is deliberately kept transport-independent.  It is the
same recursively sorted JSON object produced by the Rust contract crate,
including the empty ``signature`` field and excluding ``approval_id`` when it
is absent.  This module does not make requests or choose capability routes.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import re
import secrets
import time
from collections.abc import Mapping
from dataclasses import dataclass, replace
from enum import StrEnum
from typing import Any

CAPABILITY_DESCRIPTOR_SCHEMA_VERSION = "capability-descriptor/v2"
RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION = "runtime-capability-lease/v1"
CAPABILITY_EXECUTION_SCHEMA_VERSION = "capability-execution/v2"
CAPABILITY_CATALOG_SCHEMA_VERSION = "capability-catalog/v2"
CAPABILITY_EVENT_SCHEMA_VERSION = "capability-event/v2"
MIN_SECRET_BYTES = 32
MIN_TTL_MS = 1
MAX_TTL_MS = 120_000
_HASH_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
_IDENTIFIER_RE = re.compile(r"^[A-Za-z0-9_./:-]+$")


class CapabilityEffect(StrEnum):
    READ = "read"
    WRITE = "write"
    UNKNOWN = "unknown"


class LeaseError(ValueError):
    """Fail-closed input or signature error without including secret material."""


def canonical_json(value: Any) -> bytes:
    """Encode JSON like Rust's canonical contract encoder."""

    try:
        return json.dumps(
            value,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise LeaseError("invalid_json") from exc


def canonical_json_hash(value: Mapping[str, Any]) -> str:
    if not isinstance(value, Mapping):
        raise LeaseError("invalid_arguments")
    return "sha256:" + hashlib.sha256(canonical_json(value)).hexdigest()


def _identifier(value: str, maximum: int) -> bool:
    return (
        isinstance(value, str)
        and 0 < len(value) <= maximum
        and not any(ord(char) < 32 for char in value)
        and _IDENTIFIER_RE.fullmatch(value) is not None
    )


def _signed_payload(lease: RuntimeCapabilityLeaseV1) -> bytes:
    payload = lease.to_dict()
    payload["signature"] = ""
    if lease.approval_id is None:
        payload.pop("approval_id", None)
    return canonical_json(payload)


@dataclass(frozen=True, slots=True)
class RuntimeCapabilityLeaseV1:
    schema_version: str
    lease_id: str
    tenant_id: str
    user_id: str
    session_id: str
    run_id: str
    tool_call_id: str
    attempt_id: str
    capability_id: str
    capability_revision: int
    arguments_hash: str
    effect: CapabilityEffect
    approval_id: str | None
    issued_at_epoch_ms: int
    expires_at_epoch_ms: int
    nonce: str
    signature: str

    def to_dict(self) -> dict[str, Any]:
        result = {
            "schema_version": self.schema_version,
            "lease_id": self.lease_id,
            "tenant_id": self.tenant_id,
            "user_id": self.user_id,
            "session_id": self.session_id,
            "run_id": self.run_id,
            "tool_call_id": self.tool_call_id,
            "attempt_id": self.attempt_id,
            "capability_id": self.capability_id,
            "capability_revision": self.capability_revision,
            "arguments_hash": self.arguments_hash,
            "effect": self.effect.value,
            "issued_at_epoch_ms": self.issued_at_epoch_ms,
            "expires_at_epoch_ms": self.expires_at_epoch_ms,
            "nonce": self.nonce,
            "signature": self.signature,
        }
        if self.approval_id is not None:
            result["approval_id"] = self.approval_id
        return result

    def validate(self, now_epoch_ms: int | None = None) -> None:
        if self.schema_version != RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION:
            raise LeaseError("schema_mismatch_lease")
        for name, value, maximum in (
            ("lease_id", self.lease_id, 64),
            ("run_id", self.run_id, 64),
            ("tool_call_id", self.tool_call_id, 160),
            ("attempt_id", self.attempt_id, 160),
            ("capability_id", self.capability_id, 160),
        ):
            if not _identifier(value, maximum):
                raise LeaseError(f"invalid_{name}")
        for name, value in (
            ("tenant_id", self.tenant_id),
            ("user_id", self.user_id),
            ("session_id", self.session_id),
        ):
            if (
                not isinstance(value, str)
                or not 0 < len(value) <= 255
                or any(ord(char) < 32 for char in value)
            ):
                raise LeaseError(f"invalid_{name}")
        if (
            not isinstance(self.capability_revision, int)
            or isinstance(self.capability_revision, bool)
            or self.capability_revision < 1
            or not _HASH_RE.fullmatch(self.arguments_hash)
        ):
            raise LeaseError("invalid_lease_binding")
        if (
            not isinstance(self.issued_at_epoch_ms, int)
            or not isinstance(self.expires_at_epoch_ms, int)
            or self.issued_at_epoch_ms >= self.expires_at_epoch_ms
        ):
            raise LeaseError("lease_expired")
        now = int(time.time() * 1000) if now_epoch_ms is None else now_epoch_ms
        if now >= self.expires_at_epoch_ms:
            raise LeaseError("lease_expired")
        if (
            not isinstance(self.nonce, str)
            or not 16 <= len(self.nonce) <= 128
            or any(ord(char) < 32 for char in self.nonce)
            or not _HASH_RE.fullmatch(self.signature)
        ):
            raise LeaseError("invalid_lease_proof")
        if self.effect is CapabilityEffect.READ and self.approval_id is not None:
            raise LeaseError("invalid_approval_id")
        if self.effect in (CapabilityEffect.WRITE, CapabilityEffect.UNKNOWN) and not _identifier(
            self.approval_id or "", 64
        ):
            raise LeaseError("invalid_approval_id")
        if self.effect is not CapabilityEffect.READ and self.effect not in (
            CapabilityEffect.WRITE,
            CapabilityEffect.UNKNOWN,
        ):
            raise LeaseError("invalid_effect")


class CapabilityLeaseIssuer:
    """Issue and verify leases without exposing the signing secret."""

    __slots__ = ("_secret", "_ttl_ms")

    def __init__(self, secret: bytes | str, *, ttl_ms: int = 60_000) -> None:
        secret_bytes = secret.encode("utf-8") if isinstance(secret, str) else bytes(secret)
        if len(secret_bytes) < MIN_SECRET_BYTES:
            raise LeaseError("lease_secret_too_short")
        if (
            not isinstance(ttl_ms, int)
            or isinstance(ttl_ms, bool)
            or not MIN_TTL_MS <= ttl_ms <= MAX_TTL_MS
        ):
            raise LeaseError("lease_ttl_out_of_bounds")
        self._secret = secret_bytes
        self._ttl_ms = ttl_ms

    def issue(
        self,
        *,
        tenant_id: str,
        user_id: str,
        session_id: str,
        run_id: str,
        tool_call_id: str,
        attempt_id: str,
        capability_id: str,
        capability_revision: int,
        arguments: Mapping[str, Any],
        effect: CapabilityEffect | str,
        approval_id: str | None = None,
        now_epoch_ms: int | None = None,
        lease_id: str | None = None,
        nonce: str | None = None,
    ) -> RuntimeCapabilityLeaseV1:
        try:
            normalized_effect = CapabilityEffect(effect)
        except ValueError as exc:
            raise LeaseError("invalid_effect") from exc
        now = int(time.time() * 1000) if now_epoch_ms is None else now_epoch_ms
        lease = RuntimeCapabilityLeaseV1(
            schema_version=RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION,
            lease_id=lease_id or secrets.token_hex(16),
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            run_id=run_id,
            tool_call_id=tool_call_id,
            attempt_id=attempt_id,
            capability_id=capability_id,
            capability_revision=capability_revision,
            arguments_hash=canonical_json_hash(arguments),
            effect=normalized_effect,
            approval_id=approval_id,
            issued_at_epoch_ms=now,
            expires_at_epoch_ms=now + self._ttl_ms,
            nonce=nonce or secrets.token_urlsafe(24),
            signature="sha256:" + "0" * 64,
        )
        lease.validate(now)
        signature = hmac.new(self._secret, _signed_payload(lease), hashlib.sha256).hexdigest()
        return replace(lease, signature="sha256:" + signature)

    def verify(self, lease: RuntimeCapabilityLeaseV1, *, now_epoch_ms: int | None = None) -> None:
        lease.validate(now_epoch_ms)
        expected = hmac.new(self._secret, _signed_payload(lease), hashlib.sha256).hexdigest()
        if not hmac.compare_digest(lease.signature[7:], expected):
            raise LeaseError("lease_signature_invalid")


def verify_lease_signature(
    lease: RuntimeCapabilityLeaseV1, secret: bytes | str, *, now_epoch_ms: int | None = None
) -> None:
    CapabilityLeaseIssuer(secret).verify(lease, now_epoch_ms=now_epoch_ms)
