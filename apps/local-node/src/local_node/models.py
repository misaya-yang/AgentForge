"""Protocol-neutral action and approval contracts.

Provider-specific calls (OpenAI ``computer_call``, ordinary function tools,
or Qwen tool calls) must be normalized into these records before dispatch.
"""

from __future__ import annotations

import hashlib
import json
import math
import time
from dataclasses import dataclass
from enum import StrEnum
from typing import Any, Mapping, Protocol, runtime_checkable

from .errors import ApprovalRequired, CapabilityDenied, StaleTargetError


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def digest_payload(value: Any) -> str:
    return hashlib.sha256(canonical_json(value).encode("utf-8")).hexdigest()


class ActionStatus(StrEnum):
    PROPOSED = "proposed"
    POLICY_CHECK = "policy_check"
    AWAITING_APPROVAL = "awaiting_approval"
    DISPATCHED = "dispatched"
    RUNNING = "running"
    OBSERVED = "observed"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    CANCELLED = "cancelled"
    INTERRUPTED = "interrupted"
    UNKNOWN = "unknown"


TERMINAL_STATUSES = frozenset(
    {
        ActionStatus.SUCCEEDED,
        ActionStatus.FAILED,
        ActionStatus.CANCELLED,
        ActionStatus.INTERRUPTED,
        ActionStatus.UNKNOWN,
    }
)


@runtime_checkable
class PlatformSignatureVerifier(Protocol):
    """Trusted verifier injected by the companion's platform configuration.

    The action envelope carries data and a signature, never verifier authority.
    A transport may deserialize :class:`ActionContext`, but only a verifier
    supplied independently by trusted local configuration may authorize it.
    """

    def verify(
        self,
        *,
        key_id: str,
        payload: bytes,
        signature: str,
    ) -> bool: ...


@runtime_checkable
class TrustedLocalApprovalVerifier(Protocol):
    """Independent trusted-local verifier with durable one-use consumption."""

    @property
    def device_id(self) -> str: ...

    def verify_and_consume(
        self,
        *,
        payload: bytes,
        signature: str,
        nonce: str,
    ) -> bool: ...


@dataclass(frozen=True, slots=True)
class ApprovalProof:
    approval_id: str
    action_id: str
    device_id: str
    arguments_digest: str
    target_snapshot_digest: str
    policy_snapshot_digest: str
    nonce: str
    expires_at: float
    local_signature: str

    def canonical_local_payload(self) -> bytes:
        return canonical_json(
            {
                "approval_id": self.approval_id,
                "action_id": self.action_id,
                "device_id": self.device_id,
                "arguments_digest": self.arguments_digest,
                "target_snapshot_digest": self.target_snapshot_digest,
                "policy_snapshot_digest": self.policy_snapshot_digest,
                "nonce": self.nonce,
                "expires_at": self.expires_at,
            }
        ).encode("utf-8")

    def validate_and_consume(
        self,
        *,
        action_id: str,
        device_id: str,
        arguments_digest: str,
        target_snapshot_digest: str,
        policy_snapshot_digest: str,
        verifier: TrustedLocalApprovalVerifier | None,
        now: float | None = None,
    ) -> None:
        current = time.time() if now is None else now
        if not math.isfinite(self.expires_at):
            raise ApprovalRequired("approval lifetime is invalid")
        if current >= self.expires_at:
            raise ApprovalRequired("approval expired")
        if self.action_id != action_id or self.device_id != device_id:
            raise ApprovalRequired("approval action or device changed")
        if self.arguments_digest != arguments_digest:
            raise ApprovalRequired("approval arguments changed")
        if self.policy_snapshot_digest != policy_snapshot_digest:
            raise ApprovalRequired("approval policy changed")
        if self.target_snapshot_digest != target_snapshot_digest:
            raise StaleTargetError("approval target changed")
        if verifier is None:
            raise ApprovalRequired("trusted local approval verifier is unavailable")
        if verifier.device_id != device_id:
            raise ApprovalRequired("trusted local approval verifier belongs to another device")
        if not self.nonce or len(self.nonce) > 512 or not self.local_signature:
            raise ApprovalRequired("trusted local approval receipt is invalid")
        try:
            verified = verifier.verify_and_consume(
                payload=self.canonical_local_payload(),
                signature=self.local_signature,
                nonce=self.nonce,
            )
        except Exception as exc:
            raise ApprovalRequired("trusted local approval verification failed") from exc
        if verified is not True:
            raise ApprovalRequired("trusted local approval verification failed")


