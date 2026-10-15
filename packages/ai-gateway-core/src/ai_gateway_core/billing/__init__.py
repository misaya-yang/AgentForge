"""Pricing and token-cost helpers shared by Gateway and model adapters.

Was at ``src/services/billing/`` until Phase 5f Batch C; moved here so
metrics modules (also moved to ai_gateway_core) can resolve their
``pricing_catalog`` import without depending on gateway src/.
"""

from ai_gateway_contracts.pricing import build_pricing_snapshot, validate_pricing_snapshot

from .pricing_catalog import (
    DEFAULT_TOKEN_PRICING_PER_1K_USD,
    microcents_to_usd,
    resolve_pricing,
    resolve_pricing_with_status,
    usd_to_microcents,
)

__all__ = [
    "DEFAULT_TOKEN_PRICING_PER_1K_USD",
    "build_pricing_snapshot",
    "validate_pricing_snapshot",
    "microcents_to_usd",
    "resolve_pricing",
    "resolve_pricing_with_status",
    "usd_to_microcents",
]
