"""Signed, secret-free leases for the Agent model data plane.

The lease signature authenticates a row that already exists in PostgreSQL; it
is not a bearer credential for a provider.  Provider secrets remain inside the
Gateway and are resolved by the pinned provider revision only after the lease,
scope, request hash, and budget reservation have passed.
"""

from __future__ import annotations

import hmac
import re
from dataclasses import asdict, dataclass
from hashlib import sha256
from typing import Final

from .agent_runtime import canonical_runtime_json

RUNTIME_MODEL_LEASE_SCHEMA_VERSION: Final = "agent-runtime-model-lease/v1"
_HEX_64 = re.compile(r"^[0-9a-f]{64}$")


class RuntimeModelLeaseError(ValueError):
    """Stable fail-closed lease validation error."""

    def __init__(self, code: str):
        self.code = code
        super().__init__(code)


@dataclass(frozen=True, slots=True)
class RuntimeModelLeaseClaims:
    schema_version: str
    lease_id: str
    snapshot_id: str
    run_id: str
    runtime_thread_id: str
    tenant_id: str
    user_id: str
    session_id: str
    provider_id: str
    model_id: str
    capability_revision: int
    issued_at_ms: int
    expires_at_ms: int
    nonce_sha256: str

    def validated(self) -> RuntimeModelLeaseClaims:
        if self.schema_version != RUNTIME_MODEL_LEASE_SCHEMA_VERSION:
            raise RuntimeModelLeaseError("RUNTIME_MODEL_LEASE_VERSION_UNSUPPORTED")
        for field in (
            "lease_id",
            "snapshot_id",
            "run_id",
            "runtime_thread_id",
            "tenant_id",
            "user_id",
            "session_id",
            "provider_id",
            "model_id",
        ):
            value = getattr(self, field)
            if not isinstance(value, str) or not value or value.strip() != value:
                raise RuntimeModelLeaseError("RUNTIME_MODEL_LEASE_INVALID")
        if (
            isinstance(self.capability_revision, bool)
            or self.capability_revision < 1
            or isinstance(self.issued_at_ms, bool)
            or isinstance(self.expires_at_ms, bool)
            or self.expires_at_ms <= self.issued_at_ms
            or not _HEX_64.fullmatch(self.nonce_sha256)
        ):
            raise RuntimeModelLeaseError("RUNTIME_MODEL_LEASE_INVALID")
        return self

    def canonical_payload(self) -> str:
        self.validated()
        return canonical_runtime_json(asdict(self))


class RuntimeModelLeaseSigner:
    """HMAC signer whose output fits Agent's bounded turn metadata value."""

    def __init__(self, secret: str) -> None:
        if not secret or len(secret) < 16:
            raise ValueError("Runtime model lease signing secret must be at least 16 chars")
        self._secret = secret.encode("utf-8")

    def sign(self, claims: RuntimeModelLeaseClaims) -> str:
        digest = hmac.new(
            self._secret,
            claims.canonical_payload().encode("utf-8"),
            sha256,
        ).hexdigest()
        return f"v1:{digest}"

    def verify(self, signature: str, claims: RuntimeModelLeaseClaims) -> None:
        expected = self.sign(claims)
        if not isinstance(signature, str) or not hmac.compare_digest(signature, expected):
            raise RuntimeModelLeaseError("RUNTIME_MODEL_LEASE_SIGNATURE_INVALID")


__all__ = [
    "RUNTIME_MODEL_LEASE_SCHEMA_VERSION",
    "RuntimeModelLeaseClaims",
    "RuntimeModelLeaseError",
    "RuntimeModelLeaseSigner",
]
