"""Short-lived, request-bound proofs for private capability calls.

The ordinary internal token authenticates a trusted network peer.  A proof
additionally binds that peer's request to the exact scope, route, body and
runtime execution which issued it.  The proof is intentionally stateless:
the short expiry limits the replay window, while the execution store remains
the authority for idempotency and terminal-state replay protection.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import json
import time
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

SCHEMA_VERSION = "ai-platform-capability-proof/v1"
HEADER_NAME = "x-ai-capability-proof"
MAX_TTL_SECONDS = 120
MIN_SECRET_BYTES = 32


class CapabilityProofError(ValueError):
    """Raised when a capability proof is malformed or does not verify."""


@dataclass(frozen=True, slots=True)
class CapabilityProof:
    method: str
    path: str
    body_sha256: str
    tenant_id: str
    user_id: str
    session_id: str
    execution_id: str
    run_id: str
    expires_at: int
    nonce: str
    signature: str


def canonical_json(value: Any) -> bytes:
    """Serialize JSON deterministically for cross-language signing."""

    try:
        return json.dumps(
            value,
            ensure_ascii=False,
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
    except (TypeError, ValueError) as exc:
        raise CapabilityProofError("proof body is not canonicalizable") from exc


def canonical_body_hash(body: Any) -> str:
    return hashlib.sha256(canonical_json(body)).hexdigest()


def _secret_bytes(secret: str | bytes) -> bytes:
    value = secret.encode("utf-8") if isinstance(secret, str) else bytes(secret)
    if len(value) < MIN_SECRET_BYTES:
        raise CapabilityProofError("proof secret is too short")
    return value


def _unsigned_fields(
    *,
    method: str,
    path: str,
    body_sha256: str,
    tenant_id: str,
    user_id: str,
    session_id: str,
    execution_id: str,
    run_id: str,
    expires_at: int,
    nonce: str,
) -> dict[str, Any]:
    method = method.upper().strip()
    path = path.strip()
    fields: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "method": method,
        "path": path,
        "body_sha256": body_sha256,
        "tenant_id": tenant_id,
        "user_id": user_id,
        "session_id": session_id,
        "execution_id": execution_id,
        "run_id": run_id,
        "expires_at": expires_at,
        "nonce": nonce,
    }
    _validate_unsigned_fields(fields)
    return fields


def _validate_unsigned_fields(fields: Mapping[str, Any]) -> None:
    if fields.get("schema_version") != SCHEMA_VERSION:
        raise CapabilityProofError("proof schema is invalid")
    if (
        not fields.get("method")
        or not fields.get("path")
        or not all(
            isinstance(fields.get(key), str) and fields[key]
            for key in (
                "method",
                "path",
                "body_sha256",
                "tenant_id",
                "user_id",
                "session_id",
                "execution_id",
                "run_id",
                "nonce",
            )
        )
    ):
        raise CapabilityProofError("proof scope is invalid")
    expires_at = fields.get("expires_at")
    if isinstance(expires_at, bool) or not isinstance(expires_at, int):
        raise CapabilityProofError("proof expiry is invalid")
    if expires_at <= 0 or len(str(fields["nonce"])) > 128:
        raise CapabilityProofError("proof expiry or nonce is invalid")
    path = str(fields["path"])
    if len(path) > 2048 or any(ord(char) < 32 for char in path):
        raise CapabilityProofError("proof path is invalid")
    body_sha256 = str(fields["body_sha256"])
    if len(body_sha256) != 64 or any(char not in "0123456789abcdef" for char in body_sha256):
        raise CapabilityProofError("proof body hash is invalid")
    for key in ("tenant_id", "user_id", "session_id", "execution_id", "run_id", "nonce"):
        value = str(fields[key])
        if len(value) > 255 or any(ord(char) < 32 for char in value):
            raise CapabilityProofError("proof scope is invalid")


def _signature(secret: str | bytes, fields: Mapping[str, Any]) -> str:
    return hmac.new(_secret_bytes(secret), canonical_json(dict(fields)), hashlib.sha256).hexdigest()


def sign_capability_proof(
    secret: str | bytes,
    *,
    method: str,
    path: str,
    body: Any,
    tenant_id: str,
    user_id: str,
    session_id: str,
    execution_id: str,
    run_id: str,
    expires_at: int | None = None,
    nonce: str,
    now: int | None = None,
) -> str:
    """Return the compact header value for a capability request."""

    current = int(time.time()) if now is None else int(now)
    expiry = current + 30 if expires_at is None else int(expires_at)
    if expiry <= current or expiry > current + MAX_TTL_SECONDS:
        raise CapabilityProofError("proof expiry is outside the allowed TTL")
    fields = _unsigned_fields(
        method=method,
        path=path,
        body_sha256=canonical_body_hash(body),
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        execution_id=execution_id,
        run_id=run_id,
        expires_at=expiry,
        nonce=nonce,
    )
    envelope = {**fields, "signature": _signature(secret, fields)}
    encoded = base64.urlsafe_b64encode(canonical_json(envelope)).rstrip(b"=").decode("ascii")
    return f"v1.{encoded}"


def _decode(header: str) -> tuple[dict[str, Any], str]:
    if not isinstance(header, str) or not header.startswith("v1."):
        raise CapabilityProofError("proof header is malformed")
    encoded = header[3:]
    if not encoded or len(encoded) > 4096:
        raise CapabilityProofError("proof header is malformed")
    try:
        padded = encoded + "=" * (-len(encoded) % 4)
        raw = base64.urlsafe_b64decode(padded.encode("ascii"))
        envelope = json.loads(raw)
    except (ValueError, UnicodeError, json.JSONDecodeError) as exc:
        raise CapabilityProofError("proof header is malformed") from exc
    if not isinstance(envelope, dict):
        raise CapabilityProofError("proof header is malformed")
    signature = envelope.pop("signature", None)
    if not isinstance(signature, str) or len(signature) != 64:
        raise CapabilityProofError("proof signature is malformed")
    if set(envelope) != {
        "schema_version",
        "method",
        "path",
        "body_sha256",
        "tenant_id",
        "user_id",
        "session_id",
        "execution_id",
        "run_id",
        "expires_at",
        "nonce",
    }:
        raise CapabilityProofError("proof fields are invalid")
    return envelope, signature


def verify_capability_proof(
    secret: str | bytes,
    header: str,
    *,
    method: str,
    path: str,
    body: Any,
    tenant_id: str,
    user_id: str,
    session_id: str,
    execution_id: str,
    run_id: str,
    now: int | None = None,
) -> CapabilityProof:
    """Verify all request bindings using constant-time signature comparison.

    There is intentionally no process-local nonce cache: it would not protect
    a multi-replica deployment.  The 120-second maximum TTL, execution
    idempotency key and durable dispatch fence provide bounded replay safety.
    """

    fields, provided_signature = _decode(header)
    _validate_unsigned_fields(fields)
    expected_signature = _signature(secret, fields)
    if not hmac.compare_digest(provided_signature, expected_signature):
        raise CapabilityProofError("proof signature mismatch")
    current = int(time.time()) if now is None else int(now)
    if (
        int(fields["expires_at"]) <= current
        or int(fields["expires_at"]) > current + MAX_TTL_SECONDS
    ):
        raise CapabilityProofError("proof expired")
    expected = {
        "method": method.upper().strip(),
        "path": path,
        "body_sha256": canonical_body_hash(body),
        "tenant_id": tenant_id,
        "user_id": user_id,
        "session_id": session_id,
        "execution_id": execution_id,
        "run_id": run_id,
    }
    if any(
        not hmac.compare_digest(str(fields[key]), str(value)) for key, value in expected.items()
    ):
        raise CapabilityProofError("proof request binding mismatch")
    return CapabilityProof(
        method=str(fields["method"]),
        path=str(fields["path"]),
        body_sha256=str(fields["body_sha256"]),
        tenant_id=str(fields["tenant_id"]),
        user_id=str(fields["user_id"]),
        session_id=str(fields["session_id"]),
        execution_id=str(fields["execution_id"]),
        run_id=str(fields["run_id"]),
        expires_at=int(fields["expires_at"]),
        nonce=str(fields["nonce"]),
        signature=provided_signature,
    )


__all__ = [
    "CapabilityProof",
    "CapabilityProofError",
    "HEADER_NAME",
    "MAX_TTL_SECONDS",
    "SCHEMA_VERSION",
    "canonical_body_hash",
    "canonical_json",
    "sign_capability_proof",
    "verify_capability_proof",
]
