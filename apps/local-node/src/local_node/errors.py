"""Stable, non-secret error types returned by the local capability broker."""


class LocalNodeError(RuntimeError):
    code = "local_node_error"


class BoundaryViolation(LocalNodeError):
    code = "boundary_violation"


class PairingError(LocalNodeError):
    code = "pairing_error"


class CapabilityDenied(LocalNodeError):
    code = "capability_denied"


class PathEscapeError(CapabilityDenied):
    code = "path_escape"


class StaleTargetError(LocalNodeError):
    code = "stale_target"


class ApprovalRequired(CapabilityDenied):
    code = "approval_required"


class IdempotencyConflict(LocalNodeError):
    code = "idempotency_conflict"


class DriverUnavailable(CapabilityDenied):
    code = "driver_unavailable"


class LedgerIntegrityError(LocalNodeError):
    code = "ledger_integrity_error"


class ProcessPolicyError(CapabilityDenied):
    code = "process_policy_denied"
