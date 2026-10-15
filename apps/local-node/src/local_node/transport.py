"""Authenticated, outbound-only control-channel seam for a Local Node.

There is deliberately no server or listener in this module.  A device polls an
explicit HTTPS endpoint (or an injected WSS adapter), reports capability truth,
    claims platform-signed actions, and uploads an ordered receipt outbox. Raw
    results are retained only for bounded read-only file claims until receipt
    acknowledgement. Transport authentication never replaces action-envelope signature,
expiry, nonce, device, capability, grant, or local-approval checks.
"""

from __future__ import annotations

import hashlib
import json
import math
import ssl
import threading
import time
import urllib.error
import urllib.request
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Callable, Mapping, Protocol, cast, runtime_checkable

from .credentials import CredentialStorageStatus, SecureCredentialStore
from .doctor import DoctorReport
from .errors import BoundaryViolation, CapabilityDenied, LocalNodeError, PairingError
from .identity import DeviceCredential, DeviceIdentity
from .models import (
    ActionContext,
    ActionStatus,
    ApprovalProof,
    PlatformSignatureVerifier,
    canonical_json,
)
from .outbox import OutboxResultTooLarge, ReceiptOutbox
from .service import LocalNodeRuntime, OutboundControlPlane


PROTOCOL_VERSION = "ai-platform.local-node.v1"
_MAX_RESPONSE_BYTES = 1024 * 1024
_MAX_COMMANDS = 100


class TransportUnavailable(LocalNodeError):
    code = "transport_unavailable"


class TransportProtocolError(LocalNodeError):
    code = "transport_protocol_error"


@dataclass(frozen=True, slots=True)
class PairingRedemption:
    challenge_id: str
    user_code: str = field(repr=False)
    device_id: str
    proof_algorithm: str
    proof_public_key: str = field(repr=False)
    device_proof: str = field(repr=False)
    display_name: str
    platform: str
    node_version: str
    capability_claims: tuple[str, ...]
    permission_snapshot_digest: str
    protocol_version: str = PROTOCOL_VERSION


@dataclass(frozen=True, slots=True)
class ExchangeRequest:
    device_id: str
    doctor: Mapping[str, Any]
    receipts: tuple[Mapping[str, Any], ...]
    sent_at: float
    protocol_version: str = PROTOCOL_VERSION

    def as_dict(self) -> dict[str, Any]:
        return {
            "protocol_version": self.protocol_version,
            "kind": "heartbeat",
            "device_id": self.device_id,
            "doctor": dict(self.doctor),
            "receipts": [dict(receipt) for receipt in self.receipts],
            "sent_at": self.sent_at,
        }


@dataclass(frozen=True, slots=True)
class ExchangeResponse:
    accepted_through_sequence: int
    commands: tuple[Mapping[str, Any], ...] = ()
    protocol_version: str = PROTOCOL_VERSION


@runtime_checkable
class OutboundTransport(Protocol):
    """Injectable HTTPS/WSS client contract; never a bind/listen contract."""

    @property
    def kind(self) -> str: ...

    def redeem_pairing(
        self,
        *,
        endpoint: OutboundControlPlane,
        redemption: PairingRedemption,
    ) -> DeviceCredential: ...

    def exchange(
        self,
        *,
        endpoint: OutboundControlPlane,
        credential: DeviceCredential,
        request: ExchangeRequest,
    ) -> ExchangeResponse: ...

    def close(self) -> None: ...


@runtime_checkable
class PairingProofSigner(Protocol):
    """Non-exportable asymmetric signer injected by the native companion.

    The server can verify the public key and challenge signature without ever
    learning the device private key.  The current symmetric DeviceIdentity key
    is intentionally not treated as remotely verifiable pairing proof.
    """

    @property
    def algorithm(self) -> str: ...

    @property
    def public_key(self) -> str: ...

    def sign(self, payload: bytes) -> str: ...


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):  # type: ignore[no-untyped-def]
        del req, fp, code, msg, headers, newurl
        raise TransportProtocolError("control-plane redirects are refused")


