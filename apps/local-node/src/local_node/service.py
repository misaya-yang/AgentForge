"""Local service boundary and disconnect behavior.

The package intentionally does not start a listener. A future HTTP/WebSocket
adapter must validate its bind and control-plane endpoint through these types.
"""

from __future__ import annotations

import ipaddress
import threading
from dataclasses import dataclass
from urllib.parse import urlsplit

from .computer import ComputerController
from .errors import BoundaryViolation, CapabilityDenied
from .ledger import ActionLedger
from .processes import ProcessRunner


@dataclass(frozen=True, slots=True)
class LocalServiceBinding:
    host: str = "127.0.0.1"
    port: int = 8765

    def validate(self) -> None:
        try:
            address = ipaddress.ip_address(self.host)
        except ValueError as exc:
            raise BoundaryViolation("listener host must be a literal loopback address") from exc
        if not address.is_loopback:
            raise BoundaryViolation("Local Node listener must be loopback-only")
        if not 1 <= self.port <= 65535:
            raise BoundaryViolation("invalid listener port")


@dataclass(frozen=True, slots=True)
class OutboundControlPlane:
    url: str

    def validate(self) -> None:
        parsed = urlsplit(self.url)
        if parsed.scheme not in {"https", "wss"}:
            raise BoundaryViolation("control-plane connection must use HTTPS or WSS")
        if not parsed.hostname or parsed.username or parsed.password:
            raise BoundaryViolation("control-plane URL must not embed credentials")
        if parsed.fragment or parsed.query:
            raise BoundaryViolation("control-plane URL cannot contain a query or fragment")


class LocalNodeRuntime:
    def __init__(
        self,
        ledger: ActionLedger,
        process_runner: ProcessRunner,
        computer: ComputerController,
        *,
        binding: LocalServiceBinding | None = None,
        control_plane: OutboundControlPlane | None = None,
    ) -> None:
        self.ledger = ledger
        self.process_runner = process_runner
        self.computer = computer
        self.binding = binding or LocalServiceBinding()
        self.binding.validate()
        self.control_plane = control_plane
        if control_plane is not None:
            control_plane.validate()
        self._online = False
        self._lock = threading.RLock()

    @property
    def online(self) -> bool:
        with self._lock:
            return self._online

    def connect(self) -> None:
        if self.control_plane is None:
            raise BoundaryViolation("no outbound control plane configured")
        self.control_plane.validate()
        with self._lock:
            self._online = True

    def assert_online(self) -> None:
        if not self.online:
            raise CapabilityDenied("Local Node is offline")

    def disconnect(self) -> tuple[str, ...]:
        """Stop local control and mark unresolved side effects unknown.

        Unknown actions are not replayable: a repeated idempotency key returns
        that terminal record and must be reconciled through read-back.
        """
        with self._lock:
            self._online = False
        self.computer.emergency_stop()
        cancelling = frozenset(self.process_runner.cancel_all())
        return self.ledger.interrupt_running(exclude=cancelling)
