"""
Encryption utilities for sensitive data.

Provides symmetric encryption for API tokens and other secrets.
Uses Fernet (AES-128-CBC with HMAC) for authenticated encryption.
"""

from __future__ import annotations

import base64
import hashlib
import logging
import os
from functools import lru_cache

logger = logging.getLogger(__name__)

# Try to import cryptography, fall back to no encryption if not available
try:
    from cryptography.fernet import Fernet, InvalidToken

    CRYPTOGRAPHY_AVAILABLE = True
except ImportError:
    CRYPTOGRAPHY_AVAILABLE = False
    InvalidToken = Exception  # Fallback type


def _derive_fernet_key(secret: str) -> bytes:
    """
    Derive a Fernet-compatible key from an arbitrary secret string.

    Fernet requires a 32-byte URL-safe base64-encoded key.
    We use SHA-256 to derive a consistent key from the secret.
    """
    # Hash the secret to get exactly 32 bytes
    key_bytes = hashlib.sha256(secret.encode()).digest()
    # Fernet expects URL-safe base64 encoding
    return base64.urlsafe_b64encode(key_bytes)


@lru_cache(maxsize=1)
def _get_fernet(encryption_key: str) -> Fernet | None:
    """Get cached Fernet instance for the given key."""
    if not CRYPTOGRAPHY_AVAILABLE:
        return None
    if not encryption_key:
        return None
    try:
        derived_key = _derive_fernet_key(encryption_key)
        return Fernet(derived_key)
    except Exception as e:
        logger.error(f"Failed to initialize Fernet cipher: {e}")
        return None


def encrypt_value(value: str, encryption_key: str) -> str:
    """
    Encrypt a sensitive value.

    Args:
        value: The plaintext value to encrypt
        encryption_key: The encryption key/secret

    Returns:
        Encrypted value prefixed with 'enc:' for identification,
        or the original value if encryption is not available.
    """
    if not value:
        return value

    if not encryption_key:
        logger.warning("No encryption key configured, storing value in plaintext")
        return value

    fernet = _get_fernet(encryption_key)
    if not fernet:
        logger.warning("Encryption not available, storing value in plaintext")
        return value

    try:
        encrypted = fernet.encrypt(value.encode())
        # Prefix with 'enc:' to identify encrypted values
        return f"enc:{encrypted.decode()}"
    except Exception as e:
        logger.error(f"Encryption failed: {e}")
        return value


def decrypt_value(value: str, encryption_key: str) -> str:
    """
    Decrypt an encrypted value.

    Args:
        value: The encrypted value (prefixed with 'enc:') or plaintext
        encryption_key: The encryption key/secret

    Returns:
        Decrypted plaintext value, or the original if not encrypted.
    """
    if not value:
        return value

    # Check if value is encrypted (has 'enc:' prefix)
    if not value.startswith("enc:"):
        # Value is not encrypted, return as-is
        return value

    if not encryption_key:
        logger.warning("No encryption key configured, cannot decrypt value")
        return value

    fernet = _get_fernet(encryption_key)
    if not fernet:
        logger.warning("Decryption not available")
        return value

    try:
        # Remove 'enc:' prefix and decrypt
        encrypted_data = value[4:].encode()
        decrypted = fernet.decrypt(encrypted_data)
        return decrypted.decode()
    except InvalidToken:
        logger.error("Decryption failed: invalid token (wrong key or corrupted data)")
        return value
    except Exception as e:
        logger.error(f"Decryption failed: {e}")
        return value


def generate_encryption_key() -> str:
    """
    Generate a new random encryption key.

    Returns:
        A 32-character hex string suitable for use as an encryption key.
    """
    return os.urandom(16).hex()


def is_encrypted(value: str) -> bool:
    """Check if a value appears to be encrypted."""
    return value.startswith("enc:") if value else False


# =============================================================================
# URL Signing for Secure Image Access
# =============================================================================


