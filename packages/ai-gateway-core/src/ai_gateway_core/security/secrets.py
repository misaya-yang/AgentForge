"""Authenticated encryption helpers shared by Gateway and Assistant."""

from __future__ import annotations

import base64
import hashlib
import logging
import os
from functools import lru_cache

from cryptography.fernet import Fernet, InvalidToken

logger = logging.getLogger(__name__)


def _derive_fernet_key(secret: str) -> bytes:
    return base64.urlsafe_b64encode(hashlib.sha256(secret.encode()).digest())


@lru_cache(maxsize=8)
def _get_fernet(encryption_key: str) -> Fernet | None:
    if not encryption_key:
        return None
    try:
        return Fernet(_derive_fernet_key(encryption_key))
    except Exception as exc:
        logger.error("Failed to initialize secret cipher (%s)", type(exc).__name__)
        return None


def encrypt_value(value: str, encryption_key: str) -> str:
    """Encrypt ``value`` while preserving the legacy ``enc:`` storage format."""

    if not value:
        return value
    fernet = _get_fernet(encryption_key)
    if fernet is None:
        logger.warning("No encryption key configured; storing value in plaintext")
        return value
    try:
        return f"enc:{fernet.encrypt(value.encode()).decode()}"
    except Exception as exc:
        logger.error("Secret encryption failed (%s)", type(exc).__name__)
        return value


def decrypt_value(value: str, encryption_key: str) -> str:
    """Decrypt an ``enc:`` value; plaintext legacy values pass through."""

    if not value or not value.startswith("enc:"):
        return value
    fernet = _get_fernet(encryption_key)
    if fernet is None:
        logger.warning("No encryption key configured; encrypted value is unavailable")
        return value
    try:
        return fernet.decrypt(value[4:].encode()).decode()
    except InvalidToken:
        logger.error("Secret decryption failed: invalid token")
        return value
    except Exception as exc:
        logger.error("Secret decryption failed (%s)", type(exc).__name__)
        return value


def generate_encryption_key() -> str:
    """Return a local encryption secret suitable for ``GATEWAY_ENCRYPTION_KEY``."""

    return os.urandom(16).hex()


def is_encrypted(value: str) -> bool:
    return bool(value and value.startswith("enc:"))


__all__ = [
    "decrypt_value",
    "encrypt_value",
    "generate_encryption_key",
    "is_encrypted",
]
