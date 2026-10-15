"""Typed MCP failure, side-effect, and circuit-breaker semantics."""

from __future__ import annotations

import asyncio
import hashlib
import json
import time
import uuid
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from enum import Enum
from typing import Any

from .outcomes import (
    FailureClass,
    RecoveryAction,
    RetrySafety,
    SideEffectState,
    UserVisibility,
)


class MCPFailureKind(str, Enum):
    CANCELLED = "cancelled"
    DEADLINE = "deadline"
    TRANSPORT = "transport"
    PROTOCOL = "protocol"
    APPLICATION = "application"
    AUTHORIZATION = "authorization"
    SIDE_EFFECT_UNKNOWN = "side_effect_unknown"


class MCPOperationKind(str, Enum):
    READ = "read"
    WRITE = "write"
    UNKNOWN = "unknown"


class MCPCircuitState(str, Enum):
    CLOSED = "closed"
    OPEN = "open"
    HALF_OPEN = "half_open"


@dataclass(frozen=True)
class MCPInvocationPolicy:
    """Trusted operation facts used to decide whether replay is safe."""

    operation_kind: MCPOperationKind = MCPOperationKind.UNKNOWN
    operation_id: str = ""
    circuit_scope: str = ""
    idempotency_key: str | None = None
    idempotency_supported: bool = False
    read_back_tool: str | None = None
    read_back_argument: str = "operation_id"
    compensation_available: bool = False
    max_attempts: int = 1

    def __post_init__(self) -> None:
        object.__setattr__(self, "operation_kind", MCPOperationKind(self.operation_kind))
        object.__setattr__(self, "max_attempts", max(1, min(3, int(self.max_attempts))))

    @property
    def side_effecting(self) -> bool:
        return self.operation_kind is not MCPOperationKind.READ

    @property
    def can_idempotently_retry(self) -> bool:
        return bool(self.idempotency_supported and self.idempotency_key and self.max_attempts > 1)

    @property
    def read_back_available(self) -> bool:
        return bool(self.read_back_tool and self.operation_id)

    def request_meta(self) -> dict[str, str] | None:
        if not (self.idempotency_supported and self.idempotency_key):
            return None
        return {
            "idempotencyKey": str(self.idempotency_key),
            "operationId": self.operation_id,
        }


@dataclass(frozen=True)
class MCPFailureDecision:
    failure_kind: MCPFailureKind
    cause: MCPFailureKind
    failure_class: FailureClass
    retry_safety: RetrySafety
    recovery_action: RecoveryAction
    user_visibility: UserVisibility
    side_effect_state: SideEffectState
    recoverable: bool
    auto_retry_allowed: bool
    operation_id: str
    idempotency_key_present: bool
    read_back_required: bool = False
    compensation_required: bool = False

    def to_dict(self) -> dict[str, Any]:
        return {
            "failure_kind": self.failure_kind.value,
            "cause": self.cause.value,
            "failure_class": self.failure_class.value,
            "retry_safety": self.retry_safety.value,
            "recovery_action": self.recovery_action.value,
            "user_visibility": self.user_visibility.value,
            "side_effect_state": self.side_effect_state.value,
            "recoverable": self.recoverable,
            "auto_retry_allowed": self.auto_retry_allowed,
            "operation_id": self.operation_id,
            "idempotency_key_present": self.idempotency_key_present,
            "read_back_required": self.read_back_required,
            "compensation_required": self.compensation_required,
        }


