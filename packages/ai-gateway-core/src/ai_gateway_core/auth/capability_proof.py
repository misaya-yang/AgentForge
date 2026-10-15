"""Compatibility shim — implementation moved to ``ai_gateway_contracts``.

ARC-04 first batch (2026-08-29): the capability-proof protocol now lives in
``ai_gateway_contracts.capability_proof``.  This module only re-exports it so
existing import paths keep working.

Consumers still importing through this path
(reports/inventory/core-import-inventory.json, ``shim_consumers``):

- compatibility facade: ``ai_gateway_core.auth``;
- identity contract: ``packages/ai-gateway-contracts/tests/test_shim_identity.py``.

Removal conditions (PRD §ARC-04 goal 5): after the ARC-08 external
compatibility window, require the generated inventory to show no repository
consumers other than the facade and identity contract above; then remove the
shim, its facade exports, and its identity-test row in one change.
"""

from __future__ import annotations

from ai_gateway_contracts.capability_proof import (  # noqa: F401
    HEADER_NAME,
    MAX_TTL_SECONDS,
    SCHEMA_VERSION,
    CapabilityProof,
    CapabilityProofError,
    canonical_body_hash,
    canonical_json,
    sign_capability_proof,
    verify_capability_proof,
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
