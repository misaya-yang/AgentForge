from __future__ import annotations

import json
import re
from collections.abc import Mapping, Sequence
from typing import Any

SENSITIVE_FIELD_NAMES = {
    "api_key",
    "_api_key",
    "authorization",
    "cookie",
    "set-cookie",
    "auth_token",
    "jwt_secret",
    "api_keys",
    "allowed_api_keys",
    "password",
    "x-api-key",
    "langsmith_api_key",
    "provider_api_key",
}

PRESERVE_FIELD_NAMES = {
    "api_key_fingerprint",
    "fingerprint",
    "key_fingerprint",
}

_SENSITIVE_TEXT_RE = re.compile(
    r'(?P<prefix>"?(?:_?api_key|authorization|cookie|set-cookie|auth_token|password|x-api-key|langsmith_api_key|provider_api_key)"?\s*[:=]\s*)'
    r'(?P<value>"[^"]*"|[^,\s}\]]+)',
    re.IGNORECASE,
)

_BEARER_RE = re.compile(r"\bBearer\s+[A-Za-z0-9._~+/=-]+", re.IGNORECASE)


def _is_sensitive_key(key: object) -> bool:
    normalized = str(key or "").strip().lower()
    if normalized in PRESERVE_FIELD_NAMES:
        return False
    return normalized in SENSITIVE_FIELD_NAMES or normalized.endswith("_api_key")


def redact_sensitive_data(value: Any) -> Any:
    """Recursively redact runtime secrets while preserving safe fingerprints."""
    if isinstance(value, Mapping):
        redacted: dict[Any, Any] = {}
        for key, item in value.items():
            if _is_sensitive_key(key):
                redacted[key] = "***"
            else:
                redacted[key] = redact_sensitive_data(item)
        return redacted

    if isinstance(value, tuple):
        return tuple(redact_sensitive_data(item) for item in value)

    if isinstance(value, Sequence) and not isinstance(value, (str, bytes, bytearray)):
        return [redact_sensitive_data(item) for item in value]

    if isinstance(value, str):
        return redact_sensitive_text(value)

    return value


def redact_sensitive_text(value: str) -> str:
    """Redact secret-looking values embedded in log or upstream error text."""
    if not value:
        return ""

    text = str(value)
    try:
        parsed = json.loads(text)
    except Exception:
        parsed = None

    if parsed is not None:
        try:
            return json.dumps(redact_sensitive_data(parsed), ensure_ascii=False, separators=(",", ":"))
        except Exception:
            pass

    text = _BEARER_RE.sub("Bearer ***", text)
    return _SENSITIVE_TEXT_RE.sub(lambda m: f'{m.group("prefix")}"***"', text)
