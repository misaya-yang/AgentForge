"""Request fingerprinting for settlement idempotency."""

from __future__ import annotations

import hashlib
import json
from collections.abc import Mapping
from typing import Any


def request_fingerprint(request: Mapping[str, Any]) -> str:
    """Return a tenant-scoped fingerprint for one logical settlement request."""
    identity = {
        "tenant_id": request["tenant_id"],
        "external_id": request["external_id"],
        "metadata": request.get("metadata", {}),
    }
    encoded = json.dumps(identity, ensure_ascii=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()
