from __future__ import annotations

from dataclasses import replace

import pytest

from local_node.errors import ApprovalRequired, CapabilityDenied, StaleTargetError
from local_node.files import LocalFileService, sha256_bytes
from local_node.grants import DirectoryGrantStore
from local_node.ledger import ActionLedger
from local_node.models import ApprovalProof
from local_node.watcher import DirectoryWatcher


def _services(tmp_path, platform_signature_verifier, trusted_local_approval_verifier):
    root = tmp_path / "workspace"
    root.mkdir()
    grants = DirectoryGrantStore()
    grant = grants.issue(
        root,
        frozenset({"list", "read", "search", "watch", "write", "rollback"}),
        tenant_id="tenant-a",
        user_id="user-a",
    )
    ledger = ActionLedger(
        tmp_path / "state" / "ledger.sqlite",
        platform_signature_verifier=platform_signature_verifier,
        trusted_local_approval_verifier=trusted_local_approval_verifier,
    )
    return root, grant, ledger, LocalFileService(grants, tmp_path / "state" / "rollback", ledger)


def test_atomic_write_is_idempotent_and_rollback_restores_bytes(
    tmp_path, action_factory, platform_signature_verifier, trusted_local_approval_verifier
):
    root, grant, ledger, files = _services(
        tmp_path, platform_signature_verifier, trusted_local_approval_verifier
    )
    target = root / "document.txt"
    target.write_bytes(b"before")
    before = sha256_bytes(b"before")
    args = {
        "grant_id": grant.grant_id,
        "relative_path": "document.txt",
        "content_sha256": sha256_bytes(b"after"),
        "expected_hash": before,
    }
    action = action_factory("file.write", args, before)
    receipt = files.write_atomic(grant.grant_id, "document.txt", b"after", before, action)
    assert target.read_bytes() == b"after"
    assert receipt.before_sha256 == before
    assert receipt.after_sha256 == sha256_bytes(b"after")
    replay = files.write_atomic(grant.grant_id, "document.txt", b"after", before, action)
    assert replay == receipt

    current = receipt.after_sha256
    rollback_args = {
        "rollback_ref": receipt.rollback_ref,
        "grant_id": grant.grant_id,
        "relative_path": "document.txt",
        "expected_current_hash": current,
    }
    rollback_action = action_factory(
        "file.write",
        rollback_args,
        current,
        action_id="action-rollback",
        idempotency_key="idem-rollback",
        capability_lease_id=grant.grant_id,
        resource_refs=(receipt.rollback_ref, grant.grant_id, "document.txt"),
        tool_name="local_file_rollback",
        operation="file.rollback",
    )
    restored = files.rollback(receipt.rollback_ref, rollback_action)
    assert target.read_bytes() == b"before"
    assert restored.after_sha256 == before
    assert ledger.verify_integrity()


def test_stale_target_cannot_use_old_approval(
    tmp_path, action_factory, platform_signature_verifier, trusted_local_approval_verifier
):
    root, grant, ledger, files = _services(
        tmp_path, platform_signature_verifier, trusted_local_approval_verifier
    )
    target = root / "document.txt"
    target.write_bytes(b"approved version")
    approved_hash = sha256_bytes(target.read_bytes())
    args = {
        "grant_id": grant.grant_id,
        "relative_path": "document.txt",
        "content_sha256": sha256_bytes(b"agent version"),
        "expected_hash": approved_hash,
    }
    action = action_factory("file.write", args, approved_hash)
    target.write_bytes(b"human edit")
    with pytest.raises(StaleTargetError):
        files.write_atomic(grant.grant_id, "document.txt", b"agent version", approved_hash, action)
    assert target.read_bytes() == b"human edit"
    assert ledger.get(action.action_id).status.value == "failed"