class HttpsJsonTransport:
    """Dependency-free HTTPS POST adapter with system CA verification.

    WSS endpoints intentionally require a separately injected adapter.  The
    stdlib has no WebSocket client and this module does not silently downgrade
    WSS to HTTPS or TLS to plaintext.
    """

    kind = "https"

    def __init__(
        self,
        *,
        timeout_seconds: float = 20,
        ssl_context: ssl.SSLContext | None = None,
        opener: urllib.request.OpenerDirector | None = None,
    ) -> None:
        if timeout_seconds <= 0 or timeout_seconds > 120:
            raise BoundaryViolation("transport timeout is invalid")
        self.timeout_seconds = timeout_seconds
        if opener is None:
            context = ssl.create_default_context() if ssl_context is None else ssl_context
            opener = urllib.request.build_opener(
                _NoRedirect(), urllib.request.HTTPSHandler(context=context)
            )
        self._opener = opener

    @staticmethod
    def _require_https(endpoint: OutboundControlPlane) -> None:
        endpoint.validate()
        if not endpoint.url.startswith("https://"):
            raise TransportUnavailable(
                "WSS endpoint requires an explicitly configured WebSocket adapter"
            )

    def _post(
        self,
        endpoint: OutboundControlPlane,
        payload: Mapping[str, Any],
        *,
        credential: DeviceCredential | None,
    ) -> Mapping[str, Any]:
        self._require_https(endpoint)
        try:
            body = json.dumps(
                payload,
                allow_nan=False,
                ensure_ascii=False,
                sort_keys=True,
                separators=(",", ":"),
            ).encode("utf-8")
        except (TypeError, ValueError) as exc:
            raise TransportProtocolError("outbound control message is not finite JSON") from exc
        headers = {
            "Accept": "application/json",
            "Content-Type": "application/json",
            "User-Agent": "AI-Platform-Local-Node/0.1",
        }
        if credential is not None:
            # The credential is kept exclusively in the request header.  It is
            # never copied into JSON, argv, environment variables, or errors.
            headers["Authorization"] = f"Device {credential.credential}"
        request = urllib.request.Request(
            endpoint.url,
            data=body,
            headers=headers,
            method="POST",
        )
        try:
            with self._opener.open(request, timeout=self.timeout_seconds) as response:
                if response.status != 200:
                    raise TransportProtocolError(
                        f"control plane returned HTTP {int(response.status)}"
                    )
                content_type = response.headers.get_content_type()
                if content_type != "application/json":
                    raise TransportProtocolError("control plane returned a non-JSON response")
                raw = response.read(_MAX_RESPONSE_BYTES + 1)
        except TransportProtocolError:
            raise
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            raise TransportUnavailable("outbound control-plane request failed") from exc
        if len(raw) > _MAX_RESPONSE_BYTES:
            raise TransportProtocolError("control-plane response exceeds one MiB")
        try:
            decoded = json.loads(raw)
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise TransportProtocolError("control plane returned invalid JSON") from exc
        if not isinstance(decoded, dict):
            raise TransportProtocolError("control-plane response must be an object")
        return cast(Mapping[str, Any], decoded)

    def redeem_pairing(
        self,
        *,
        endpoint: OutboundControlPlane,
        redemption: PairingRedemption,
    ) -> DeviceCredential:
        result = self._post(
            endpoint,
            {
                "protocol_version": redemption.protocol_version,
                "kind": "pairing_redeem",
                "challenge_id": redemption.challenge_id,
                "user_code": redemption.user_code,
                "device_id": redemption.device_id,
                "proof_algorithm": redemption.proof_algorithm,
                "proof_public_key": redemption.proof_public_key,
                "device_proof": redemption.device_proof,
                "display_name": redemption.display_name,
                "platform": redemption.platform,
                "node_version": redemption.node_version,
                "capability_claims": list(redemption.capability_claims),
                "permission_snapshot_digest": redemption.permission_snapshot_digest,
            },
            credential=None,
        )
        if set(result) != {"protocol_version", "device_id", "credential", "expires_at"}:
            raise TransportProtocolError("pairing response has an unexpected shape")
        if result.get("protocol_version") != PROTOCOL_VERSION:
            raise TransportProtocolError("pairing protocol version is incompatible")
        device_id = _required_string(result.get("device_id"), "paired device id")
        credential = _required_string(result.get("credential"), "device credential", secret=True)
        expires_at = _required_time(result.get("expires_at"), "device credential expiry")
        if expires_at <= time.time():
            raise PairingError("control plane returned an expired device credential")
        return DeviceCredential(device_id, credential, expires_at)

    def exchange(
        self,
        *,
        endpoint: OutboundControlPlane,
        credential: DeviceCredential,
        request: ExchangeRequest,
    ) -> ExchangeResponse:
        result = self._post(endpoint, request.as_dict(), credential=credential)
        if set(result) != {
            "protocol_version",
            "accepted_through_sequence",
            "commands",
        }:
            raise TransportProtocolError("exchange response has an unexpected shape")
        if result.get("protocol_version") != PROTOCOL_VERSION:
            raise TransportProtocolError("control-channel protocol version is incompatible")
        accepted = result.get("accepted_through_sequence")
        if isinstance(accepted, bool) or not isinstance(accepted, int) or accepted < 0:
            raise TransportProtocolError("receipt acknowledgement is invalid")
        commands = result.get("commands")
        if not isinstance(commands, list) or len(commands) > _MAX_COMMANDS:
            raise TransportProtocolError("control command batch is invalid")
        if any(not isinstance(command, dict) for command in commands):
            raise TransportProtocolError("control command must be an object")
        return ExchangeResponse(accepted, tuple(commands))

    def close(self) -> None:
        return None


