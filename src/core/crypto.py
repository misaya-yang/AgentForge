"""
Encryption utilities for sensitive data.

Provides symmetric encryption for API tokens and other secrets.
Uses Fernet (AES-128-CBC with HMAC) for authenticated encryption.
"""

from __future__ import annotations

import hashlib
import logging

from ai_gateway_core import security as _security

decrypt_value = _security.decrypt_value
encrypt_value = _security.encrypt_value
generate_encryption_key = _security.generate_encryption_key
is_encrypted = _security.is_encrypted

logger = logging.getLogger(__name__)


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
