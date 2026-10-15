"""Strict, credential-free Local Node contracts owned by the Gateway."""

from __future__ import annotations

import hashlib
import json
from collections.abc import Mapping
from dataclasses import dataclass
from enum import Enum
from typing import Any

LOCAL_NODE_PROTOCOL_VERSION = "ai-platform.local-node.v1"


class LocalNodeReceiptStatus(str, Enum):
    RUNNING = "running"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"
    TIMEOUT = "timeout"
    SIDE_EFFECT_UNKNOWN = "side_effect_unknown"


@dataclass(frozen=True, slots=True)
class LocalNodeDeviceScope:
    tenant_id: str
    user_id: str
    session_id: str
    device_id: str
    channel_id: str

    def validate(self) -> None:
        validate_scope(self)


@dataclass(frozen=True, slots=True)
class LocalNodeCapability:
    capability: str
    operation: str
    effect: str
    schema: Mapping[str, Any]
    revision: int


@dataclass(frozen=True, slots=True)
class LocalNodeAction:
    scope: LocalNodeDeviceScope
    lease_id: str
    execution_id: str
    run_id: str
    tool_call_id: str
    attempt_id: str
    capability_revision: int
    capability_id: str
    effect: str
    operation: str
    arguments: Mapping[str, Any]
    arguments_sha256: str
    idempotency_key: str
    approval_id: str | None
    grant_id: str | None = None
    grant_revision: int | None = None
    dispatch_fence: str | None = None

    def validate(self) -> None:
        self.scope.validate()
        fields = (
            self.execution_id,
            self.lease_id,
            self.run_id,
            self.tool_call_id,
            self.attempt_id,
            self.operation,
            self.idempotency_key,
            self.capability_id,
            self.effect,
        )
        if any(not isinstance(value, str) or not value or len(value) > 255 for value in fields):
            raise ValueError("local node action identity is invalid")
        if self.effect not in {"read", "write", "unknown"}:
            raise ValueError("local node action effect is invalid")
        if self.approval_id is not None and (
            not isinstance(self.approval_id, str)
            or not self.approval_id
            or len(self.approval_id) > 255
            or any(ord(char) < 0x20 for char in self.approval_id)
        ):
            raise ValueError("local node approval identity is invalid")
        if self.effect == "read" and self.approval_id is not None:
            raise ValueError("read action cannot carry approval")
        if self.effect in {"write", "unknown"} and self.approval_id is None:
            raise ValueError("write and unknown actions require approval")
        if self.capability_revision < 1 or not isinstance(self.arguments, Mapping):
            raise ValueError("local node action is invalid")
        if self.grant_revision is not None and self.grant_revision < 1:
            raise ValueError("local node grant revision is invalid")
        if self.arguments_sha256 != arguments_digest(self.arguments):
            raise ValueError("local node arguments hash mismatch")


@dataclass(frozen=True, slots=True)
class LocalNodeReceipt:
    execution_id: str
    tenant_id: str
    user_id: str
    session_id: str
    device_id: str
    dispatch_fence: str
    sequence: int
    status: LocalNodeReceiptStatus
    event: str
    payload: Mapping[str, Any]
    channel_id: str | None = None

    def validate(self, *, after_sequence: int = 0) -> None:
        if (
            not self.execution_id
            or not self.device_id
            or not self.dispatch_fence
            or self.sequence <= after_sequence
            or (self.status == LocalNodeReceiptStatus.SIDE_EFFECT_UNKNOWN
                and self.event != "action.side_effect_unknown")
        ):
            raise ValueError("local node receipt is invalid")
        if not isinstance(self.payload, Mapping):
            raise ValueError("local node receipt payload is invalid")


def arguments_digest(arguments: Mapping[str, Any]) -> str:
    if not isinstance(arguments, Mapping):
        raise ValueError("local node arguments must be an object")
    try:
        encoded = json.dumps(
            arguments,
            ensure_ascii=False,
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
    except (TypeError, ValueError) as exc:
        raise ValueError("local node arguments are not canonicalizable") from exc
    return "sha256:" + hashlib.sha256(encoded).hexdigest()


def validate_scope(scope: LocalNodeDeviceScope) -> None:
    values = (
        scope.tenant_id,
        scope.user_id,
        scope.session_id,
        scope.device_id,
        scope.channel_id,
    )
    if any(
        not isinstance(value, str)
        or not value
        or len(value) > 255
        or any(ord(char) < 0x20 for char in value)
        for value in values
    ):
        raise ValueError("local node device scope is invalid")
