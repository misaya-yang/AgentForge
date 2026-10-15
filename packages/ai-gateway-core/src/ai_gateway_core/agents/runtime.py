"""Compatibility shim — implementation moved to ``ai_gateway_contracts``.

ARC-04 first batch (2026-08-29): the Agent Runtime snapshot/envelope protocol
now lives in ``ai_gateway_contracts.agent_runtime`` (replay-store protocol in
``ai_gateway_contracts.replay``).  This module only re-exports it so existing
import paths keep working.

Consumers still importing through this path
(reports/inventory/core-import-inventory.json, ``shim_consumers``):

- compatibility facade: ``ai_gateway_core.agents``;
- identity contract: ``packages/ai-gateway-contracts/tests/test_shim_identity.py``.

Removal conditions (PRD §ARC-04 goal 5): after the ARC-08 external
compatibility window, require the generated inventory to show no repository
consumers other than the facade and identity contract above; then remove the
shim, its facade exports, and its identity-test row in one change.
"""

from __future__ import annotations

from ai_gateway_contracts.agent_runtime import (  # noqa: F401
    AGENT_RUNTIME_ENVELOPE_SCHEMA_VERSION,
    AGENT_RUNTIME_SCHEMA_VERSION,
    AgentRuntimeEnvelopeError,
    AgentRuntimeSigner,
    VerifiedAgentRuntime,
    agent_memory_principal,
    canonical_runtime_json,
    runtime_sha256,
)
from ai_gateway_contracts.replay import InMemoryReplayStore, ReplayStore  # noqa: F401

__all__ = [
    "AGENT_RUNTIME_ENVELOPE_SCHEMA_VERSION",
    "AGENT_RUNTIME_SCHEMA_VERSION",
    "AgentRuntimeEnvelopeError",
    "AgentRuntimeSigner",
    "InMemoryReplayStore",
    "ReplayStore",
    "VerifiedAgentRuntime",
    "agent_memory_principal",
    "canonical_runtime_json",
    "runtime_sha256",
]