_DEADLINE_CODES = frozenset({"MCP_TIMEOUT", "MCP_HOST_DEADLINE"})
_TRANSPORT_CODES = frozenset(
    {
        "MCP_UPSTREAM_UNAVAILABLE",
        "MCP_DNS_UNAVAILABLE",
        "MCP_NOT_CONNECTED",
        "MCP_CIRCUIT_OPEN",
    }
)
_CANCELLATION_CODES = frozenset({"MCP_CANCELLED", "MCP_CANCELLED_AFTER_DISPATCH"})
_APPLICATION_CODES = frozenset(
    {
        "MCP_REMOTE_ERROR",
        "MCP_REMOTE_TOOL_ERROR",
        "MCP_UPSTREAM_REJECTED",
    }
)
_AUTHORIZATION_CODES = frozenset(
    {
        "MCP_CAPABILITY_TYPE_INVALID",
        "MCP_CAPABILITY_NOT_BOUND",
        "MCP_CAPABILITY_UNAVAILABLE",
        "MCP_CONNECTION_UNAVAILABLE",
        "MCP_SCHEMA_CHANGED",
        "MCP_RISK_CHANGED",
        "MCP_PRINCIPAL_POLICY_DENIED",
        "MCP_DELEGATED_PRINCIPAL_DENIED",
        "MCP_SERVICE_PRINCIPAL_DENIED",
        "MCP_PUBLIC_CHANNEL_DENIED",
        "MCP_SECRET_UNAVAILABLE",
        "MCP_ORIGIN_DENIED",
        "MCP_OAUTH_AUDIENCE_MISMATCH",
        "MCP_APPROVAL_REQUIRED",
        "MCP_AUTHORIZATION_UNAVAILABLE",
        "MCP_DNS_REBINDING_BLOCKED",
        "MCP_SSRF_BLOCKED",
        "MCP_TLS_REQUIRED",
        "MCP_URL_INVALID",
        "CONNECTOR_CAPABILITY_UNAVAILABLE",
        "CONNECTOR_PRINCIPAL_POLICY_DENIED",
        "CONNECTOR_DELEGATED_PRINCIPAL_DENIED",
        "CONNECTOR_SERVICE_PRINCIPAL_DENIED",
        "CONNECTOR_CHANNEL_DENIED",
        "CONNECTOR_PUBLIC_CHANNEL_DENIED",
        "CONNECTOR_SCOPE_DENIED",
    }
)


def failure_kind_for_code(stable_code: str) -> MCPFailureKind:
    code = str(stable_code or "MCP_ERROR").upper()
    if code in _CANCELLATION_CODES:
        return MCPFailureKind.CANCELLED
    if code in _DEADLINE_CODES:
        return MCPFailureKind.DEADLINE
    if code in _TRANSPORT_CODES:
        return MCPFailureKind.TRANSPORT
    if code in _APPLICATION_CODES:
        return MCPFailureKind.APPLICATION
    if code in _AUTHORIZATION_CODES or "AUTH" in code or "DENIED" in code:
        return MCPFailureKind.AUTHORIZATION
    return MCPFailureKind.PROTOCOL