def sign_url(url: str, secret_key: str, expiry_seconds: int = 3600) -> str:
    """
    Sign a URL with HMAC-SHA256 for secure access.

    Creates a signed URL that includes:
    - Original URL
    - Expiration timestamp
    - HMAC signature

    Args:
        url: The URL to sign (e.g., file:///path/to/image.png)
        secret_key: Secret key for HMAC signing
        expiry_seconds: URL validity duration in seconds (default: 1 hour)

    Returns:
        Signed URL with signature and expiry query parameters

    Example:
        file:///path/to/image.png?expires=1704067200&sig=abc123...
    """
    import hmac
    import time
    from urllib.parse import parse_qs, urlencode, urlparse, urlunparse

    if not url or not secret_key:
        return url

    # Calculate expiry timestamp
    expires = int(time.time()) + expiry_seconds

    # Parse URL and add expiry
    parsed = urlparse(url)
    query_params = parse_qs(parsed.query)
    query_params["expires"] = [str(expires)]

    # Build the string to sign (URL path + expiry)
    sign_string = f"{parsed.path}:{expires}"

    # Generate HMAC-SHA256 signature
    signature = hmac.new(
        secret_key.encode("utf-8"),
        sign_string.encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()[:32]  # Use first 32 chars for brevity

    query_params["sig"] = [signature]

    # Reconstruct URL with signature
    new_query = urlencode(query_params, doseq=True)
    signed_url = urlunparse(
        (
            parsed.scheme,
            parsed.netloc,
            parsed.path,
            parsed.params,
            new_query,
            parsed.fragment,
        )
    )

    return signed_url


def verify_signed_url(url: str, secret_key: str) -> tuple[bool, str]:
    """
    Verify a signed URL's authenticity and expiration.

    Args:
        url: The signed URL to verify
        secret_key: Secret key used for signing

    Returns:
        Tuple of (is_valid, error_message)
        - (True, "") if valid
        - (False, "reason") if invalid

    Example:
        valid, error = verify_signed_url(signed_url, secret_key)
        if not valid:
            raise HTTPException(403, error)
    """
    import hmac
    import time
    from urllib.parse import parse_qs, urlparse

    if not url:
        return False, "Empty URL"

    if not secret_key:
        # No secret key configured - allow unsigned URLs for backward compatibility
        # but log a warning
        logger.warning("URL signature verification skipped: no secret key configured")
        return True, ""

    parsed = urlparse(url)
    query_params = parse_qs(parsed.query)

    # Extract signature and expiry
    sig_list = query_params.get("sig", [])
    expires_list = query_params.get("expires", [])

    if not sig_list or not expires_list:
        return False, "Missing signature or expiry"

    provided_sig = sig_list[0]
    try:
        expires = int(expires_list[0])
    except ValueError:
        return False, "Invalid expiry format"

    # Check expiration
    if time.time() > expires:
        return False, "URL has expired"

    # Recreate the signature
    sign_string = f"{parsed.path}:{expires}"
    expected_sig = hmac.new(
        secret_key.encode("utf-8"),
        sign_string.encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()[:32]

    # Constant-time comparison to prevent timing attacks
    if not hmac.compare_digest(provided_sig, expected_sig):
        return False, "Invalid signature"

    return True, ""


def get_unsigned_url(url: str) -> str:
    """
    Strip signature parameters from a signed URL.

    Useful for getting the original URL path for file access.

    Args:
        url: Signed URL with sig and expires parameters

    Returns:
        URL without signature parameters
    """
    from urllib.parse import parse_qs, urlencode, urlparse, urlunparse

    if not url:
        return url

    parsed = urlparse(url)
    query_params = parse_qs(parsed.query)

    # Remove signature-related params
    query_params.pop("sig", None)
    query_params.pop("expires", None)

    # Reconstruct URL
    new_query = urlencode(query_params, doseq=True) if query_params else ""
    return urlunparse(
        (
            parsed.scheme,
            parsed.netloc,
            parsed.path,
            parsed.params,
            new_query,
            parsed.fragment,
        )
    )
