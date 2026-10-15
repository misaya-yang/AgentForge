"""Versioned, owner/origin-bound credentials; legacy reads never rewrite records."""

from __future__ import annotations

import json

from ai_gateway_core.security.secrets import decrypt_value, encrypt_value

from .models import ConfluenceCredentials

PREFIX = "cfcred:v1:"
MARKER = "encrypted:cfcred:v1"


class CredentialUnavailable(ValueError):
    """Only fixed, secret-free diagnostics may leave this boundary."""


def require_key(key: str) -> None:
    if not isinstance(key, str) or not key.strip():
        raise CredentialUnavailable(
            "Configure GATEWAY_ENCRYPTION_KEY for Knowledge API and workers"
        )


def _scope(row):
    return {k: row.get(k) for k in ("connection_id", "tenant_id", "domain")} | {
        "owner_id": row.get("owner_id") or row.get("created_by")
    }


def seal_credentials(row, email: str, api_token: str, key: str) -> str:
    require_key(key)
    plain = json.dumps(
        {"scope": _scope(row), "email": email, "api_token": api_token}, sort_keys=True
    )
    try:
        encrypted = encrypt_value(plain, key)
        # Shared compatibility helpers may fail open. This caller must not.
        if (
            not encrypted.startswith("enc:")
            or encrypted == plain
            or decrypt_value(encrypted, key) != plain
        ):
            raise CredentialUnavailable("Credential encryption is unavailable")
        return PREFIX + encrypted
    except Exception:
        raise CredentialUnavailable("Credential encryption is unavailable") from None


def connection_credentials(row, key: str) -> ConfluenceCredentials:
    stored = row.get("api_token")
    marker = row.get("email")
    if not isinstance(stored, str) or not stored:
        raise CredentialUnavailable("Saved credentials are unavailable")
    if marker == MARKER or stored.startswith(("cfcred:", "enc:")):
        require_key(key)
        if marker != MARKER or not stored.startswith(PREFIX + "enc:"):
            raise CredentialUnavailable("Saved credential format is unavailable")
        encrypted = stored[len(PREFIX) :]
        try:
            plain = decrypt_value(encrypted, key)
            if plain == encrypted:
                raise ValueError()
            data = json.loads(plain)
            if data.get("scope") != _scope(row) or any(
                not isinstance(data.get(k), str) or not data[k] for k in ("email", "api_token")
            ):
                raise ValueError()
            return ConfluenceCredentials(row["domain"], data["email"], data["api_token"])
        except Exception:
            raise CredentialUnavailable(
                "Saved credentials cannot be decrypted; check the configured encryption key"
            ) from None
    # Explicit compatibility for previously provisioned plaintext records only.
    # No implicit migration, persistence, re-encryption or new legacy writes.
    if not isinstance(marker, str) or not marker or marker.startswith("encrypted:"):
        raise CredentialUnavailable("Saved credential format is unavailable")
    return ConfluenceCredentials(row["domain"], marker, stored)
