"""Action-envelope and approval-binding acceptance tests (OS-A13/A19/A25)."""

from __future__ import annotations

import dataclasses

import pytest
from local_node.errors import ApprovalRequired, CapabilityDenied, StaleTargetError
from local_node.models import ApprovalProof, digest_payload


def _approval_for(
    action,
    signer,
    *,
    expires_at: float | None = None,
    action_id: str | None = None,
    device_id: str | None = None,
    nonce: str | None = None,
) -> ApprovalProof:
    unsigned = ApprovalProof(
        approval_id="approval-local-001",
        action_id=action.action_id if action_id is None else action_id,
        device_id=action.device_id if device_id is None else device_id,
        arguments_digest=action.arguments_digest,
        target_snapshot_digest=action.target_snapshot_digest,
        policy_snapshot_digest=action.policy_snapshot_digest,
        nonce=f"local-approval-{action.action_id}" if nonce is None else nonce,
        expires_at=action.expires_at if expires_at is None else expires_at,
        local_signature="",
    )
    return dataclasses.replace(
        unsigned,
        local_signature=signer.sign(unsigned.canonical_local_payload()),
    )


def test_normalized_argument_digest_is_order_independent(local_action_factory) -> None:
    first = local_action_factory(
        normalized_arguments={"path": "notes/result.txt", "patch": {"b": 2, "a": 1}}
    )
    second = local_action_factory(
        normalized_arguments={"patch": {"a": 1, "b": 2}, "path": "notes/result.txt"}
    )

    assert first.arguments_digest == second.arguments_digest


def test_tampered_arguments_are_rejected_before_dispatch(
    local_action_factory, platform_signature_verifier
) -> None:
    action = local_action_factory()

    with pytest.raises(CapabilityDenied, match="arguments"):
        action.validate_payload(
            {"path": "../outside.txt", "content_sha256": "attacker-content"},
            verifier=platform_signature_verifier,
        )


def test_missing_platform_verifier_fails_closed(local_action_factory) -> None:
    action = local_action_factory()

    with pytest.raises(CapabilityDenied, match="verifier is unavailable"):
        action.verify_platform_signature(None)


@pytest.mark.parametrize(
    ("field", "replacement"),
    [
        ("action_id", "action-b"),
        ("idempotency_key", "idem-b"),
        ("tenant_id", "tenant-b"),
        ("user_id", "user-b"),
        ("device_id", "device-b"),
        ("session_id", "session-b"),
        ("run_id", "run-b"),
        ("agent_id", "agent-b"),
        ("agent_version", "agent-version-b"),
        ("call_id", "call-b"),
        ("envelope_version", 2),
        ("capability", "file.rollback"),
        ("tool_name", "local_file_rollback"),
        ("operation", "file.rollback"),
        ("capability_lease_id", "lease-b"),
        ("resource_refs", ("resource-b",)),
        ("arguments_digest", "changed-arguments"),
        ("target_snapshot_digest", "changed-target"),
        ("policy_snapshot_digest", "changed-policy"),
        ("nonce", "changed-nonce"),
        ("issued_at", 1.0),
        ("expires_at", 4_102_444_800.0),
        ("platform_key_id", "platform-key-b"),
    ],
)
def test_platform_signature_binds_every_authority_field(
    local_action_factory,
    platform_signature_verifier,
    field: str,
    replacement: object,
) -> None:
    action = local_action_factory()
    tampered = dataclasses.replace(action, **{field: replacement})

    with pytest.raises(
        CapabilityDenied, match="signature verification failed|version is unsupported"
    ):
        tampered.verify_platform_signature(platform_signature_verifier)


def test_untrusted_approval_cannot_expand_signed_envelope(
    local_action_factory, platform_signature_verifier, trusted_local_approval
) -> None:
    _signer, local_verifier = trusted_local_approval
    base = local_action_factory()
    forged_receipt = ApprovalProof(
        "approval-forged-by-platform",
        base.action_id,
        base.device_id,
        base.arguments_digest,
        base.target_snapshot_digest,
        base.policy_snapshot_digest,
        "forged-platform-nonce",
        base.expires_at,
        "forged-local-signature",
    )
    forged = dataclasses.replace(base, approval=forged_receipt, platform_signature="")
    forged = dataclasses.replace(
        forged,
        platform_signature=platform_signature_verifier.sign(forged.canonical_signed_payload()),
    )

    forged.verify_platform_signature(platform_signature_verifier)
    with pytest.raises(ApprovalRequired, match="trusted local approval verification failed"):
        forged.require_approval(
            target_snapshot_digest=forged.target_snapshot_digest,
            verifier=local_verifier,
        )

    approved = local_action_factory(approved=True)
    approved.verify_platform_signature(platform_signature_verifier)
    approved.require_approval(
        target_snapshot_digest=approved.target_snapshot_digest,
        verifier=local_verifier,
    )
    with pytest.raises(ApprovalRequired, match="verification failed"):
        approved.require_approval(
            target_snapshot_digest=approved.target_snapshot_digest,
            verifier=local_verifier,
        )


