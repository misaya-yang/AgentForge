"""Shim identity tests for the ARC-04 first batch.

PRD §ARC-04 goal 5: every moved protocol keeps a thin compatibility shim in
``ai_gateway_core`` until all consumers migrate.  These tests lock that the
shims re-export the *same objects* (not copies) and the full public surface,
so code mixing import paths keeps working (isinstance checks, dataclass
equality, pydantic registry lookups).

Delete this file together with the shims once consumers import
``ai_gateway_contracts`` directly (see each shim's removal conditions).
"""

from __future__ import annotations

import importlib

import ai_gateway_core.agents.runtime as runtime_shim
import ai_gateway_core.agents.runtime_lease as runtime_lease_shim
import ai_gateway_core.auth.capability_proof as capability_proof_shim
import ai_gateway_core.events.envelope as event_envelope_shim
import ai_gateway_core.events.errors as event_errors_shim
import pytest

STATIC_SHIMS = {
    "ai_gateway_core.agents.runtime": runtime_shim,
    "ai_gateway_core.agents.runtime_lease": runtime_lease_shim,
    "ai_gateway_core.auth.capability_proof": capability_proof_shim,
    "ai_gateway_core.events.envelope": event_envelope_shim,
    "ai_gateway_core.events.errors": event_errors_shim,
}

# (core shim module, contracts module, public names that must be identical)
SHIM_PAIRS: list[tuple[str, str, tuple[str, ...]]] = [
    (
        "ai_gateway_core.auth.capability_proof",
        "ai_gateway_contracts.capability_proof",
        (
            "CapabilityProof",
            "CapabilityProofError",
            "HEADER_NAME",
            "MAX_TTL_SECONDS",
            "SCHEMA_VERSION",
            "canonical_body_hash",
            "canonical_json",
            "sign_capability_proof",
            "verify_capability_proof",
        ),
    ),
    (
        "ai_gateway_core.agents.runtime",
        "ai_gateway_contracts.agent_runtime",
        (
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
        ),
    ),
    (
        "ai_gateway_core.agents.runtime_lease",
        "ai_gateway_contracts.agent_runtime_lease",
        (
            "RUNTIME_MODEL_LEASE_SCHEMA_VERSION",
            "RuntimeModelLeaseClaims",
            "RuntimeModelLeaseError",
            "RuntimeModelLeaseSigner",
        ),
    ),
    (
        "ai_gateway_core.events.envelope",
        "ai_gateway_contracts.event_envelope",
        (
            "EventEnvelope",
            "UsageRecordedV1",
            "parse_envelope",
            "payload_for",
        ),
    ),
    (
        "ai_gateway_core.events.errors",
        "ai_gateway_contracts.event_errors",
        (
            "EventBusError",
            "EventDeserializationError",
            "EventHandlerError",
        ),
    ),
]


@pytest.mark.parametrize(
    ("shim_path", "contracts_path", "names"),
    SHIM_PAIRS,
    ids=[pair[0] for pair in SHIM_PAIRS],
)
def test_shim_reexports_same_objects(
    shim_path: str, contracts_path: str, names: tuple[str, ...]
) -> None:
    shim = STATIC_SHIMS[shim_path]
    contracts = importlib.import_module(contracts_path)
    for name in names:
        assert getattr(shim, name) is getattr(contracts, name), f"{shim_path}.{name}"


@pytest.mark.parametrize(
    ("shim_path", "contracts_path", "names"),
    SHIM_PAIRS,
    ids=[pair[0] for pair in SHIM_PAIRS],
)
def test_shim_all_matches_public_surface(
    shim_path: str, contracts_path: str, names: tuple[str, ...]
) -> None:
    shim = STATIC_SHIMS[shim_path]
    contracts = importlib.import_module(contracts_path)
    assert sorted(shim.__all__) == sorted(names)
    # Every public contracts name must be reachable through the shim so a
    # future addition to the contracts public surface cannot be silently
    # dropped.  ``agent_runtime`` predates ``__all__`` — fall back to the
    # locked name list there.
    public = set(getattr(contracts, "__all__", names))
    missing = public - set(dir(shim))
    assert not missing, f"{shim_path} does not re-export {sorted(missing)}"


def test_events_package_facade_uses_contract_classes() -> None:
    from ai_gateway_contracts.event_envelope import (
        EventEnvelope as ContractEnvelope,
    )
    from ai_gateway_contracts.event_envelope import (
        UsageRecordedV1 as ContractUsage,
    )
    from ai_gateway_contracts.event_envelope import (
        parse_envelope as contract_parse,
    )
    from ai_gateway_contracts.event_errors import (
        EventDeserializationError as ContractDeserializationError,
    )
    from ai_gateway_core.events import (
        EventDeserializationError,
        EventEnvelope,
        UsageRecordedV1,
        parse_envelope,
    )

    assert EventEnvelope is ContractEnvelope
    assert UsageRecordedV1 is ContractUsage
    assert parse_envelope is contract_parse
    assert EventDeserializationError is ContractDeserializationError


def test_agents_package_facade_uses_contract_classes() -> None:
    from ai_gateway_contracts.agent_runtime import (
        AgentRuntimeSigner as ContractSigner,
    )
    from ai_gateway_contracts.agent_runtime import (
        runtime_sha256 as contract_sha256,
    )
    from ai_gateway_contracts.agent_runtime_lease import (
        RUNTIME_MODEL_LEASE_SCHEMA_VERSION as CONTRACT_LEASE_VERSION,
    )
    from ai_gateway_contracts.agent_runtime_lease import (
        RuntimeModelLeaseClaims as ContractClaims,
    )
    from ai_gateway_core.agents import (
        RUNTIME_MODEL_LEASE_SCHEMA_VERSION,
        AgentRuntimeSigner,
        RuntimeModelLeaseClaims,
        runtime_sha256,
    )

    assert AgentRuntimeSigner is ContractSigner
    assert runtime_sha256 is contract_sha256
    assert RuntimeModelLeaseClaims is ContractClaims
    assert RUNTIME_MODEL_LEASE_SCHEMA_VERSION == CONTRACT_LEASE_VERSION


def test_auth_package_facade_uses_contract_classes() -> None:
    from ai_gateway_contracts.capability_proof import (
        CapabilityProof as ContractCapabilityProof,
    )
    from ai_gateway_contracts.capability_proof import (
        sign_capability_proof as contract_sign,
    )
    from ai_gateway_core.auth import CapabilityProof, sign_capability_proof

    assert CapabilityProof is ContractCapabilityProof
    assert sign_capability_proof is contract_sign


def test_gateway_secret_replay_store_is_contract_protocol() -> None:
    from ai_gateway_contracts.replay import (
        InMemoryReplayStore as ContractInMemory,
    )
    from ai_gateway_contracts.replay import ReplayStore as ContractReplayStore
    from ai_gateway_core.auth import (
        InMemoryReplayStore,
        ReplayStore,
    )

    assert InMemoryReplayStore is ContractInMemory
    assert ReplayStore is ContractReplayStore
