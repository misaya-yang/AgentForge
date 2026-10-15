from __future__ import annotations

from dataclasses import replace

import pytest

from src.services.agent_runtime.capability_leases import (
    CapabilityEffect,
    CapabilityLeaseIssuer,
    LeaseError,
    canonical_json_hash,
)

SECRET = b"s" * 32


def _issue(**kwargs):
    values = {
        "tenant_id": "tenant-a",
        "user_id": "user-a",
        "session_id": "session-a",
        "run_id": "run-a",
        "tool_call_id": "call-a",
        "attempt_id": "attempt-a",
        "capability_id": "fixture.echo",
        "capability_revision": 3,
        "arguments": {"b": 2, "a": {"z": True, "y": [1, 2]}},
        "effect": CapabilityEffect.READ,
        "now_epoch_ms": 1_000,
    }
    values.update(kwargs)
    return CapabilityLeaseIssuer(SECRET, ttl_ms=1_000).issue(**values)


def test_python_hash_matches_rust_sorted_key_contract() -> None:
    expected = "sha256:f8e6c1629cb9bb68de10d9a1b6e5f5055e80710862e225568c7547f1ea540af8"
    assert canonical_json_hash({"b": 2, "a": {"z": True, "y": [1, 2]}}) == expected
    assert canonical_json_hash({"a": {"y": [1, 2], "z": True}, "b": 2}) == expected


def test_signature_matches_the_rust_cross_language_fixture() -> None:
    lease = _issue(lease_id="lease-a", nonce="nonce-with-16bytes")
    assert lease.signature == (
        "sha256:8da76c35a714b9d32464df841017ce5523fd2f19ec15bb440542e406cc5dd68d"
    )


def test_signature_verifies_and_detects_scope_or_argument_tampering() -> None:
    issuer = CapabilityLeaseIssuer(SECRET, ttl_ms=1_000)
    lease = _issue()
    issuer.verify(lease, now_epoch_ms=1_001)
    with pytest.raises(LeaseError, match="signature"):
        issuer.verify(replace(lease, tenant_id="tenant-b"), now_epoch_ms=1_001)
    with pytest.raises(LeaseError, match="signature"):
        issuer.verify(replace(lease, arguments_hash="sha256:" + "0" * 64), now_epoch_ms=1_001)


def test_write_and_unknown_require_approval_but_read_forbids_it() -> None:
    with pytest.raises(LeaseError, match="approval"):
        _issue(effect=CapabilityEffect.READ, approval_id="approval-a")
    with pytest.raises(LeaseError, match="approval"):
        _issue(effect=CapabilityEffect.WRITE)
    with pytest.raises(LeaseError, match="approval"):
        _issue(effect=CapabilityEffect.UNKNOWN)
    assert (
        _issue(effect=CapabilityEffect.WRITE, approval_id="approval-a").effect
        == CapabilityEffect.WRITE
    )


def test_secret_and_ttl_are_bounded() -> None:
    with pytest.raises(LeaseError, match="secret"):
        CapabilityLeaseIssuer(b"short")
    with pytest.raises(LeaseError, match="ttl"):
        CapabilityLeaseIssuer(SECRET, ttl_ms=120_001)
