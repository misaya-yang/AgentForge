from __future__ import annotations

from dataclasses import replace

import pytest
from ai_gateway_core.local_node import (
    LocalNodeAction,
    LocalNodeDeviceScope,
    LocalNodeReceipt,
    LocalNodeReceiptStatus,
    arguments_digest,
)


def _scope() -> LocalNodeDeviceScope:
    return LocalNodeDeviceScope("tenant", "user", "session", "device", "channel")


def test_action_binds_arguments_and_scope() -> None:
    arguments = {"path": "notes.txt"}
    action = LocalNodeAction(
        scope=_scope(),
        lease_id="lease",
        execution_id="execution",
        run_id="run",
        tool_call_id="call",
        attempt_id="attempt",
        capability_revision=1,
        capability_id="local_node_action",
        effect="unknown",
        operation="file.read",
        arguments=arguments,
        arguments_sha256=arguments_digest(arguments),
        idempotency_key="idem",
        approval_id="approval",
    )
    action.validate()
    with pytest.raises(ValueError, match="hash"):
        replace(action, arguments_sha256="0" * 64).validate()


def test_receipt_rejects_replayed_sequence_and_unknown_shape() -> None:
    receipt = LocalNodeReceipt(
        execution_id="execution",
        tenant_id="tenant",
        user_id="user",
        session_id="session",
        device_id="device",
        dispatch_fence="fence",
        sequence=2,
        status=LocalNodeReceiptStatus.SUCCEEDED,
        event="action.succeeded",
        payload={"result": {"ok": True}},
    )
    receipt.validate(after_sequence=1)
    with pytest.raises(ValueError):
        receipt.validate(after_sequence=2)
