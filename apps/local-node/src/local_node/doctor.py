"""Redaction-safe capability truth report for a trusted local UI."""

from __future__ import annotations

import os
import platform
from dataclasses import asdict, dataclass
from typing import Any

from .computer import ComputerDriver
from .grants import DirectoryGrantStore
from .identity import DeviceIdentity
from .ledger import ActionLedger
from .processes import ProcessPolicy
from .service import LocalServiceBinding, OutboundControlPlane


@dataclass(frozen=True, slots=True)
class DoctorReport:
    device_id: str | None
    secure_credential_storage: str
    trusted_local_approval: str
    operating_system: str
    ledger: str
    service_boundary: str
    control_plane: str
    directory_grants: int
    process_runner: str
    computer: dict[str, Any]

    def as_dict(self) -> dict[str, Any]:
        return asdict(self)


class CapabilityDoctor:
    def __init__(
        self,
        identity: DeviceIdentity | None,
        grants: DirectoryGrantStore,
        ledger: ActionLedger,
        process_policy: ProcessPolicy,
        computer: ComputerDriver,
        binding: LocalServiceBinding,
        control_plane: OutboundControlPlane | None,
    ) -> None:
        self.identity = identity
        self.grants = grants
        self.ledger = ledger
        self.process_policy = process_policy
        self.computer = computer
        self.binding = binding
        self.control_plane = control_plane

    def run(self) -> DoctorReport:
        ledger_status = "ready"
        try:
            self.ledger.verify_integrity()
            if not os.access(self.ledger.path.parent, os.W_OK):
                ledger_status = "denied"
        except Exception:
            ledger_status = "denied"
        boundary_status = "ready"
        try:
            self.binding.validate()
        except Exception:
            boundary_status = "denied"
        if self.control_plane is None:
            control_status = "needs_action"
        else:
            try:
                self.control_plane.validate()
                control_status = "ready"
            except Exception:
                control_status = "denied"
        # The canonical tool contract only permits deny/explicit-domain
        # network policies.  This dependency-free runner has no backend able to
        # enforce either policy, so an executable allowlist alone is never
        # advertised as a ready process capability.  ``inherit`` exists only as
        # an explicit local test seam and is not canonical production health.
        process_status = "needs_action" if not self.process_policy.allowed_executables else "unavailable"
        return DoctorReport(
            None if self.identity is None else self.identity.device_id,
            "unavailable" if self.identity is None else self.identity.credential_storage.value,
            "ready" if self.ledger.trusted_local_approval_verifier is not None else "unavailable",
            platform.system(),
            ledger_status,
            boundary_status,
            control_status,
            len(self.grants.active()),
            process_status,
            asdict(self.computer.doctor()),
        )
