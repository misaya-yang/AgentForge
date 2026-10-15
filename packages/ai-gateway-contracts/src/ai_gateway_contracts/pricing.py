"""Pure immutable price receipt contract shared by launch and settlement.

No I/O, environment configuration, cache or pricing authority lives here.
"""

from __future__ import annotations

import hashlib
import json
from decimal import Decimal
from typing import Any


def build_pricing_snapshot(
    *, tenant_id: str, provider_id: str, model_id: str,
    input_price_per_1k: Any, output_price_per_1k: Any,
    pricing_status: str = "tenant_model",
) -> dict[str, Any]:
    """Keep Decimal precision and derive a version from the actual price terms."""
    prices = [Decimal(str(value)) for value in (input_price_per_1k, output_price_per_1k)]
    if any(not value.is_finite() or value < 0 for value in prices):
        raise ValueError("invalid_model_price")
    receipt = {
        "tenant_id": tenant_id, "provider_id": provider_id, "model_id": model_id,
        "currency": "USD", "unit": "per_1k_tokens",
        "input_price_per_1k": format(prices[0].normalize(), "f"),
        "output_price_per_1k": format(prices[1].normalize(), "f"),
        "pricing_status": pricing_status,
    }
    encoded = json.dumps(receipt, sort_keys=True, separators=(",", ":")).encode()
    return {**receipt, "version": hashlib.sha256(encoded).hexdigest()}


def validate_pricing_snapshot(
    value: Any, *, tenant_id: str, provider_id: str, model_id: str,
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("invalid_pricing_snapshot")
    if any(value.get(key) != expected for key, expected in (
        ("tenant_id", tenant_id), ("provider_id", provider_id), ("model_id", model_id),
        ("currency", "USD"), ("unit", "per_1k_tokens"),
    )):
        raise ValueError("pricing_snapshot_identity_mismatch")
    receipt = build_pricing_snapshot(
        tenant_id=tenant_id, provider_id=provider_id, model_id=model_id,
        input_price_per_1k=value.get("input_price_per_1k"),
        output_price_per_1k=value.get("output_price_per_1k"),
        pricing_status=str(value.get("pricing_status") or "tenant_model"),
    )
    if receipt["version"] != value.get("version"):
        raise ValueError("pricing_snapshot_version_mismatch")
    return receipt
