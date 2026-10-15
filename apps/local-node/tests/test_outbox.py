from __future__ import annotations

import sqlite3

import pytest

from local_node.models import ActionStatus
from local_node.outbox import OutboxError, ReceiptOutbox


def test_outbox_is_ordered_digest_only_and_contiguously_acked(tmp_path):
    outbox = ReceiptOutbox(tmp_path / "state" / "receipts.sqlite")
    first = outbox.append(
        event_id="event-1",
        event_type="action.succeeded",
        action_id="action-1",
        status=ActionStatus.SUCCEEDED,
        result={"artifact_ref": "artifact-1", "content": "must-not-be-stored"},
    )
    second = outbox.append(
        event_id="event-2",
        event_type="action.failed",
        action_id="action-2",
        status=ActionStatus.FAILED,
        error_code="driver_unavailable",
    )

    assert [event.sequence for event in outbox.pending()] == [first.sequence, second.sequence]
    exported = outbox.export_pending_json()
    assert "must-not-be-stored" not in exported
    assert first.result_digest is not None

    outbox.acknowledge(first.sequence)
    assert [event.event_id for event in outbox.pending()] == ["event-2"]
    with pytest.raises(OutboxError):
        outbox.acknowledge(second.sequence + 1)


def test_outbox_detects_acknowledgement_gap_and_event_id_conflict(tmp_path):
    outbox = ReceiptOutbox(tmp_path / "state" / "receipts.sqlite")
    first = outbox.append(event_id="event-1", event_type="heartbeat")
    second = outbox.append(event_id="event-2", event_type="heartbeat")
    outbox.append(event_id="event-1", event_type="heartbeat")
    with pytest.raises(OutboxError):
        outbox.append(event_id="event-1", event_type="different")

    with sqlite3.connect(outbox.path) as db:
        db.execute("DELETE FROM receipt_events WHERE sequence=?", (first.sequence,))
    with pytest.raises(OutboxError):
        outbox.acknowledge(second.sequence)


def test_outbox_rejects_raw_and_digest_together(tmp_path):
    outbox = ReceiptOutbox(tmp_path / "state" / "receipts.sqlite")
    with pytest.raises(OutboxError):
        outbox.append(
            event_id="event-1",
            event_type="action.succeeded",
            result={"ok": True},
            result_digest="0" * 64,
        )


def test_bounded_file_result_is_transported_then_erased_on_ack(tmp_path):
    outbox = ReceiptOutbox(tmp_path / "state" / "receipts.sqlite")
    event = outbox.append(
        event_id="event-file-read",
        event_type="action.succeeded",
        action_id="action-file-read",
        status=ActionStatus.SUCCEEDED,
        result={"kind": "file_read", "content": "temporary-result"},
        retain_result=True,
    )

    assert event.as_dict()["result_digest"] is not None
    assert "result" not in event.as_dict()
    assert event.as_dict(include_result=True)["result"] == {
        "kind": "file_read",
        "content": "temporary-result",
    }
    assert "temporary-result" not in outbox.export_pending_json()

    outbox.acknowledge(event.sequence)
    assert outbox.pending() == ()
    assert b"temporary-result" not in outbox.path.read_bytes()