def decide_mcp_failure(
    stable_code: str,
    policy: MCPInvocationPolicy,
    *,
    operation_started: bool = True,
) -> MCPFailureDecision:
    """Classify one failure without ever assuming an uncertain write is safe."""

    normalized_code = str(stable_code or "MCP_ERROR").upper()
    cause = failure_kind_for_code(normalized_code)
    key_present = bool(policy.idempotency_key and policy.idempotency_supported)
    if normalized_code == "MCP_APPROVAL_REQUIRED":
        return MCPFailureDecision(
            failure_kind=MCPFailureKind.AUTHORIZATION,
            cause=MCPFailureKind.AUTHORIZATION,
            failure_class=FailureClass.APPROVAL_PENDING,
            retry_safety=RetrySafety.NOT_APPLICABLE,
            recovery_action=RecoveryAction.PAUSE,
            user_visibility=UserVisibility.BLOCKING,
            side_effect_state=SideEffectState.NOT_STARTED,
            recoverable=True,
            auto_retry_allowed=False,
            operation_id=policy.operation_id,
            idempotency_key_present=key_present,
        )
    if cause is MCPFailureKind.CANCELLED and (not operation_started or not policy.side_effecting):
        return MCPFailureDecision(
            failure_kind=MCPFailureKind.CANCELLED,
            cause=MCPFailureKind.CANCELLED,
            failure_class=FailureClass.CANCELLED,
            retry_safety=RetrySafety.NOT_APPLICABLE,
            recovery_action=RecoveryAction.ABORT,
            user_visibility=UserVisibility.INFO,
            side_effect_state=(
                SideEffectState.NOT_STARTED if not operation_started else SideEffectState.NONE
            ),
            recoverable=False,
            auto_retry_allowed=False,
            operation_id=policy.operation_id,
            idempotency_key_present=key_present,
        )
    if normalized_code in {
        "MCP_CIRCUIT_OPEN",
        "MCP_DNS_UNAVAILABLE",
        "MCP_NOT_CONNECTED",
    }:
        return MCPFailureDecision(
            failure_kind=cause,
            cause=cause,
            failure_class=FailureClass.TRANSIENT_TRANSPORT,
            retry_safety=RetrySafety.SAFE,
            recovery_action=RecoveryAction.RETRY,
            user_visibility=UserVisibility.WARNING,
            side_effect_state=SideEffectState.NOT_STARTED,
            recoverable=True,
            auto_retry_allowed=(normalized_code != "MCP_CIRCUIT_OPEN" and policy.max_attempts > 1),
            operation_id=policy.operation_id,
            idempotency_key_present=key_present,
        )
    if not operation_started and cause in {
        MCPFailureKind.DEADLINE,
        MCPFailureKind.TRANSPORT,
        MCPFailureKind.PROTOCOL,
    }:
        return MCPFailureDecision(
            failure_kind=cause,
            cause=cause,
            failure_class=FailureClass.TRANSIENT_TRANSPORT,
            retry_safety=RetrySafety.SAFE,
            recovery_action=RecoveryAction.RETRY,
            user_visibility=UserVisibility.WARNING,
            side_effect_state=SideEffectState.NOT_STARTED,
            recoverable=True,
            auto_retry_allowed=False,
            operation_id=policy.operation_id,
            idempotency_key_present=key_present,
        )
    possible_lost_response = cause in {
        MCPFailureKind.CANCELLED,
        MCPFailureKind.DEADLINE,
        MCPFailureKind.TRANSPORT,
        MCPFailureKind.PROTOCOL,
    }
    if policy.side_effecting and possible_lost_response:
        if policy.can_idempotently_retry:
            action = RecoveryAction.RETRY
            auto_retry = True
            read_back = False
            compensate = False
        elif policy.read_back_available:
            action = RecoveryAction.RESUME
            auto_retry = False
            read_back = True
            compensate = False
        elif policy.compensation_available:
            action = RecoveryAction.COMPENSATE
            auto_retry = False
            read_back = False
            compensate = True
        else:
            action = RecoveryAction.PAUSE
            auto_retry = False
            read_back = False
            compensate = False
        return MCPFailureDecision(
            failure_kind=MCPFailureKind.SIDE_EFFECT_UNKNOWN,
            cause=cause,
            failure_class=FailureClass.SIDE_EFFECT_UNKNOWN,
            retry_safety=RetrySafety.NEEDS_IDEMPOTENCY,
            recovery_action=action,
            user_visibility=UserVisibility.BLOCKING,
            side_effect_state=SideEffectState.UNKNOWN,
            recoverable=True,
            auto_retry_allowed=auto_retry,
            operation_id=policy.operation_id,
            idempotency_key_present=key_present,
            read_back_required=read_back,
            compensation_required=compensate,
        )

    if cause in {MCPFailureKind.DEADLINE, MCPFailureKind.TRANSPORT}:
        return MCPFailureDecision(
            failure_kind=cause,
            cause=cause,
            failure_class=FailureClass.TRANSIENT_TRANSPORT,
            retry_safety=RetrySafety.SAFE,
            recovery_action=RecoveryAction.RETRY,
            user_visibility=UserVisibility.WARNING,
            side_effect_state=SideEffectState.NONE,
            recoverable=True,
            auto_retry_allowed=policy.max_attempts > 1,
            operation_id=policy.operation_id,
            idempotency_key_present=key_present,
        )
    if cause is MCPFailureKind.APPLICATION:
        return MCPFailureDecision(
            failure_kind=cause,
            cause=cause,
            failure_class=FailureClass.TOOL_ERROR,
            retry_safety=RetrySafety.UNSAFE,
            recovery_action=RecoveryAction.ABORT,
            user_visibility=UserVisibility.WARNING,
            side_effect_state=SideEffectState.NONE,
            recoverable=False,
            auto_retry_allowed=False,
            operation_id=policy.operation_id,
            idempotency_key_present=key_present,
        )
    if cause is MCPFailureKind.AUTHORIZATION:
        return MCPFailureDecision(
            failure_kind=cause,
            cause=cause,
            failure_class=FailureClass.POLICY_UNAVAILABLE,
            retry_safety=RetrySafety.UNSAFE,
            recovery_action=RecoveryAction.ABORT,
            user_visibility=UserVisibility.BLOCKING,
            side_effect_state=SideEffectState.NOT_STARTED,
            recoverable=False,
            auto_retry_allowed=False,
            operation_id=policy.operation_id,
            idempotency_key_present=key_present,
        )
    return MCPFailureDecision(
        failure_kind=MCPFailureKind.PROTOCOL,
        cause=cause,
        failure_class=FailureClass.TOOL_ERROR,
        retry_safety=RetrySafety.UNSAFE,
        recovery_action=RecoveryAction.ABORT,
        user_visibility=UserVisibility.WARNING,
        side_effect_state=SideEffectState.NONE,
        recoverable=False,
        auto_retry_allowed=False,
        operation_id=policy.operation_id,
        idempotency_key_present=key_present,
    )