@pytest.mark.parametrize(
    ("field", "replacement", "expected_error"),
    [
        ("arguments_digest", "tampered-arguments", ApprovalRequired),
        ("policy_snapshot_digest", "weaker-policy", ApprovalRequired),
        ("target_snapshot_digest", "stale-target", StaleTargetError),
    ],
)
def test_approval_is_bound_to_exact_intent(
    local_action_factory,
    field: str,
    replacement: str,
    expected_error: type[Exception],
    trusted_local_approval,
) -> None:
    signer, local_verifier = trusted_local_approval
    base = local_action_factory()
    proof = _approval_for(base, signer)
    action = dataclasses.replace(base, approval=proof, **{field: replacement})

    with pytest.raises(expected_error):
        action.require_approval(
            target_snapshot_digest=action.target_snapshot_digest,
            verifier=local_verifier,
        )


@pytest.mark.parametrize(
    ("field", "replacement", "expected_error"),
    [
        ("action_id", "another-action", ApprovalRequired),
        ("device_id", "another-device", ApprovalRequired),
        ("arguments_digest", "changed-arguments", ApprovalRequired),
        ("target_snapshot_digest", "changed-target", StaleTargetError),
        ("policy_snapshot_digest", "changed-policy", ApprovalRequired),
    ],
)
def test_locally_signed_receipt_rejects_exact_binding_mutation(
    local_action_factory,
    platform_signature_verifier,
    trusted_local_approval,
    field: str,
    replacement: str,
    expected_error: type[Exception],
) -> None:
    """A platform-valid envelope cannot repurpose a local device receipt."""

    signer, local_verifier = trusted_local_approval
    base = local_action_factory()
    proof = _approval_for(base, signer)
    mutated_proof = dataclasses.replace(proof, **{field: replacement})
    action = dataclasses.replace(base, approval=mutated_proof, platform_signature="")
    action = dataclasses.replace(
        action,
        platform_signature=platform_signature_verifier.sign(
            action.canonical_signed_payload()
        ),
    )

    action.verify_platform_signature(platform_signature_verifier)
    with pytest.raises(expected_error):
        action.require_approval(
            target_snapshot_digest=action.target_snapshot_digest,
            verifier=local_verifier,
        )


def test_target_changed_after_approval_is_stale(
    local_action_factory, trusted_local_approval
) -> None:
    signer, local_verifier = trusted_local_approval
    base = local_action_factory()
    action = dataclasses.replace(base, approval=_approval_for(base, signer))

    with pytest.raises(StaleTargetError):
        action.require_approval(
            target_snapshot_digest="target-after-human-edit",
            verifier=local_verifier,
        )


def test_missing_or_expired_local_approval_fails_closed(
    local_action_factory, trusted_local_approval
) -> None:
    signer, local_verifier = trusted_local_approval
    missing = local_action_factory()
    with pytest.raises(ApprovalRequired):
        missing.require_approval(
            target_snapshot_digest=missing.target_snapshot_digest,
            verifier=local_verifier,
        )

    expired_base = local_action_factory()
    expired = dataclasses.replace(
        expired_base,
        approval=_approval_for(expired_base, signer, expires_at=0.0),
    )
    with pytest.raises(ApprovalRequired, match="expired"):
        expired.require_approval(
            target_snapshot_digest=expired.target_snapshot_digest,
            verifier=local_verifier,
        )

    non_finite_base = local_action_factory()
    non_finite = local_action_factory(
        approval=_approval_for(non_finite_base, signer, expires_at=float("nan"))
    )
    with pytest.raises(ApprovalRequired, match="lifetime is invalid"):
        non_finite.require_approval(
            target_snapshot_digest=non_finite.target_snapshot_digest,
            verifier=local_verifier,
        )


def test_non_finite_action_lifetime_is_rejected_before_verification(
    local_action_factory, platform_signature_verifier
) -> None:
    action = local_action_factory(ttl_seconds=float("nan"))

    with pytest.raises(CapabilityDenied, match="lifetime is invalid"):
        action.verify_platform_signature(platform_signature_verifier)


def test_identity_fields_are_part_of_the_immutable_action_context(local_action_factory) -> None:
    action = local_action_factory()
    identity = (
        action.tenant_id,
        action.user_id,
        action.device_id,
        action.session_id,
        action.run_id,
        action.agent_id,
        action.agent_version,
        action.call_id,
        action.envelope_version,
        action.capability_lease_id,
        action.nonce,
    )

    assert identity == (
        "tenant-a",
        "user-a",
        "device-a",
        "session-a",
        "run-a",
        "agent-a",
        "agent-version-a",
        "call-a",
        1,
        "lease-a",
        "nonce-action-001",
    )
    with pytest.raises(dataclasses.FrozenInstanceError):
        action.tenant_id = "tenant-b"  # type: ignore[misc]


def test_digest_never_embeds_secret_argument_bytes() -> None:
    canary = "sk-local-os-canary-never-persist"

    digest = digest_payload({"token": canary, "path": "notes.txt"})

    assert len(digest) == 64
    assert canary not in digest
