"""Compatibility shim — implementation moved to ``ai_gateway_contracts``.

ARC-04 first batch (2026-08-29): the event envelope protocol now lives in
``ai_gateway_contracts.event_envelope`` (error taxonomy in
``ai_gateway_contracts.event_errors``).  This module only re-exports it so
existing import paths keep working.  ``_PAYLOAD_MODELS`` is re-exported too
because the schema-evolution contract test reads it.

Consumers still importing through this path
(reports/inventory/core-import-inventory.json, ``shim_consumers``):

- compatibility facade: ``ai_gateway_core.events``;
- identity contract: ``packages/ai-gateway-contracts/tests/test_shim_identity.py``.

Removal conditions (PRD §ARC-04 goal 5): after the ARC-08 external
compatibility window, require the generated inventory to show no repository
consumers other than the facade and identity contract above; then remove the
shim, its facade exports, and its identity-test row in one change.
"""

from __future__ import annotations

from ai_gateway_contracts.event_envelope import (  # noqa: F401
    _PAYLOAD_MODELS,
    EventEnvelope,
    PayloadT,
    UsageRecordedV1,
    parse_envelope,
    payload_for,
)

__all__ = [
    "EventEnvelope",
    "UsageRecordedV1",
    "parse_envelope",
    "payload_for",
]
