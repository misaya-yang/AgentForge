"""Durable action receipt, replay, and integrity tests (OS-A17/A23/A26)."""

from __future__ import annotations

import dataclasses
import hashlib
import sqlite3
from pathlib import Path

import pytest
from local_node.errors import CapabilityDenied, IdempotencyConflict, LedgerIntegrityError
from local_node.ledger import ActionLedger
from local_node.models import ActionStatus


@pytest.fixture
def ledger(
    tmp_path: Path,
    platform_signature_verifier,
    trusted_local_approval_verifier,
):
    value = ActionLedger(
        tmp_path / "state" / "actions.sqlite3",
        platform_signature_verifier=platform_signature_verifier,
        trusted_local_approval_verifier=trusted_local_approval_verifier,
    )
    try:
        yield value
    finally:
        value.close()


def test_dispatch_is_durable_before_executor_is_allowed_to_run(
    ledger: ActionLedger,
    local_action_factory,
) -> None:
    action = local_action_factory()

    begin = ledger.begin(action)

    assert begin.created is True
    assert begin.record.status is ActionStatus.POLICY_CHECK
    assert ledger.get(action.action_id) == begin.record
    assert ledger.verify_integrity()


def test_ledger_default_rejects_claim_when_platform_verifier_is_missing(
    tmp_path: Path,
    local_action_factory,
) -> None:
    untrusted = ActionLedger(tmp_path / "no-verifier.sqlite3")
    action = local_action_factory()
    try:
        with pytest.raises(CapabilityDenied, match="verifier is unavailable"):
            untrusted.begin(action)
        assert untrusted.get(action.action_id) is None
    finally:
        untrusted.close()


def test_state_machine_rejects_skipping_policy_and_dispatch(
    ledger: ActionLedger,
    local_action_factory,
) -> None:
    action = local_action_factory()
    ledger.begin(action)

    with pytest.raises(IdempotencyConflict, match="policy_check -> running"):
        ledger.mark_running(action.action_id)

    ledger.mark_awaiting_approval(action.action_id)
    assert ledger.get(action.action_id).status is ActionStatus.AWAITING_APPROVAL
    ledger.mark_dispatched(action.action_id)
    ledger.mark_running(action.action_id)
    ledger.mark_observed(action.action_id)
    assert ledger.get(action.action_id).status is ActionStatus.OBSERVED


def test_disconnect_before_dispatch_is_interrupted_not_unknown(
    ledger: ActionLedger,
    local_action_factory,
) -> None:
    action = local_action_factory()
    ledger.begin(action)
    ledger.mark_awaiting_approval(action.action_id)

    assert ledger.interrupt_running() == (action.action_id,)
    record = ledger.get(action.action_id)
    assert record is not None
    assert record.status is ActionStatus.INTERRUPTED
    assert record.result["side_effect_unknown"] is False


def test_same_idempotency_key_returns_existing_receipt_without_second_dispatch(
    ledger: ActionLedger,
    local_action_factory,
) -> None:
    action = local_action_factory()
    first = ledger.begin(action)
    second = ledger.begin(action)

    assert first.created is True
    assert second.created is False
    assert second.record == first.record
    assert ledger.get_by_idempotency_key(action.idempotency_key) == first.record
    dispatches = [entry for entry in ledger.entries() if entry["event_type"] == "policy_check"]
    assert len(dispatches) == 1


def test_idempotency_key_reuse_with_changed_intent_is_rejected(
    ledger: ActionLedger,
    local_action_factory,
) -> None:
    original = local_action_factory()
    tampered = local_action_factory(
        action_id="action-002",
        normalized_arguments={"path": "notes/other.txt", "content_sha256": "changed"},
    )
    ledger.begin(original)

    with pytest.raises(IdempotencyConflict):
        ledger.begin(tampered)


@pytest.mark.parametrize(
    ("field", "replacement"),
    [
        ("tenant_id", "tenant-b"),
        ("user_id", "user-b"),
        ("device_id", "device-b"),
        ("session_id", "session-b"),
        ("run_id", "run-b"),
        ("agent_id", "agent-b"),
        ("agent_version", "agent-version-b"),
        ("call_id", "call-b"),
        ("capability", "file.rollback"),
        ("capability_lease_id", "lease-b"),
        ("resource_refs", ("resource-b",)),
        ("target_snapshot_digest", "target-b"),
        ("policy_snapshot_digest", "policy-b"),
        ("nonce", "nonce-b"),
    ],
)
def test_same_arguments_and_idempotency_cannot_replay_across_signed_scope(
    ledger: ActionLedger,
    local_action_factory,
    platform_signature_verifier,
    field: str,
    replacement: object,
) -> None:
    original = local_action_factory()
    ledger.begin(original)
    changed = dataclasses.replace(original, **{field: replacement}, platform_signature="")
    changed = dataclasses.replace(
        changed,
        platform_signature=platform_signature_verifier.sign(changed.canonical_signed_payload()),
    )

    with pytest.raises(IdempotencyConflict, match="reused with new intent"):
        ledger.begin(changed)


def test_action_record_binds_replay_to_full_signed_envelope_digest(
    ledger: ActionLedger,
    local_action_factory,
) -> None:
    action = local_action_factory()

    record = ledger.begin(action).record

    assert record.envelope_digest == hashlib.sha256(action.canonical_signed_payload()).hexdigest()


