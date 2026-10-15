"""Security primitives shared across gateway and microservices."""

from .redaction import REDACTION_PATTERNS, SENSITIVE_KEY_RE, redact_trace_text
from .safe_fetch import (
    SafeFetchError,
    SafeFetchResponse,
    is_safe_destination,
    safe_callback_post,
    safe_fetch,
    safe_fetch_with_response,
    safe_form_post,
    validate_callback_url,
)
from .secrets import decrypt_value, encrypt_value, generate_encryption_key, is_encrypted

__all__ = [
    "REDACTION_PATTERNS",
    "SENSITIVE_KEY_RE",
    "SafeFetchError",
    "SafeFetchResponse",
    "decrypt_value",
    "encrypt_value",
    "generate_encryption_key",
    "is_safe_destination",
    "is_encrypted",
    "redact_trace_text",
    "safe_callback_post",
    "safe_fetch",
    "safe_fetch_with_response",
    "safe_form_post",
    "validate_callback_url",
]