@dataclass(frozen=True, slots=True)
class DispatchOutcome:
    action_id: str
    status: ActionStatus
    result: Mapping[str, Any] = field(default_factory=dict)
    error_code: str | None = None


@runtime_checkable
class DeviceActionDispatcher(Protocol):
    """Canonical local capability broker adapter, not a planner or agent loop."""

    def connect(self) -> None: ...

    def dispatch(
        self,
        *,
        action: ActionContext,
        normalized_arguments: Mapping[str, Any],
    ) -> DispatchOutcome: ...

    def cancel(self, action_id: str) -> DispatchOutcome: ...

    def emergency_stop(self) -> tuple[str, ...]: ...

    def disconnect(self) -> tuple[str, ...]: ...


class LocalActionRouter:
    """Exact-capability router over existing Local Node services.

    Handlers are injected from the companion composition root.  An absent
    handler is a hard denial; this class never invents a shell, file, or GUI
    fallback.
    """

    def __init__(
        self,
        runtime: LocalNodeRuntime,
        handlers: Mapping[str, Callable[[ActionContext, Mapping[str, Any]], DispatchOutcome]],
    ) -> None:
        self.runtime = runtime
        self._handlers = dict(handlers)

    def connect(self) -> None:
        self.runtime.connect()

    def dispatch(
        self,
        *,
        action: ActionContext,
        normalized_arguments: Mapping[str, Any],
    ) -> DispatchOutcome:
        handler = self._handlers.get(action.capability)
        if handler is None:
            raise CapabilityDenied("claimed capability has no configured local handler")
        outcome = handler(action, normalized_arguments)
        if outcome.action_id != action.action_id:
            raise TransportProtocolError("local handler returned another action identity")
        return outcome

    def cancel(self, action_id: str) -> DispatchOutcome:
        cancelled = self.runtime.process_runner.cancel(action_id)
        return DispatchOutcome(
            action_id,
            ActionStatus.CANCELLED if cancelled else ActionStatus.FAILED,
            {},
            None if cancelled else "action_not_running",
        )

    def emergency_stop(self) -> tuple[str, ...]:
        action_ids = set(self.runtime.process_runner.cancel_all())
        self.runtime.computer.emergency_stop()
        return tuple(sorted(action_ids))

    def disconnect(self) -> tuple[str, ...]:
        return tuple(self.runtime.disconnect())


class ClaimReplayGuard:
    """Durably consume command IDs and nonces before local dispatch."""

    def __init__(self, path: Path) -> None:
        import os
        import sqlite3

        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        self._lock = threading.RLock()
        self._db = sqlite3.connect(path, check_same_thread=False, isolation_level=None)
        os.chmod(path, 0o600)
        self._db.execute("PRAGMA journal_mode=WAL")
        self._db.execute("PRAGMA synchronous=FULL")
        self._db.execute(
            """
            CREATE TABLE IF NOT EXISTS consumed_commands (
              command_id TEXT PRIMARY KEY,
              nonce TEXT UNIQUE NOT NULL,
              payload_digest TEXT NOT NULL,
              consumed_at REAL NOT NULL
            )
            """
        )

    def reserve(self, *, command_id: str, nonce: str, payload_digest: str) -> bool:
        command_id = _required_string(command_id, "command id")
        nonce = _required_string(nonce, "command nonce")
        with self._lock:
            self._db.execute("BEGIN IMMEDIATE")
            try:
                by_id = self._db.execute(
                    "SELECT command_id,nonce,payload_digest FROM consumed_commands "
                    "WHERE command_id=?",
                    (command_id,),
                ).fetchone()
                by_nonce = self._db.execute(
                    "SELECT command_id,nonce,payload_digest FROM consumed_commands WHERE nonce=?",
                    (nonce,),
                ).fetchone()
                existing = by_id or by_nonce
                if existing is not None:
                    if existing != (command_id, nonce, payload_digest):
                        raise CapabilityDenied("control command id or nonce was replayed")
                    self._db.execute("COMMIT")
                    return False
                self._db.execute(
                    "INSERT INTO consumed_commands VALUES(?,?,?,?)",
                    (command_id, nonce, payload_digest, time.time()),
                )
                self._db.execute("COMMIT")
                return True
            except Exception:
                self._db.execute("ROLLBACK")
                raise

    def close(self) -> None:
        self._db.close()


@dataclass(frozen=True, slots=True)
class TransportDoctorReport:
    status: str
    endpoint: str
    transport: str
    credential: str
    identity_credential_storage: str
    outbound_credential_storage: str
    platform_signature_verifier: str
    pairing_proof_signer: str
    runtime: Mapping[str, Any]
    reason: str | None = None

    def as_dict(self) -> dict[str, Any]:
        return asdict(self)


