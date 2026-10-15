"""Behavioural tests importing ``ai_gateway_contracts`` directly.

Proves the contracts package is self-sufficient (no ``ai_gateway_core``
import needed) and that the migrated wire formats are unchanged: the same
schema versions, canonical payloads and signature round-trips as before the
ARC-04 move.
"""

from __future__ import annotations

import json
import time
from dataclasses import replace

from ai_gateway_contracts.agent_launch import RESOLVED_AGENT_LAUNCH_SCHEMA_VERSION
from ai_gateway_contracts.agent_runtime import (
    AGENT_RUNTIME_ENVELOPE_SCHEMA_VERSION,
    AGENT_RUNTIME_SCHEMA_VERSION,
    AgentRuntimeSigner,
    runtime_sha256,
)
from ai_gateway_contracts.agent_runtime_lease import (
    RUNTIME_MODEL_LEASE_SCHEMA_VERSION,
    RuntimeModelLeaseClaims,
    RuntimeModelLeaseError,
    RuntimeModelLeaseSigner,
)
from ai_gateway_contracts.capability_proof import (
    SCHEMA_VERSION as CAPABILITY_PROOF_SCHEMA_VERSION,
)
from ai_gateway_contracts.capability_proof import (
    CapabilityProofError,
    sign_capability_proof,
    verify_capability_proof,
)
from ai_gateway_contracts.event_envelope import (
    EventEnvelope,
    UsageRecordedV1,
    parse_envelope,
)
from ai_gateway_contracts.event_errors import EventDeserializationError

SECRET = "contract-self-test-secret-0123456789"


def test_schema_version_constants_unchanged() -> None:
    assert RESOLVED_AGENT_LAUNCH_SCHEMA_VERSION == "resolved-agent-launch/v1"
    assert CAPABILITY_PROOF_SCHEMA_VERSION == "ai-platform-capability-proof/v1"
    assert AGENT_RUNTIME_SCHEMA_VERSION == "agent-runtime/v1"
    assert AGENT_RUNTIME_ENVELOPE_SCHEMA_VERSION == "agent-runtime-envelope/v1"
    assert RUNTIME_MODEL_LEASE_SCHEMA_VERSION == "agent-runtime-model-lease/v1"
    assert UsageRecordedV1.EVENT_TYPE == "usage.recorded.v1"


def test_capability_proof_round_trip_via_contracts_only() -> None:
    body = {"dataset_id": "kb-1", "scope": "read"}
    header = sign_capability_proof(
        SECRET,
        method="POST",
        path="/internal/v1/capabilities/kb/read",
        body=body,
        tenant_id="t1",
        user_id="u1",
        session_id="s1",
        execution_id="e1",
        run_id="r1",
        nonce="nonce-1",
    )
    proof = verify_capability_proof(
        SECRET,
        header,
        method="POST",
        path="/internal/v1/capabilities/kb/read",
        body=body,
        tenant_id="t1",
        user_id="u1",
        session_id="s1",
        execution_id="e1",
        run_id="r1",
    )
    assert proof.tenant_id == "t1"
    assert proof.run_id == "r1"


def test_capability_proof_tamper_rejected() -> None:
    body = {"dataset_id": "kb-1"}
    header = sign_capability_proof(
        SECRET,
        method="GET",
        path="/internal/v1/x",
        body=body,
        tenant_id="t1",
        user_id="u1",
        session_id="s1",
        execution_id="e1",
        run_id="r1",
        nonce="nonce-2",
    )
    try:
        verify_capability_proof(
            SECRET,
            header,
            method="GET",
            path="/internal/v1/x",
            body=body,
            tenant_id="t1",
            user_id="u1",
            session_id="s1",
            execution_id="e1",
            run_id="other-run",
        )
    except CapabilityProofError:
        return
    raise AssertionError("binding mismatch must raise CapabilityProofError")


def _lease_claims() -> RuntimeModelLeaseClaims:
    now_ms = int(time.time() * 1000)
    return RuntimeModelLeaseClaims(
        schema_version=RUNTIME_MODEL_LEASE_SCHEMA_VERSION,
        lease_id="019d0000-0000-7000-8000-000000000001",
        snapshot_id="019d0000-0000-7000-8000-000000000002",
        run_id="019d0000-0000-7000-8000-000000000003",
        runtime_thread_id="019d0000-0000-7000-8000-000000000004",
        tenant_id="t1",
        user_id="u1",
        session_id="s1",
        provider_id="provider-a",
        model_id="model-a",
        capability_revision=3,
        issued_at_ms=now_ms,
        expires_at_ms=now_ms + 30_000,
        nonce_sha256="a" * 64,
    )


