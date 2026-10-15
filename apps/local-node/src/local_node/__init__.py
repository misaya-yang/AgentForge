"""Device-side, fail-closed primitives for AI--Platfform Local Node."""

from .errors import (
    ApprovalRequired,
    CapabilityDenied,
    DriverUnavailable,
    IdempotencyConflict,
    LocalNodeError,
    PathEscapeError,
    StaleTargetError,
)
from .models import (
    ActionContext,
    ActionStatus,
    ApprovalProof,
    PlatformSignatureVerifier,
    TrustedLocalApprovalVerifier,
)

__all__ = [
    "ActionContext",
    "ActionStatus",
    "ApprovalProof",
    "PlatformSignatureVerifier",
    "TrustedLocalApprovalVerifier",
    "ApprovalRequired",
    "CapabilityDenied",
    "DriverUnavailable",
    "IdempotencyConflict",
    "LocalNodeError",
    "PathEscapeError",
    "StaleTargetError",
]
