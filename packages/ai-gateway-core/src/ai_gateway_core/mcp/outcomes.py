"""Small failure vocabulary shared by the MCP resilience policy."""

from __future__ import annotations

from enum import Enum


class FailureClass(str, Enum):
    TRANSIENT_TRANSPORT = "transient_transport"
    POLICY_UNAVAILABLE = "policy_unavailable"
    INVALID_INPUT = "invalid_input"
    PROVIDER_REFUSAL = "provider_refusal"
    SIDE_EFFECT_UNKNOWN = "side_effect_unknown"
    PERSISTENCE_FAILURE = "persistence_failure"
    MODEL_ERROR = "model_error"
    TOOL_ERROR = "tool_error"
    MAX_ITERATIONS = "max_iterations"
    RUN_BUDGET_EXCEEDED = "run_budget_exceeded"
    APPROVAL_PENDING = "approval_pending"
    RESUME_REQUIRED = "resume_required"
    COMPENSATION_REQUIRED = "compensation_required"
    CANCELLED = "cancelled"
    INTERNAL_ERROR = "internal_error"


class RetrySafety(str, Enum):
    SAFE = "safe"
    NEEDS_IDEMPOTENCY = "needs_idempotency_or_read_back"
    UNSAFE = "unsafe"
    NOT_APPLICABLE = "not_applicable"


class RecoveryAction(str, Enum):
    RETRY = "retry"
    DEGRADE = "degrade"
    PAUSE = "pause"
    RESUME = "resume"
    ABORT = "abort"
    COMPENSATE = "compensate"


class UserVisibility(str, Enum):
    INFO = "info"
    WARNING = "warning"
    BLOCKING = "blocking"


class SideEffectState(str, Enum):
    NONE = "none"
    NOT_STARTED = "not_started"
    COMMITTED = "committed"
    UNKNOWN = "unknown"


__all__ = [
    "FailureClass",
    "RecoveryAction",
    "RetrySafety",
    "SideEffectState",
    "UserVisibility",
]