class DeviceOutboundRunner:
    """One outbound device session; construction alone starts no I/O or thread."""

    def __init__(
        self,
        *,
        endpoint: OutboundControlPlane,
        identity: DeviceIdentity,
        credential: DeviceCredential | None,
        credential_store: SecureCredentialStore | None,
        credential_reference: str | None,
        pairing_signer: PairingProofSigner | None,
        transport: OutboundTransport | None,
        platform_signature_verifier: PlatformSignatureVerifier | None,
        dispatcher: DeviceActionDispatcher,
        doctor: Callable[[], DoctorReport | Mapping[str, Any]],
        outbox: ReceiptOutbox,
        replay_guard: ClaimReplayGuard,
        allow_test_credentials: bool = False,
    ) -> None:
        endpoint.validate()
        self.endpoint = endpoint
        self.identity = identity
        self.credential_store = credential_store
        self.credential_reference = credential_reference
        self.pairing_signer = pairing_signer
        self.transport = transport
        self.platform_signature_verifier = platform_signature_verifier
        self.dispatcher = dispatcher
        self.doctor_provider = doctor
        self.outbox = outbox
        self.replay_guard = replay_guard
        self.allow_test_credentials = allow_test_credentials
        self.credential = credential
        if (
            self.credential is None
            and self.credential_store is not None
            and self.credential_reference is not None
            and _storage_is_allowed(
                self.credential_store,
                allow_test_credentials=self.allow_test_credentials,
            )
        ):
            self.credential = load_persisted_credential(
                self.credential_store,
                self.credential_reference,
                expected_device_id=self.identity.device_id,
            )
        self._stopped = threading.Event()

    def doctor(self) -> TransportDoctorReport:
        endpoint_status = "ready"
        reason: str | None = None
        try:
            self.endpoint.validate()
        except BoundaryViolation:
            endpoint_status = "denied"
            reason = "outbound endpoint is invalid"
        transport_status = "ready" if self.transport is not None else "unavailable"
        if self.transport is None and reason is None:
            reason = "outbound transport adapter is unavailable"
        credential_status = "unavailable"
        if self.credential is not None:
            credential_status = (
                "ready"
                if self.credential.device_id == self.identity.device_id
                and bool(self.credential.credential)
                and time.time() < self.credential.expires_at
                else "expired_or_invalid"
            )
        if credential_status != "ready" and reason is None:
            reason = "paired device credential is unavailable or invalid"
        identity_storage_status = self.identity.credential_storage.value
        outbound_storage_status = (
            CredentialStorageStatus.UNAVAILABLE.value
            if self.credential_store is None
            else self.credential_store.status.value
        )
        storage_ready = (
            self.credential_store is not None
            and self.credential_store.status is CredentialStorageStatus.READY
        )
        if not storage_ready and not (
            self.allow_test_credentials
            and self.credential_store is not None
            and self.credential_store.status is CredentialStorageStatus.TEST_ONLY_INSECURE
        ):
            if reason is None:
                reason = "native secure credential storage is unavailable"
        verifier_status = "ready" if self.platform_signature_verifier is not None else "unavailable"
        if verifier_status != "ready" and reason is None:
            reason = "trusted platform signature verifier is unavailable"
        pairing_status = "ready" if self.pairing_signer is not None else "unavailable"
        runtime_report = self.doctor_provider()
        runtime = (
            runtime_report.as_dict()
            if isinstance(runtime_report, DoctorReport)
            else dict(runtime_report)
        )
        local_approval_status = str(runtime.get("trusted_local_approval") or "unavailable")
        if local_approval_status != "ready" and reason is None:
            reason = "trusted local approval verifier is unavailable"
        ready = all(
            (
                endpoint_status == "ready",
                transport_status == "ready",
                credential_status == "ready",
                storage_ready
                or (
                    self.allow_test_credentials
                    and self.credential_store is not None
                    and self.credential_store.status is CredentialStorageStatus.TEST_ONLY_INSECURE
                ),
                verifier_status == "ready",
                local_approval_status == "ready",
            )
        )
        return TransportDoctorReport(
            "ready" if ready else "unavailable",
            endpoint_status,
            transport_status,
            credential_status,
            identity_storage_status,
            outbound_storage_status,
            verifier_status,
            pairing_status,
            runtime,
            reason,
        )

    def _require_ready(self) -> DeviceCredential:
        report = self.doctor()
        if report.status != "ready" or self.credential is None:
            raise TransportUnavailable(report.reason or "outbound runner is unavailable")
        return self.credential

    def redeem_pairing(
        self,
        *,
        challenge_id: str,
        user_code: str,
        display_name: str,
        platform: str,
        node_version: str,
        capability_claims: tuple[str, ...],
        permission_snapshot_digest: str,
    ) -> DeviceCredential:
        if self.transport is None:
            raise TransportUnavailable("outbound transport adapter is unavailable")
        if self.pairing_signer is None:
            raise PairingError("native asymmetric pairing signer is unavailable")
        if self.credential_store is None or self.credential_reference is None:
            raise PairingError("secure outbound credential storage is unavailable")
        if not _storage_is_allowed(
            self.credential_store,
            allow_test_credentials=self.allow_test_credentials,
        ):
            raise PairingError("secure outbound credential storage is unavailable")
        challenge_id = _required_string(challenge_id, "pairing challenge id")
        user_code = _required_string(user_code, "pairing user code", secret=True)
        display_name = _required_string(display_name, "device display name", max_length=80)
        if platform not in {"macos", "windows", "linux"}:
            raise PairingError("device platform is unsupported")
        node_version = _required_string(node_version, "node version", max_length=64)
        if (
            not capability_claims
            or len(capability_claims) > 32
            or len(set(capability_claims)) != len(capability_claims)
        ):
            raise PairingError("device capability claims are invalid")
        capability_claims = tuple(sorted(capability_claims))
        if len(permission_snapshot_digest) != 64 or any(
            character not in "0123456789abcdef" for character in permission_snapshot_digest
        ):
            raise PairingError("permission snapshot digest is invalid")
        if self.credential_store.load(self.credential_reference) is not None:
            raise PairingError("outbound credential reference is already occupied")
        material = canonical_json(
            {
                "redemption_digest": pairing_redemption_digest(
                    challenge_id=challenge_id,
                    user_code=user_code,
                    device_id=self.identity.device_id,
                    proof_algorithm=self.pairing_signer.algorithm,
                    proof_public_key=self.pairing_signer.public_key,
                    display_name=display_name,
                    platform=platform,
                    node_version=node_version,
                    capability_claims=capability_claims,
                    permission_snapshot_digest=permission_snapshot_digest,
                )
            }
        ).encode("utf-8")
        credential = self.transport.redeem_pairing(
            endpoint=self.endpoint,
            redemption=PairingRedemption(
                challenge_id,
                user_code,
                self.identity.device_id,
                self.pairing_signer.algorithm,
                self.pairing_signer.public_key,
                self.pairing_signer.sign(material),
                display_name,
                platform,
                node_version,
                capability_claims,
                permission_snapshot_digest,
            ),
        )
        if credential.device_id != self.identity.device_id:
            raise PairingError("paired credential belongs to another device")
        encoded = canonical_json(
            {
                "protocol_version": PROTOCOL_VERSION,
                "device_id": credential.device_id,
                "credential": credential.credential,
                "expires_at": credential.expires_at,
            }
        ).encode("utf-8")
        self.credential_store.store(self.credential_reference, encoded)
        self.credential = credential
        return credential

    def run_once(self) -> int:
        credential = self._require_ready()
        assert self.transport is not None
        self.dispatcher.connect()
        pending = self.outbox.pending()
        request = ExchangeRequest(
            device_id=self.identity.device_id,
            doctor=self.doctor().runtime,
            receipts=tuple(event.as_dict(include_result=True) for event in pending),
            sent_at=time.time(),
        )
        try:
            response = self.transport.exchange(
                endpoint=self.endpoint,
                credential=credential,
                request=request,
            )
            if response.protocol_version != PROTOCOL_VERSION:
                raise TransportProtocolError("control-channel protocol version is incompatible")
            if pending:
                first = pending[0].sequence
                last = pending[-1].sequence
                if not first - 1 <= response.accepted_through_sequence <= last:
                    raise TransportProtocolError(
                        "control plane acknowledged outside the transmitted receipt range"
                    )
            elif response.accepted_through_sequence != self.outbox.acked_through:
                raise TransportProtocolError("control plane acknowledged unsent receipts")
            self.outbox.acknowledge(response.accepted_through_sequence)
            processed = 0
            for command in response.commands:
                self._process_command(command)
                processed += 1
            return processed
        except Exception:
            for action_id in self.dispatcher.disconnect():
                self._append_terminal(
                    command_id="disconnect",
                    action_id=action_id,
                    status=ActionStatus.UNKNOWN,
                    result={"replay_allowed": False},
                    error_code="transport_disconnected",
                )
            raise

    def run_forever(self, *, heartbeat_seconds: float = 5) -> None:
        if heartbeat_seconds < 0.25 or heartbeat_seconds > 300:
            raise BoundaryViolation("heartbeat interval is invalid")
        self._stopped.clear()
        while not self._stopped.is_set():
            self.run_once()
            self._stopped.wait(heartbeat_seconds)

    def stop(self) -> None:
        self._stopped.set()
        self.dispatcher.emergency_stop()
        if self.transport is not None:
            self.transport.close()

    def _process_command(self, raw: Mapping[str, Any]) -> None:
        command = _strict_object(raw, "control command")
        kind = _required_string(command.get("kind"), "command kind")
        if kind == "claim":
            self._claim(command)
        elif kind in {"cancel", "emergency_stop"}:
            self._control(command, kind=kind)
        else:
            raise TransportProtocolError("unsupported control command")

    def _claim(self, command: Mapping[str, Any]) -> None:
        if set(command) != {"kind", "command_id", "action", "normalized_arguments"}:
            raise TransportProtocolError("claim command has an unexpected shape")
        command_id = _required_string(command.get("command_id"), "command id")
        arguments = _strict_object(command.get("normalized_arguments"), "normalized arguments")
        action = _parse_action_context(command.get("action"))
        if action.device_id != self.identity.device_id:
            raise CapabilityDenied("claimed action targets another device")
        # This validates identity, lifetime, argument digest and the platform
        # signature before the nonce is durably consumed or a handler is called.
        action.validate_payload(
            arguments,
            verifier=self.platform_signature_verifier,
        )
        action_digest = hashlib.sha256(action.canonical_signed_payload()).hexdigest()
        if not self.replay_guard.reserve(
            command_id=command_id,
            nonce=action.nonce,
            payload_digest=action_digest,
        ):
            return
        try:
            outcome = self.dispatcher.dispatch(
                action=action,
                normalized_arguments=arguments,
            )
        except Exception as exc:
            self._append_terminal(
                command_id=command_id,
                action_id=action.action_id,
                status=ActionStatus.UNKNOWN,
                result={"replay_allowed": False},
                error_code=getattr(exc, "code", "local_dispatch_unknown"),
            )
            raise
        self._append_terminal(
            command_id=command_id,
            action_id=outcome.action_id,
            status=outcome.status,
            result=outcome.result,
            error_code=outcome.error_code,
            retain_result=(
                outcome.status is ActionStatus.SUCCEEDED
                and action.operation
                in {"file.list", "file.read", "file.hash", "file.search", "file.watch"}
            ),
        )

    def _control(self, command: Mapping[str, Any], *, kind: str) -> None:
        required = {
            "kind",
            "command_id",
            "device_id",
            "nonce",
            "issued_at",
            "expires_at",
            "platform_key_id",
            "platform_signature",
        }
        if kind == "cancel":
            required.add("action_id")
        if set(command) != required:
            raise TransportProtocolError("control command has an unexpected shape")
        command_id = _required_string(command.get("command_id"), "command id")
        device_id = _required_string(command.get("device_id"), "command device id")
        nonce = _required_string(command.get("nonce"), "command nonce")
        key_id = _required_string(command.get("platform_key_id"), "platform key id")
        signature = _required_string(
            command.get("platform_signature"), "platform command signature", secret=True
        )
        issued_at = _required_time(command.get("issued_at"), "command issue time")
        expires_at = _required_time(command.get("expires_at"), "command expiry")
        if device_id != self.identity.device_id:
            raise CapabilityDenied("control command targets another device")
        now = time.time()
        if issued_at > now + 30 or expires_at <= issued_at or expires_at - issued_at > 600:
            raise CapabilityDenied("control command lifetime is invalid")
        if now >= expires_at:
            raise CapabilityDenied("control command expired")
        if self.platform_signature_verifier is None:
            raise CapabilityDenied("platform signature verifier is unavailable")
        signed = dict(command)
        del signed["platform_signature"]
        payload = canonical_json(signed).encode("utf-8")
        try:
            verified = self.platform_signature_verifier.verify(
                key_id=key_id,
                payload=payload,
                signature=signature,
            )
        except Exception as exc:
            raise CapabilityDenied("platform control signature verification failed") from exc
        if verified is not True:
            raise CapabilityDenied("platform control signature verification failed")
        if not self.replay_guard.reserve(
            command_id=command_id,
            nonce=nonce,
            payload_digest=hashlib.sha256(payload).hexdigest(),
        ):
            return
        if kind == "cancel":
            action_id = _required_string(command.get("action_id"), "cancel action id")
            outcome = self.dispatcher.cancel(action_id)
            self._append_terminal(
                command_id=command_id,
                action_id=action_id,
                status=outcome.status,
                result=outcome.result,
                error_code=outcome.error_code,
            )
            return
        stopped = self.dispatcher.emergency_stop()
        self.outbox.append(
            event_id=f"command.{command_id}",
            event_type="device.emergency_stopped",
            status=ActionStatus.CANCELLED,
            result={"stopped_action_ids": sorted(stopped)},
        )

    def _append_terminal(
        self,
        *,
        command_id: str,
        action_id: str,
        status: ActionStatus,
        result: Mapping[str, Any],
        error_code: str | None,
        retain_result: bool = False,
    ) -> None:
        try:
            self.outbox.append(
                event_id=f"command.{command_id}.{action_id}",
                action_id=action_id,
                event_type=f"action.{status.value}",
                status=status,
                result=result,
                retain_result=retain_result,
                error_code=error_code,
            )
        except OutboxResultTooLarge:
            # The command is already consumed. Persist a bounded outcome so
            # reconnect/retry can deliver it without executing the action again.
            # Only retained read-only results can safely be reported as failed;
            # an oversized side-effect success must remain explicitly unknown.
            if status is ActionStatus.SUCCEEDED:
                status = ActionStatus.FAILED if retain_result else ActionStatus.UNKNOWN
            self.outbox.append(
                event_id=f"command.{command_id}.{action_id}",
                action_id=action_id,
                event_type=f"action.{status.value}",
                status=status,
                result={"replay_allowed": False},
                error_code=OutboxResultTooLarge.code,
            )