def build_operation_identity(
    *,
    context: Any,
    tool_name: str,
    arguments: dict[str, Any],
    logical_operation_id: str = "",
) -> tuple[str, str]:
    """Return deterministic operation and idempotency identities without raw args."""

    def _value(name: str) -> Any:
        if isinstance(context, Mapping):
            return context.get(name)
        return getattr(context, name, None)

    scope = ":".join(
        [
            str(_value("tenant_id") or ""),
            str(_value("user_id") or ""),
            str(_value("session_id") or ""),
            str(_value("run_id") or _value("request_id") or ""),
            str(tool_name),
        ]
    )
    encoded_args = json.dumps(arguments, sort_keys=True, separators=(",", ":"), default=str)
    fingerprint = hashlib.sha256(f"{scope}:{encoded_args}".encode()).hexdigest()
    digest = hashlib.sha256(f"{fingerprint}:{logical_operation_id}".encode()).hexdigest()
    return f"mcp_op_{digest[:24]}", f"mcp_idem_{digest[24:56]}"


@dataclass(frozen=True)
class MCPCircuitLease:
    state: MCPCircuitState
    probe_token: str | None = None


class MCPCircuitOpen(RuntimeError):
    def __init__(self, retry_after: float, state: MCPCircuitState) -> None:
        self.retry_after = max(0.0, float(retry_after))
        self.state = state
        super().__init__("MCP_CIRCUIT_OPEN")


