from __future__ import annotations

import pytest
from ai_gateway_core.local_node import LocalNodeAction, LocalNodeDeviceScope, arguments_digest


def test_local_node_action_uses_gateway_scope_and_hash_contract() -> None:
    arguments = {"path": "notes.txt"}
    action = LocalNodeAction(
        scope=LocalNodeDeviceScope("tenant", "user", "session", "device", "channel"),
        lease_id="00000000-0000-0000-0000-000000000003",
        execution_id="00000000-0000-0000-0000-000000000001",
        run_id="00000000-0000-0000-0000-000000000002",
        tool_call_id="call",
        attempt_id="attempt",
        capability_revision=1,
        capability_id="local_node.file.read",
        effect="read",
        operation="file.read",
        arguments=arguments,
        arguments_sha256=arguments_digest(arguments),
        idempotency_key="idem",
        approval_id=None,
    )
    action.validate()


def _action(*, effect: str, approval_id: str | None, arguments: dict[str, object] | None = None) -> LocalNodeAction:
    values = arguments or {"path": "notes.txt"}
    return LocalNodeAction(
        scope=LocalNodeDeviceScope("tenant", "user", "session", "device", "channel"),
        lease_id="00000000-0000-0000-0000-000000000003",
        execution_id="00000000-0000-0000-0000-000000000001",
        run_id="00000000-0000-0000-0000-000000000002",
        tool_call_id="call",
        attempt_id="attempt",
        capability_revision=2,
        capability_id="local_node.file.write",
        effect=effect,
        operation="file.write",
        arguments=values,
        arguments_sha256=arguments_digest(values),
        idempotency_key="idem",
        approval_id=approval_id,
    )


def test_write_and_unknown_actions_require_an_approval_id() -> None:
    for effect in ("write", "unknown"):
        with pytest.raises(ValueError, match="require approval"):
            _action(effect=effect, approval_id=None).validate()
        _action(effect=effect, approval_id="approval-1").validate()


def test_read_actions_reject_approval_and_unknown_effects() -> None:
    with pytest.raises(ValueError, match="cannot carry approval"):
        _action(effect="read", approval_id="approval-1").validate()
    with pytest.raises(ValueError, match="effect is invalid"):
        _action(effect="delete", approval_id=None).validate()


def test_arguments_hash_is_bound_to_canonical_arguments() -> None:
    action = _action(effect="write", approval_id="approval-1")
    tampered = _action(effect="write", approval_id="approval-1", arguments={"path": "other.txt"})
    object.__setattr__(tampered, "arguments_sha256", action.arguments_sha256)
    with pytest.raises(ValueError, match="arguments hash mismatch"):
        tampered.validate()