@dataclass(frozen=True, slots=True)
class ActionContext:
    action_id: str
    idempotency_key: str
    tenant_id: str
    user_id: str
    session_id: str
    run_id: str
    agent_id: str
    agent_version: str
    call_id: str
    device_id: str
    envelope_version: int
    capability: str
    tool_name: str
    operation: str
    capability_lease_id: str
    resource_refs: tuple[str, ...]
    arguments_digest: str
    target_snapshot_digest: str
    policy_snapshot_digest: str
    nonce: str
    issued_at: float
    expires_at: float
    platform_key_id: str
    platform_signature: str
    approval: ApprovalProof | None = None
    trace_context: Mapping[str, str] | None = None

    @classmethod
    def create(
        cls,
        *,
        action_id: str,
        idempotency_key: str,
        tenant_id: str,
        user_id: str,
        session_id: str,
        run_id: str,
        agent_id: str,
        agent_version: str,
        call_id: str,
        device_id: str,
        envelope_version: int,
        capability: str,
        tool_name: str,
        operation: str,
        capability_lease_id: str,
        resource_refs: tuple[str, ...],
        normalized_arguments: Mapping[str, Any],
        target_snapshot_digest: str,
        policy_snapshot_digest: str,
        nonce: str,
        platform_key_id: str,
        platform_signature: str = "",
        ttl_seconds: float = 60,
        approval: ApprovalProof | None = None,
        trace_context: Mapping[str, str] | None = None,
        now: float | None = None,
    ) -> "ActionContext":
        issued = time.time() if now is None else now
        normalized_trace = None if trace_context is None else dict(trace_context)
        return cls(
            action_id=action_id,
            idempotency_key=idempotency_key,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            run_id=run_id,
            agent_id=agent_id,
            agent_version=agent_version,
            call_id=call_id,
            device_id=device_id,
            envelope_version=envelope_version,
            capability=capability,
            tool_name=tool_name,
            operation=operation,
            capability_lease_id=capability_lease_id,
            resource_refs=tuple(resource_refs),
            arguments_digest=digest_payload(normalized_arguments),
            target_snapshot_digest=target_snapshot_digest,
            policy_snapshot_digest=policy_snapshot_digest,
            nonce=nonce,
            issued_at=issued,
            expires_at=issued + ttl_seconds,
            platform_key_id=platform_key_id,
            platform_signature=platform_signature,
            approval=approval,
            trace_context=normalized_trace,
        )

    def canonical_signed_payload(self) -> bytes:
        """Return the only byte representation accepted by platform verifiers."""
        return canonical_json(
            {
                "action_id": self.action_id,
                "idempotency_key": self.idempotency_key,
                "tenant_id": self.tenant_id,
                "user_id": self.user_id,
                "session_id": self.session_id,
                "run_id": self.run_id,
                "agent_id": self.agent_id,
                "agent_version": self.agent_version,
                "call_id": self.call_id,
                "device_id": self.device_id,
                "envelope_version": self.envelope_version,
                "capability": self.capability,
                "tool_name": self.tool_name,
                "operation": self.operation,
                "capability_lease_id": self.capability_lease_id,
                "resource_refs": list(self.resource_refs),
                "resource_refs_digest": digest_payload(list(self.resource_refs)),
                "arguments_digest": self.arguments_digest,
                "target_snapshot_digest": self.target_snapshot_digest,
                "policy_snapshot_digest": self.policy_snapshot_digest,
                "nonce": self.nonce,
                "issued_at": self.issued_at,
                "expires_at": self.expires_at,
                "platform_key_id": self.platform_key_id,
                "approval": None
                if self.approval is None
                else {
                    "approval_id": self.approval.approval_id,
                    "action_id": self.approval.action_id,
                    "device_id": self.approval.device_id,
                    "arguments_digest": self.approval.arguments_digest,
                    "target_snapshot_digest": self.approval.target_snapshot_digest,
                    "policy_snapshot_digest": self.approval.policy_snapshot_digest,
                    "nonce": self.approval.nonce,
                    "expires_at": self.approval.expires_at,
                    "local_signature": self.approval.local_signature,
                },
            }
        ).encode("utf-8")

    def verify_platform_signature(
        self,
        verifier: PlatformSignatureVerifier | None,
    ) -> None:
        self._validate_envelope_metadata()
        if verifier is None:
            raise CapabilityDenied("platform signature verifier is unavailable")
        if not isinstance(self.platform_signature, str) or not (
            self.platform_signature and len(self.platform_signature) <= 8192
        ):
            raise CapabilityDenied("platform-signed action envelope is required")
        try:
            verified = verifier.verify(
                key_id=self.platform_key_id,
                payload=self.canonical_signed_payload(),
                signature=self.platform_signature,
            )
        except Exception as exc:
            raise CapabilityDenied("platform action signature verification failed") from exc
        if verified is not True:
            raise CapabilityDenied("platform action signature verification failed")

    def _validate_envelope_metadata(self) -> None:
        now = time.time()
        identifiers = (
            self.action_id,
            self.idempotency_key,
            self.tenant_id,
            self.user_id,
            self.session_id,
            self.run_id,
            self.agent_id,
            self.agent_version,
            self.call_id,
            self.device_id,
            self.capability,
            self.tool_name,
            self.operation,
            self.capability_lease_id,
            self.nonce,
            self.platform_key_id,
            self.arguments_digest,
            self.target_snapshot_digest,
            self.policy_snapshot_digest,
        )
        if any(
            not isinstance(value, str) or not value or len(value) > 512 for value in identifiers
        ):
            raise CapabilityDenied("action envelope identity is invalid")
        if type(self.envelope_version) is not int or self.envelope_version != 1:
            raise CapabilityDenied("action envelope version is unsupported")
        if not isinstance(self.resource_refs, tuple) or not self.resource_refs:
            raise CapabilityDenied("action envelope resource references are invalid")
        if len(self.resource_refs) > 64 or any(
            not isinstance(value, str) or not value or len(value) > 1024
            for value in self.resource_refs
        ):
            raise CapabilityDenied("action envelope resource references are invalid")
        if len(set(self.resource_refs)) != len(self.resource_refs):
            raise CapabilityDenied("action envelope resource references are invalid")
        if not math.isfinite(self.issued_at) or not math.isfinite(self.expires_at):
            raise CapabilityDenied("action envelope lifetime is invalid")
        if self.issued_at > now + 30 or self.expires_at <= self.issued_at:
            raise CapabilityDenied("action envelope lifetime is invalid")
        if now >= self.expires_at:
            raise CapabilityDenied("action envelope expired")
        if self.approval is not None:
            approval_identifiers = (
                self.approval.approval_id,
                self.approval.action_id,
                self.approval.device_id,
                self.approval.arguments_digest,
                self.approval.target_snapshot_digest,
                self.approval.policy_snapshot_digest,
                self.approval.nonce,
                self.approval.local_signature,
            )
            if any(
                not isinstance(value, str) or not value or len(value) > 512
                for value in approval_identifiers
            ) or not math.isfinite(self.approval.expires_at):
                raise CapabilityDenied("action approval metadata is invalid")
            if self.approval.expires_at > self.expires_at:
                raise CapabilityDenied("action approval exceeds envelope lifetime")

    def validate_payload(
        self,
        normalized_arguments: Mapping[str, Any],
        *,
        verifier: PlatformSignatureVerifier | None,
        capability_lease_id: str | None = None,
        resource_refs: tuple[str, ...] | None = None,
    ) -> None:
        self._validate_envelope_metadata()
        if capability_lease_id is not None and self.capability_lease_id != capability_lease_id:
            raise CapabilityDenied("action capability lease does not match local scope")
        if resource_refs is not None and self.resource_refs != tuple(resource_refs):
            raise CapabilityDenied("action resources do not match local scope")
        if digest_payload(normalized_arguments) != self.arguments_digest:
            raise CapabilityDenied("action arguments do not match signed envelope")
        self.verify_platform_signature(verifier)

    def require_approval(
        self,
        *,
        target_snapshot_digest: str,
        verifier: TrustedLocalApprovalVerifier | None,
    ) -> None:
        if self.target_snapshot_digest != target_snapshot_digest:
            raise StaleTargetError("action envelope target changed")
        if self.approval is None:
            raise ApprovalRequired("local approval is required")
        self.approval.validate_and_consume(
            action_id=self.action_id,
            device_id=self.device_id,
            arguments_digest=self.arguments_digest,
            target_snapshot_digest=target_snapshot_digest,
            policy_snapshot_digest=self.policy_snapshot_digest,
            verifier=verifier,
        )