class MCPCircuitBreaker:
    """Bounded closed/open/half-open breaker with one probe owner."""

    def __init__(
        self,
        *,
        failure_threshold: int = 3,
        cooldown_seconds: float = 30.0,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self.failure_threshold = max(1, min(20, int(failure_threshold)))
        self.cooldown_seconds = max(0.0, min(3600.0, float(cooldown_seconds)))
        self._clock = clock
        self._lock = asyncio.Lock()
        self._state = MCPCircuitState.CLOSED
        self._failure_streak = 0
        self._opened_at: float | None = None
        self._probe_token: str | None = None
        self._last_healthy_at: float | None = None
        self._touched_at = self._clock()

    @property
    def touched_at(self) -> float:
        return self._touched_at

    async def acquire(self) -> MCPCircuitLease:
        async with self._lock:
            now = self._clock()
            self._touched_at = now
            if self._state is MCPCircuitState.CLOSED:
                return MCPCircuitLease(MCPCircuitState.CLOSED)
            if self._state is MCPCircuitState.OPEN:
                opened_at = now if self._opened_at is None else self._opened_at
                elapsed = now - opened_at
                if elapsed < self.cooldown_seconds:
                    raise MCPCircuitOpen(
                        self.cooldown_seconds - elapsed,
                        MCPCircuitState.OPEN,
                    )
                token = uuid.uuid4().hex
                self._state = MCPCircuitState.HALF_OPEN
                self._probe_token = token
                return MCPCircuitLease(MCPCircuitState.HALF_OPEN, token)
            raise MCPCircuitOpen(0.0, MCPCircuitState.HALF_OPEN)

    async def validate(self, lease: MCPCircuitLease) -> None:
        """Revalidate a lease immediately before an upstream request starts.

        Callers can spend time queued behind a connection semaphore after
        acquiring a CLOSED lease.  A different request may open the circuit in
        that interval, so a queued request must not trust its stale lease.
        """

        async with self._lock:
            now = self._clock()
            self._touched_at = now
            if self._state is MCPCircuitState.CLOSED:
                if lease.probe_token is None:
                    return
            elif (
                self._state is MCPCircuitState.HALF_OPEN
                and lease.probe_token
                and lease.probe_token == self._probe_token
            ):
                return
            retry_after = 0.0
            if self._state is MCPCircuitState.OPEN and self._opened_at is not None:
                retry_after = max(
                    0.0,
                    self.cooldown_seconds - (now - self._opened_at),
                )
            raise MCPCircuitOpen(retry_after, self._state)

    async def record_success(self, lease: MCPCircuitLease) -> None:
        async with self._lock:
            if self._state is MCPCircuitState.HALF_OPEN and (
                not lease.probe_token or lease.probe_token != self._probe_token
            ):
                return
            if self._state is MCPCircuitState.OPEN and not lease.probe_token:
                return
            if lease.probe_token and lease.probe_token != self._probe_token:
                return
            self._state = MCPCircuitState.CLOSED
            self._failure_streak = 0
            self._opened_at = None
            self._probe_token = None
            self._last_healthy_at = self._clock()
            self._touched_at = self._last_healthy_at

    async def record_failure(self, lease: MCPCircuitLease) -> None:
        async with self._lock:
            now = self._clock()
            self._touched_at = now
            if lease.probe_token:
                if lease.probe_token != self._probe_token:
                    return
                self._state = MCPCircuitState.OPEN
                self._opened_at = now
                self._probe_token = None
                self._failure_streak = self.failure_threshold
                return
            if self._state is not MCPCircuitState.CLOSED:
                return
            self._failure_streak += 1
            if self._failure_streak >= self.failure_threshold:
                self._state = MCPCircuitState.OPEN
                self._opened_at = now

    async def record_neutral(self, lease: MCPCircuitLease) -> None:
        """Release a cancelled lease without changing upstream health history."""

        async with self._lock:
            self._touched_at = self._clock()
            if (
                self._state is MCPCircuitState.HALF_OPEN
                and lease.probe_token
                and lease.probe_token == self._probe_token
            ):
                # The probe did not establish health or failure. Return to OPEN
                # while releasing ownership; the elapsed cooldown allows the
                # next caller to acquire a fresh probe immediately.
                self._state = MCPCircuitState.OPEN
                self._probe_token = None

    async def snapshot(self) -> dict[str, Any]:
        async with self._lock:
            now = self._clock()
            retry_after = 0.0
            if self._state is MCPCircuitState.OPEN and self._opened_at is not None:
                retry_after = max(
                    0.0,
                    self.cooldown_seconds - (now - self._opened_at),
                )
            return {
                "state": self._state.value,
                "failure_streak": self._failure_streak,
                "retry_after_seconds": retry_after,
                "probe_owned": self._probe_token is not None,
                "last_healthy_at": self._last_healthy_at,
            }


def counts_toward_circuit(decision: MCPFailureDecision) -> bool:
    return decision.cause in {
        MCPFailureKind.DEADLINE,
        MCPFailureKind.TRANSPORT,
        MCPFailureKind.PROTOCOL,
    }


__all__ = [
    "MCPCircuitBreaker",
    "MCPCircuitLease",
    "MCPCircuitOpen",
    "MCPCircuitState",
    "MCPFailureDecision",
    "MCPFailureKind",
    "MCPInvocationPolicy",
    "MCPOperationKind",
    "build_operation_identity",
    "counts_toward_circuit",
    "decide_mcp_failure",
    "failure_kind_for_code",
]
