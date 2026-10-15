from __future__ import annotations

from dataclasses import replace

import pytest
from ai_gateway_contracts.agent_runtime_lease import (
    RUNTIME_MODEL_LEASE_SCHEMA_VERSION,
    RuntimeModelLeaseClaims,
    RuntimeModelLeaseError,
    RuntimeModelLeaseSigner,
)


def _claims() -> RuntimeModelLeaseClaims:
    return RuntimeModelLeaseClaims(
        schema_version=RUNTIME_MODEL_LEASE_SCHEMA_VERSION,
        lease_id="019d0000-0000-7000-8000-000000000001",
        snapshot_id="019d0000-0000-7000-8000-000000000002",
        run_id="019d0000-0000-7000-8000-000000000003",
        runtime_thread_id="019d0000-0000-7000-8000-000000000004",
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        provider_id="provider-a",
        model_id="model-a",
        capability_revision=3,
        issued_at_ms=1_000,
        expires_at_ms=61_000,
        nonce_sha256="a" * 64,
    )


def test_runtime_model_lease_signature_binds_every_scope_dimension() -> None:
    signer = RuntimeModelLeaseSigner("runtime-model-lease-test-secret")
    claims = _claims()
    signature = signer.sign(claims)

    assert signature.startswith("v1:")
    assert len(signature) == 67
    signer.verify(signature, claims)

    with pytest.raises(RuntimeModelLeaseError, match="SIGNATURE_INVALID"):
        signer.verify(signature, replace(claims, tenant_id="tenant-b"))
    with pytest.raises(RuntimeModelLeaseError, match="SIGNATURE_INVALID"):
        signer.verify(signature, replace(claims, model_id="model-b"))


def test_runtime_model_lease_rejects_invalid_time_and_nonce() -> None:
    signer = RuntimeModelLeaseSigner("runtime-model-lease-test-secret")
    with pytest.raises(RuntimeModelLeaseError, match="LEASE_INVALID"):
        signer.sign(replace(_claims(), expires_at_ms=1_000))
    with pytest.raises(RuntimeModelLeaseError, match="LEASE_INVALID"):
        signer.sign(replace(_claims(), nonce_sha256="not-a-digest"))