def test_runtime_lease_sign_verify_round_trip() -> None:
    signer = RuntimeModelLeaseSigner(SECRET)
    claims = _lease_claims()
    signature = signer.sign(claims)
    assert signature.startswith("v1:")
    signer.verify(signature, claims)


def test_runtime_lease_tamper_rejected() -> None:
    signer = RuntimeModelLeaseSigner(SECRET)
    claims = _lease_claims()
    signature = signer.sign(claims)
    tampered = replace(claims, model_id="model-b")
    try:
        signer.verify(signature, tampered)
    except RuntimeModelLeaseError as exc:
        assert exc.code == "RUNTIME_MODEL_LEASE_SIGNATURE_INVALID"
        return
    raise AssertionError("tampered lease must raise RuntimeModelLeaseError")


def _snapshot() -> dict:
    return {
        "schema_version": AGENT_RUNTIME_SCHEMA_VERSION,
        "tenant_id": "t1",
        "agent_id": "agent-1",
        "agent_version_id": "av-1",
        "publication": {"id": "pub-1", "channel": "hosted", "auth_mode": "private"},
        "model": {"id": "model-a", "provider": "provider-a", "parameters": {}},
        "instructions": {"agent": "be helpful", "prompt_hash": "sha256:abc"},
        "capabilities": [
            {
                "type": "platform",
                "id": "cap-1",
                "version": "1.0.0",
                "schema_hash": "sha256:def",
                "risk": "low",
                "config": {},
            }
        ],
        "knowledge": {"datasets": ["kb-1"], "retrieval": {}},
        "memory": {"mode": "off"},
        "channel_policy": {
            "attachments": False,
            "high_risk_tools": False,
            "allowed_origins": [],
        },
        "fingerprints": {
            "spec": "sha256:spec",
            "tool_schema": "sha256:tools",
            "skills": "sha256:skills",
            "knowledge_revision": "sha256:kb",
        },
    }


def test_agent_runtime_envelope_round_trip_via_contracts_only() -> None:
    signer = AgentRuntimeSigner(secret=SECRET)
    snapshot = _snapshot()
    request_body = {"messages": [{"role": "user", "content": "hi"}]}
    envelope = signer.sign(
        tenant_id="t1",
        caller_principal="user:u1",
        agent_id="agent-1",
        agent_version_id="av-1",
        draft_revision=None,
        publication_id="pub-1",
        channel="hosted",
        session_id="s1",
        resolved_snapshot=snapshot,
        request_body=request_body,
        spec_hash="sha256:spec",
    )
    verified = signer.verify(
        envelope,
        request_body=request_body,
        expected_tenant_id="t1",
        expected_caller_principal="user:u1",
        expected_session_id="s1",
    )
    assert verified.agent_id == "agent-1"
    assert verified.capability_ids == frozenset({"cap-1"})
    assert verified.runtime_fingerprint == runtime_sha256(snapshot)


def test_event_envelope_round_trip_via_contracts_only() -> None:
    payload = UsageRecordedV1(
        tenant_id="t1",
        user_id="u1",
        model="model-a",
        provider="provider-a",
        input_tokens=10,
        output_tokens=5,
        timestamp=1234.5,
    )
    envelope = EventEnvelope[UsageRecordedV1](
        event_type=UsageRecordedV1.EVENT_TYPE,
        producer="ai-gateway",
        tenant_id="t1",
        request_id="req-1",
        payload=payload,
    )
    parsed = parse_envelope(envelope.model_dump_json())
    assert parsed.payload.input_tokens == 10
    assert parsed.event_id == envelope.event_id


def test_event_envelope_unknown_type_is_poison() -> None:
    raw = json.dumps(
        {
            "event_type": "no.such.event.v1",
            "producer": "ai-gateway",
            "tenant_id": "t1",
            "request_id": "req-1",
            "payload": {},
        }
    )
    try:
        parse_envelope(raw)
    except EventDeserializationError:
        return
    raise AssertionError("unknown event_type must raise EventDeserializationError")
