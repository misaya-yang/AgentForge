"""Compatibility shim — implementation moved to ``ai_gateway_contracts``.

ARC-04 first batch (2026-08-29): the event-bus error taxonomy now lives in
``ai_gateway_contracts.event_errors`` because the envelope parser
(``parse_envelope``) raises ``EventDeserializationError`` as part of the
wire contract.  This module only re-exports it so existing import paths keep
working.

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

from ai_gateway_contracts.event_errors import (  # noqa: F401
    EventBusError,
    EventDeserializationError,
    EventHandlerError,
)

__all__ = [
    "EventBusError",
    "EventDeserializationError",
    "EventHandlerError",
]