def test_signed_envelope_and_approval_are_enforced_at_file_side_effect_boundary(
    tmp_path, action_factory, platform_signature_verifier, trusted_local_approval_verifier
):
    root, grant, ledger, files = _services(
        tmp_path, platform_signature_verifier, trusted_local_approval_verifier
    )
    target = root / "document.txt"
    target.write_bytes(b"before")
    before = sha256_bytes(b"before")
    args = {
        "grant_id": grant.grant_id,
        "relative_path": "document.txt",
        "content_sha256": sha256_bytes(b"after"),
        "expected_hash": before,
    }
    signed_without_approval = action_factory("file.write", args, before, approved=False)
    forged_approval = ApprovalProof(
        "forged-approval",
        signed_without_approval.action_id,
        signed_without_approval.device_id,
        signed_without_approval.arguments_digest,
        before,
        signed_without_approval.policy_snapshot_digest,
        "forged-local-nonce",
        signed_without_approval.expires_at,
        "forged-local-signature",
    )
    forged = replace(signed_without_approval, approval=forged_approval)

    with pytest.raises(CapabilityDenied, match="signature verification failed"):
        files.write_atomic(grant.grant_id, "document.txt", b"after", before, forged)
    assert target.read_bytes() == b"before"
    assert ledger.get(forged.action_id) is None

    no_verifier_ledger = ActionLedger(tmp_path / "state" / "no-verifier.sqlite")
    no_verifier_files = LocalFileService(
        files.grants,
        tmp_path / "state" / "no-verifier-rollbacks",
        no_verifier_ledger,
    )
    signed = action_factory(
        "file.write",
        args,
        before,
        action_id="action-no-verifier",
        idempotency_key="idem-no-verifier",
    )
    try:
        with pytest.raises(CapabilityDenied, match="verifier is unavailable"):
            no_verifier_files.write_atomic(grant.grant_id, "document.txt", b"after", before, signed)
        assert target.read_bytes() == b"before"
        assert no_verifier_ledger.get(signed.action_id) is None
    finally:
        no_verifier_ledger.close()

    no_local_approval_ledger = ActionLedger(
        tmp_path / "state" / "no-local-approval.sqlite",
        platform_signature_verifier=platform_signature_verifier,
    )
    no_local_approval_files = LocalFileService(
        files.grants,
        tmp_path / "state" / "no-local-approval-rollbacks",
        no_local_approval_ledger,
    )
    locally_approved = action_factory(
        "file.write",
        args,
        before,
        action_id="action-no-local-approval-verifier",
        idempotency_key="idem-no-local-approval-verifier",
    )
    try:
        with pytest.raises(ApprovalRequired, match="trusted local approval verifier"):
            no_local_approval_files.write_atomic(
                grant.grant_id,
                "document.txt",
                b"after",
                before,
                locally_approved,
            )
        assert target.read_bytes() == b"before"
        assert no_local_approval_ledger.get(locally_approved.action_id).status.value == "failed"
    finally:
        no_local_approval_ledger.close()


def test_create_and_watcher_report_hash_but_not_content(
    tmp_path, action_factory, platform_signature_verifier, trusted_local_approval_verifier
):
    root, grant, _, files = _services(
        tmp_path, platform_signature_verifier, trusted_local_approval_verifier
    )
    watcher = DirectoryWatcher(files, grant.grant_id)
    assert watcher.scan_once() == ()
    target = root / "new.txt"
    target.write_text("private body", encoding="utf-8")
    created = watcher.scan_once()
    assert len(created) == 1
    assert created[0].kind == "create"
    assert created[0].sha256 == sha256_bytes(b"private body")
    assert "private body" not in repr(created[0])
    target.write_text("changed", encoding="utf-8")
    assert watcher.scan_once()[0].kind == "modify"
    target.rename(root / "renamed.txt")
    renamed = watcher.scan_once()[0]
    assert renamed.kind == "rename"
    assert renamed.previous_path == "new.txt"
    (root / "renamed.txt").unlink()
    assert watcher.scan_once()[0].kind == "delete"


def test_watcher_revocation_is_effective_on_the_next_poll(
    tmp_path, platform_signature_verifier, trusted_local_approval_verifier
):
    root, grant, _, files = _services(
        tmp_path, platform_signature_verifier, trusted_local_approval_verifier
    )
    watcher = DirectoryWatcher(files, grant.grant_id)
    assert watcher.scan_once() == ()
    files.grants.revoke(grant.grant_id)
    (root / "after-revoke.txt").write_text("must not be observed", encoding="utf-8")

    with pytest.raises(CapabilityDenied, match="unavailable"):
        watcher.scan_once()
