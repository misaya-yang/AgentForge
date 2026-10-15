"""Non-secret Local Node diagnostics.

This CLI deliberately has no credential, token, key, or pairing-secret flags.
The packaged native companion must inject its secure credential store, trusted
platform verifier, Computer Use backend, and outbound runner in process.  The
standalone command performs no network I/O and starts no listener.
"""

from __future__ import annotations

import argparse
import json
import platform
from typing import Never, Sequence

from .computer import MacOSComputerDriver
from .errors import BoundaryViolation
from .service import OutboundControlPlane


class SecretSafeArgumentParser(argparse.ArgumentParser):
    """Argparse error path that never echoes unknown argument values."""

    def error(self, message: str) -> Never:
        del message
        self.print_usage()
        self.exit(2, f"{self.prog}: error: invalid arguments\n")


def build_parser() -> argparse.ArgumentParser:
    parser = SecretSafeArgumentParser(
        prog="local-node",
        description="AI--Platfform Local Node diagnostics (network is not started)",
    )
    subcommands = parser.add_subparsers(
        dest="command",
        required=True,
        parser_class=SecretSafeArgumentParser,
    )
    doctor = subcommands.add_parser(
        "doctor",
        help="report truthful local readiness without reading credentials",
    )
    doctor.add_argument(
        "--endpoint",
        help="optional HTTPS/WSS control-plane URL to validate; credentials are forbidden",
    )
    return parser


def _doctor(endpoint: str | None) -> tuple[dict[str, object], int]:
    endpoint_status = "needs_action"
    transport_status = "unavailable"
    reason = "outbound endpoint is not configured"
    if endpoint is not None:
        try:
            control_plane = OutboundControlPlane(endpoint)
            control_plane.validate()
            endpoint_status = "ready"
            if endpoint.startswith("https://"):
                transport_status = "available_not_started"
                reason = "secure credential store and platform verifier require native injection"
            else:
                transport_status = "needs_adapter"
                reason = "WSS requires an explicitly injected WebSocket adapter"
        except BoundaryViolation:
            endpoint_status = "denied"
            reason = "outbound endpoint is invalid"
    computer = MacOSComputerDriver().doctor()
    report: dict[str, object] = {
        "status": "unavailable",
        "operating_system": platform.system(),
        "network_started": False,
        "listener": "disabled",
        "endpoint": endpoint_status,
        "transport": transport_status,
        "secure_credential_storage": "unavailable",
        "platform_signature_verifier": "unavailable",
        "trusted_local_approval": "unavailable",
        "computer": {
            "status": computer.status.value,
            "driver": computer.driver,
            "accessibility": computer.accessibility.value,
            "screen_recording": computer.screen_recording.value,
        },
        "reason": reason,
    }
    return report, 2


def main(argv: Sequence[str] | None = None) -> int:
    arguments = build_parser().parse_args(argv)
    if arguments.command == "doctor":
        report, exit_code = _doctor(arguments.endpoint)
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
        return exit_code
    raise AssertionError("argparse accepted an unknown command")


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