def _required_string(
    value: object,
    field_name: str,
    *,
    secret: bool = False,
    max_length: int = 4096,
) -> str:
    if not isinstance(value, str) or not value or len(value) > max_length:
        qualifier = "secret " if secret else ""
        raise TransportProtocolError(f"{qualifier}{field_name} is invalid")
    if any(ord(character) < 0x20 for character in value):
        raise TransportProtocolError(f"{field_name} contains control characters")
    return value


def _required_time(value: object, field_name: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise TransportProtocolError(f"{field_name} is invalid")
    result = float(value)
    if not math.isfinite(result) or result <= 0:
        raise TransportProtocolError(f"{field_name} is invalid")
    return result


def _strict_object(value: object, field_name: str) -> dict[str, Any]:
    if not isinstance(value, dict) or any(not isinstance(key, str) for key in value):
        raise TransportProtocolError(f"{field_name} must be an object")
    try:
        encoded = json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
    except (TypeError, ValueError) as exc:
        raise TransportProtocolError(f"{field_name} must be finite JSON") from exc
    if len(encoded) > 65_536:
        raise TransportProtocolError(f"{field_name} exceeds 64 KiB")
    return cast(dict[str, Any], value)


def _parse_action_context(raw: object) -> ActionContext:
    value = _strict_object(raw, "signed action")
    required = {
        "action_id",
        "idempotency_key",
        "tenant_id",
        "user_id",
        "session_id",
        "run_id",
        "agent_id",
        "agent_version",
        "call_id",
        "device_id",
        "envelope_version",
        "capability",
        "tool_name",
        "operation",
        "capability_lease_id",
        "resource_refs",
        "arguments_digest",
        "target_snapshot_digest",
        "policy_snapshot_digest",
        "nonce",
        "issued_at",
        "expires_at",
        "platform_key_id",
        "platform_signature",
        "approval",
        "trace_context",
    }
    if set(value) != required:
        raise TransportProtocolError("signed action has an unexpected shape")
    envelope_version = value["envelope_version"]
    if isinstance(envelope_version, bool) or not isinstance(envelope_version, int):
        raise TransportProtocolError("signed action envelope version is invalid")
    resources = value["resource_refs"]
    if not isinstance(resources, list) or any(not isinstance(item, str) for item in resources):
        raise TransportProtocolError("signed action resource references are invalid")
    trace = value["trace_context"]
    if trace is not None and (
        not isinstance(trace, dict)
        or any(not isinstance(key, str) or not isinstance(item, str) for key, item in trace.items())
    ):
        raise TransportProtocolError("signed action trace context is invalid")
    approval_raw = value["approval"]
    approval: ApprovalProof | None = None
    if approval_raw is not None:
        approval_value = _strict_object(approval_raw, "approval proof")
        if set(approval_value) != {
            "approval_id",
            "action_id",
            "device_id",
            "arguments_digest",
            "target_snapshot_digest",
            "policy_snapshot_digest",
            "nonce",
            "expires_at",
            "local_signature",
        }:
            raise TransportProtocolError("approval proof has an unexpected shape")
        approval = ApprovalProof(
            _required_string(approval_value["approval_id"], "approval id"),
            _required_string(approval_value["action_id"], "approval action id"),
            _required_string(approval_value["device_id"], "approval device id"),
            _required_string(approval_value["arguments_digest"], "approval arguments digest"),
            _required_string(approval_value["target_snapshot_digest"], "approval target digest"),
            _required_string(approval_value["policy_snapshot_digest"], "approval policy digest"),
            _required_string(approval_value["nonce"], "approval nonce"),
            _required_time(approval_value["expires_at"], "approval expiry"),
            _required_string(
                approval_value["local_signature"],
                "trusted local approval signature",
                secret=True,
            ),
        )
    return ActionContext(
        action_id=_required_string(value["action_id"], "action id"),
        idempotency_key=_required_string(value["idempotency_key"], "idempotency key"),
        tenant_id=_required_string(value["tenant_id"], "tenant id"),
        user_id=_required_string(value["user_id"], "user id"),
        session_id=_required_string(value["session_id"], "session id"),
        run_id=_required_string(value["run_id"], "run id"),
        agent_id=_required_string(value["agent_id"], "agent id"),
        agent_version=_required_string(value["agent_version"], "agent version"),
        call_id=_required_string(value["call_id"], "call id"),
        device_id=_required_string(value["device_id"], "device id"),
        envelope_version=envelope_version,
        capability=_required_string(value["capability"], "capability"),
        tool_name=_required_string(value["tool_name"], "tool name"),
        operation=_required_string(value["operation"], "action operation"),
        capability_lease_id=_required_string(value["capability_lease_id"], "capability lease id"),
        resource_refs=tuple(resources),
        arguments_digest=_required_string(value["arguments_digest"], "arguments digest"),
        target_snapshot_digest=_required_string(
            value["target_snapshot_digest"], "target snapshot digest"
        ),
        policy_snapshot_digest=_required_string(
            value["policy_snapshot_digest"], "policy snapshot digest"
        ),
        nonce=_required_string(value["nonce"], "action nonce"),
        issued_at=_required_time(value["issued_at"], "action issue time"),
        expires_at=_required_time(value["expires_at"], "action expiry"),
        platform_key_id=_required_string(value["platform_key_id"], "platform key id"),
        platform_signature=_required_string(
            value["platform_signature"], "platform action signature", secret=True
        ),
        approval=approval,
        trace_context=None if trace is None else cast(Mapping[str, str], trace),
    )


def action_to_wire(action: ActionContext) -> dict[str, Any]:
    """Explicit wire helper for tests/control planes; signature is preserved."""
    value = asdict(action)
    value["resource_refs"] = list(action.resource_refs)
    value["trace_context"] = None if action.trace_context is None else dict(action.trace_context)
    return value


def pairing_redemption_digest(
    *,
    challenge_id: str,
    user_code: str,
    device_id: str,
    proof_algorithm: str,
    proof_public_key: str,
    display_name: str,
    platform: str,
    node_version: str,
    capability_claims: tuple[str, ...],
    permission_snapshot_digest: str,
) -> str:
    """Digest the exact pre-signature pairing body without retaining its code."""
    preproof = {
        "protocol_version": PROTOCOL_VERSION,
        "kind": "pairing_redeem",
        "challenge_id": challenge_id,
        "user_code_digest": hashlib.sha256(user_code.encode("utf-8")).hexdigest(),
        "device_id": device_id,
        "proof_algorithm": proof_algorithm,
        "proof_public_key": proof_public_key,
        "display_name": display_name,
        "platform": platform,
        "node_version": node_version,
        "capability_claims": sorted(capability_claims),
        "permission_snapshot_digest": permission_snapshot_digest,
    }
    return "sha256:" + hashlib.sha256(canonical_json(preproof).encode("utf-8")).hexdigest()


def _storage_is_allowed(
    store: SecureCredentialStore,
    *,
    allow_test_credentials: bool,
) -> bool:
    return store.status is CredentialStorageStatus.READY or (
        allow_test_credentials and store.status is CredentialStorageStatus.TEST_ONLY_INSECURE
    )


def load_persisted_credential(
    store: SecureCredentialStore,
    reference: str,
    *,
    expected_device_id: str,
) -> DeviceCredential | None:
    """Load one opaque server credential from an explicitly supplied store."""
    raw = store.load(reference)
    if raw is None:
        return None
    if len(raw) > 16 * 1024:
        raise PairingError("stored outbound credential record is invalid")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise PairingError("stored outbound credential record is invalid") from exc
    if not isinstance(value, dict) or set(value) != {
        "protocol_version",
        "device_id",
        "credential",
        "expires_at",
    }:
        raise PairingError("stored outbound credential record is invalid")
    if value.get("protocol_version") != PROTOCOL_VERSION:
        raise PairingError("stored outbound credential protocol is incompatible")
    device_id = _required_string(value.get("device_id"), "stored device id")
    if device_id != expected_device_id:
        raise PairingError("stored outbound credential belongs to another device")
    credential = _required_string(
        value.get("credential"),
        "stored outbound credential",
        secret=True,
    )
    expires_at = _required_time(value.get("expires_at"), "stored credential expiry")
    if time.time() >= expires_at:
        return None
    return DeviceCredential(device_id, credential, expires_at)