def test_action_can_have_exactly_one_terminal_state(
    ledger: ActionLedger,
    local_action_factory,
) -> None:
    action = local_action_factory()
    ledger.begin(action)
    ledger.mark_awaiting_approval(action.action_id)
    ledger.mark_dispatched(action.action_id)
    ledger.mark_running(action.action_id)
    ledger.mark_observed(action.action_id)
    success = ledger.finish(
        action.action_id,
        ActionStatus.SUCCEEDED,
        {"after_digest": "after-v1"},
    )

    # An identical acknowledgement is idempotent, but a different terminal
    # state or result must never create a second truth.
    assert (
        ledger.finish(
            action.action_id,
            ActionStatus.SUCCEEDED,
            {"after_digest": "after-v1"},
        )
        == success
    )
    with pytest.raises(IdempotencyConflict):
        ledger.finish(action.action_id, ActionStatus.FAILED, {"error_code": "late_error"})
    with pytest.raises(IdempotencyConflict):
        ledger.finish(
            action.action_id,
            ActionStatus.SUCCEEDED,
            {"after_digest": "different-result"},
        )


def test_disconnect_marks_unknown_and_reconnect_does_not_replay(
    ledger: ActionLedger,
    local_action_factory,
) -> None:
    action = local_action_factory()
    ledger.begin(action)
    ledger.mark_dispatched(action.action_id)
    ledger.mark_running(action.action_id)

    assert ledger.interrupt_running() == (action.action_id,)
    record = ledger.get(action.action_id)
    assert record is not None
    assert record.status is ActionStatus.UNKNOWN
    assert record.result == {
        "error_code": "transport_disconnected",
        "replay_allowed": False,
        "side_effect_unknown": True,
    }

    replay = ledger.begin(action)
    assert replay.created is False
    assert replay.record.status is ActionStatus.UNKNOWN
    assert len([entry for entry in ledger.entries() if entry["event_type"] == "policy_check"]) == 1


@pytest.mark.parametrize("tamper", ["modify", "delete", "reorder"])
def test_event_chain_detects_tampering(
    tmp_path: Path,
    local_action_factory,
    platform_signature_verifier,
    tamper: str,
) -> None:
    path = tmp_path / f"ledger-{tamper}.sqlite3"
    ledger = ActionLedger(path, platform_signature_verifier=platform_signature_verifier)
    action = local_action_factory()
    ledger.begin(action)
    ledger.mark_dispatched(action.action_id)
    ledger.mark_running(action.action_id)
    ledger.finish(action.action_id, ActionStatus.SUCCEEDED, {"after_digest": "after-v1"})
    ledger.close()

    database = sqlite3.connect(path)
    try:
        if tamper == "modify":
            database.execute(
                "UPDATE events SET payload_json=? WHERE seq=(SELECT MIN(seq) FROM events)",
                ('{"attacker":"rewrote-history"}',),
            )
        elif tamper == "delete":
            database.execute("DELETE FROM events WHERE seq=(SELECT MIN(seq) + 1 FROM events)")
        else:
            rows = database.execute(
                "SELECT seq, created_at FROM events ORDER BY seq LIMIT 2"
            ).fetchall()
            assert len(rows) == 2
            database.execute("UPDATE events SET created_at=? WHERE seq=?", (rows[1][1], rows[0][0]))
            database.execute("UPDATE events SET created_at=? WHERE seq=?", (rows[0][1], rows[1][0]))
        database.commit()
    finally:
        database.close()

    reopened = ActionLedger(path)
    try:
        with pytest.raises(LedgerIntegrityError):
            reopened.verify_integrity()
    finally:
        reopened.close()


def test_legacy_unbound_ledger_row_cannot_replay_as_a_signed_action(
    tmp_path: Path,
    local_action_factory,
    platform_signature_verifier,
) -> None:
    path = tmp_path / "legacy.sqlite3"
    database = sqlite3.connect(path)
    try:
        database.execute(
            "CREATE TABLE actions ("
            "action_id TEXT PRIMARY KEY, idempotency_key TEXT UNIQUE NOT NULL, "
            "arguments_digest TEXT NOT NULL, status TEXT NOT NULL, result_json TEXT, "
            "created_at REAL NOT NULL, updated_at REAL NOT NULL)"
        )
        action = local_action_factory()
        database.execute(
            "INSERT INTO actions VALUES(?,?,?,?,?,?,?)",
            (
                action.action_id,
                action.idempotency_key,
                action.arguments_digest,
                ActionStatus.SUCCEEDED.value,
                '{"old":true}',
                1.0,
                1.0,
            ),
        )
        database.commit()
    finally:
        database.close()

    ledger = ActionLedger(path, platform_signature_verifier=platform_signature_verifier)
    try:
        with pytest.raises(IdempotencyConflict, match="reused with new intent"):
            ledger.begin(action)
        persisted = ledger.get(action.action_id)
        assert persisted is not None
        assert persisted.envelope_digest == "legacy-unbound"
    finally:
        ledger.close()


def test_secret_canary_is_redacted_before_persistence(
    ledger: ActionLedger,
    local_action_factory,
) -> None:
    canary = "sk-local-ledger-canary-123456789"
    action = local_action_factory(
        normalized_arguments={
            "path": "notes/output.txt",
            "authorization": f"Bearer {canary}",
        }
    )
    ledger.begin(action)
    ledger.finish(
        action.action_id,
        ActionStatus.FAILED,
        {"error_detail_safe": f"provider failed with token={canary}"},
    )

    persisted_files = (
        ledger.path,
        Path(f"{ledger.path}-wal"),
        Path(f"{ledger.path}-shm"),
    )
    for path in persisted_files:
        if path.exists():
            assert canary.encode() not in path.read_bytes(), f"secret leaked into {path.name}"
    assert all(canary not in repr(entry) for entry in ledger.entries())
