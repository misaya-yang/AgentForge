from __future__ import annotations

import pytest

from local_node.errors import IdempotencyConflict, LedgerIntegrityError
from local_node.ledger import ActionLedger
from local_node.models import ActionStatus


def test_ledger_rejects_second_terminal_and_redacts_secret(
    tmp_path, action_factory, platform_signature_verifier
):
    path = tmp_path / "ledger.sqlite"
    ledger = ActionLedger(path, platform_signature_verifier=platform_signature_verifier)
    action = action_factory("test", {"value": 1}, "target", approved=False)
    ledger.begin(action)
    record = ledger.finish(
        action.action_id,
        ActionStatus.FAILED,
        {"error_detail_safe": "SECRET_CANARY_123", "token": "sk-example"},
    )
    assert record.result == {"error_detail_safe": "[REDACTED]", "token": "[REDACTED]"}
    with pytest.raises(IdempotencyConflict):
        ledger.finish(action.action_id, ActionStatus.SUCCEEDED, {"ok": True})
    assert ledger.get_by_idempotency_key(action.idempotency_key).status is ActionStatus.FAILED
    assert ledger.entries()[0]["event_type"] == "policy_check"
    ledger.close()
    for candidate in (path, path.with_name(path.name + "-wal"), path.with_name(path.name + "-shm")):
        if candidate.exists():
            raw = candidate.read_bytes()
            assert b"SECRET_CANARY_123" not in raw
            assert b"sk-example" not in raw


def test_ledger_detects_event_tampering(tmp_path, action_factory, platform_signature_verifier):
    path = tmp_path / "ledger.sqlite"
    ledger = ActionLedger(path, platform_signature_verifier=platform_signature_verifier)
    action = action_factory("test", {"value": 1}, "target", approved=False)
    ledger.begin(action)
    ledger.mark_dispatched(action.action_id)
    ledger.mark_running(action.action_id)
    ledger.finish(action.action_id, ActionStatus.SUCCEEDED, {"ok": True})
    assert ledger.verify_integrity()
    ledger._db.execute("UPDATE events SET event_type='tampered' WHERE seq=1")
    with pytest.raises(LedgerIntegrityError):
        ledger.verify_integrity()
