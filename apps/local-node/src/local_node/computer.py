"""Provider-neutral Computer Use contract with fail-closed macOS health checks."""

from __future__ import annotations

import platform
import secrets
import threading
import time
from dataclasses import asdict, dataclass, field
from enum import StrEnum
from typing import Any, Callable, Mapping, Protocol

from .errors import CapabilityDenied, DriverUnavailable, StaleTargetError
from .ledger import ActionLedger
from .models import ActionContext, ActionStatus, digest_payload


class HealthStatus(StrEnum):
    READY = "ready"
    DENIED = "denied"
    NEEDS_ACTION = "needs_action"
    UNSUPPORTED = "unsupported"


@dataclass(frozen=True, slots=True)
class DriverHealth:
    status: HealthStatus
    platform: str
    driver: str
    accessibility: HealthStatus
    screen_recording: HealthStatus
    reason: str | None = None


@dataclass(frozen=True, slots=True)
class ComputerObservation:
    observation_id: str
    app_id: str
    window_id: str
    origin: str | None
    width: int
    height: int
    screenshot_ref: str | None
    accessibility_digest: str | None
    observed_at: float

    @property
    def digest(self) -> str:
        return digest_payload(asdict(self))


@dataclass(frozen=True, slots=True)
class ComputerAction:
    kind: str
    observation_id: str
    arguments: Mapping[str, Any] = field(default_factory=dict)


class ComputerBackend(Protocol):
    name: str

    def observe(self, app_id: str, window_id: str | None) -> ComputerObservation: ...

    def execute(self, action: ComputerAction) -> None: ...

    def stop(self) -> None: ...


class ComputerDriver(Protocol):
    def doctor(self) -> DriverHealth: ...

    def observe(self, app_id: str, window_id: str | None = None) -> ComputerObservation: ...

    def execute(self, action: ComputerAction) -> None: ...

    def stop(self) -> None: ...


class MacOSComputerDriver:
    """Safe adapter point for Hermes-style CUA/Accessibility backends.

    No subprocess or OS permission is inferred. Until a concrete backend and
    positive permission probes are injected, every operation is unavailable.
    """

    def __init__(
        self,
        backend: ComputerBackend | None = None,
        *,
        accessibility_probe: Callable[[], bool] | None = None,
        screen_recording_probe: Callable[[], bool] | None = None,
        platform_name: str | None = None,
    ) -> None:
        self.backend = backend
        self.accessibility_probe = accessibility_probe
        self.screen_recording_probe = screen_recording_probe
        self.platform_name = platform.system() if platform_name is None else platform_name

    def doctor(self) -> DriverHealth:
        if self.platform_name != "Darwin":
            return DriverHealth(
                HealthStatus.UNSUPPORTED,
                self.platform_name,
                "unavailable",
                HealthStatus.UNSUPPORTED,
                HealthStatus.UNSUPPORTED,
                "macOS Computer Use driver is only enabled on Darwin",
            )
        try:
            accessibility = (
                HealthStatus.NEEDS_ACTION
                if self.accessibility_probe is None
                else HealthStatus.READY
                if self.accessibility_probe()
                else HealthStatus.DENIED
            )
        except Exception:
            accessibility = HealthStatus.DENIED
        try:
            screen = (
                HealthStatus.NEEDS_ACTION
                if self.screen_recording_probe is None
                else HealthStatus.READY
                if self.screen_recording_probe()
                else HealthStatus.DENIED
            )
        except Exception:
            screen = HealthStatus.DENIED
        if self.backend is None:
            status = HealthStatus.NEEDS_ACTION
            reason = "no trusted CUA/Accessibility backend is configured"
            driver = "unavailable"
        elif accessibility is HealthStatus.READY and screen is HealthStatus.READY:
            status = HealthStatus.READY
            reason = None
            driver = self.backend.name
        else:
            status = (
                HealthStatus.DENIED
                if HealthStatus.DENIED in {accessibility, screen}
                else HealthStatus.NEEDS_ACTION
            )
            reason = "required macOS permissions are not ready"
            driver = self.backend.name
        return DriverHealth(status, self.platform_name, driver, accessibility, screen, reason)

    def _require_ready(self) -> ComputerBackend:
        health = self.doctor()
        if health.status is not HealthStatus.READY or self.backend is None:
            raise DriverUnavailable(health.reason or "computer driver unavailable")
        return self.backend

    def observe(self, app_id: str, window_id: str | None = None) -> ComputerObservation:
        return self._require_ready().observe(app_id, window_id)

    def execute(self, action: ComputerAction) -> None:
        self._require_ready().execute(action)

    def stop(self) -> None:
        if self.backend is not None:
            self.backend.stop()


@dataclass(frozen=True, slots=True)
class ComputerScope:
    allowed_apps: frozenset[str]
    allowed_origins: frozenset[str] = frozenset()


@dataclass(frozen=True, slots=True)
class ComputerLease:
    lease_id: str
    session_id: str
    app_id: str
    window_id: str | None
    expires_at: float
    token: str = field(repr=False)


@dataclass(frozen=True, slots=True)
class ComputerActionResult:
    action_id: str
    status: str
    before_observation_id: str
    after_observation_id: str
    after_digest: str

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "ComputerActionResult":
        return cls(**value)


class ComputerController:
    _SUPPORTED_ACTIONS = frozenset(
        {
            "click",
            "double_click",
            "type_text",
            "key_press",
            "scroll",
            "drag",
            "wait",
            "open_app",
            "focus_app",
        }
    )
    _CONTROL_CAPABILITIES = frozenset({"app.control"})

    def __init__(
        self,
        driver: ComputerDriver,
        scope: ComputerScope,
        ledger: ActionLedger,
    ) -> None:
        self.driver = driver
        self.scope = scope
        self.ledger = ledger
        self._lock = threading.RLock()
        self._lease: ComputerLease | None = None
        self._observation: ComputerObservation | None = None

    def acquire(
        self,
        app_id: str,
        *,
        session_id: str,
        window_id: str | None = None,
        ttl_seconds: int = 300,
    ) -> ComputerLease:
        if self.driver.doctor().status is not HealthStatus.READY:
            raise DriverUnavailable("computer driver or permissions are not ready")
        if app_id not in self.scope.allowed_apps:
            raise CapabilityDenied("application is outside the Computer Use scope")
        if not session_id or len(session_id) > 512:
            raise CapabilityDenied("Computer Use session identity is invalid")
        if ttl_seconds <= 0 or ttl_seconds > 3600:
            raise CapabilityDenied("invalid Computer Use lease lifetime")
        with self._lock:
            if self._lease is not None and time.time() < self._lease.expires_at:
                raise CapabilityDenied("another Computer Use lease is active")
            lease = ComputerLease(
                "lease_" + secrets.token_urlsafe(16),
                session_id,
                app_id,
                window_id,
                time.time() + ttl_seconds,
                secrets.token_urlsafe(32),
            )
            self._lease = lease
            self._observation = None
            return lease

    def _require_lease(self, token: str) -> ComputerLease:
        with self._lock:
            lease = self._lease
        if lease is None or time.time() >= lease.expires_at:
            raise CapabilityDenied("Computer Use lease is absent or expired")
        if not secrets.compare_digest(token, lease.token):
            raise CapabilityDenied("Computer Use lease token is invalid")
        return lease

    def observe(self, token: str) -> ComputerObservation:
        lease = self._require_lease(token)
        observation = self.driver.observe(lease.app_id, lease.window_id)
        if observation.app_id != lease.app_id:
            self.stop(token)
            raise StaleTargetError("focused application changed")
        if observation.origin is not None and self.scope.allowed_origins:
            if observation.origin not in self.scope.allowed_origins:
                self.stop(token)
                raise CapabilityDenied("browser origin left the allowed scope")
        with self._lock:
            self._observation = observation
        return observation

    def execute(
        self,
        token: str,
        computer_action: ComputerAction,
        action: ActionContext,
    ) -> ComputerActionResult:
        lease = self._require_lease(token)
        if computer_action.kind not in self._SUPPORTED_ACTIONS:
            raise CapabilityDenied("unsupported Computer Use action")
        # Provider primitives (click/type/scroll) are normalized under the
        # platform's app.control authority.  They are never promoted into
        # ad-hoc `computer.*` grants that would bypass the API capability
        # ceiling. Read-only app.observe/screen.observe remain separate paths
        # and cannot authorize input injection.
        if action.capability not in self._CONTROL_CAPABILITIES:
            raise CapabilityDenied("action envelope capability mismatch")
        if action.session_id != lease.session_id:
            raise CapabilityDenied("Computer Use lease belongs to another session")
        # The canonical control-plane contract exposes one app-control tool and
        # operation.  The concrete primitive is still signed because ``kind``
        # is part of the normalized argument payload validated below.  Keeping
        # the operation canonical avoids inventing an ungrantable capability or
        # allowing an unsigned dispatcher to reinterpret a batch item.
        if action.operation != "app.control":
            raise CapabilityDenied("action envelope operation mismatch")
        with self._lock:
            before = self._observation
        if before is None or before.observation_id != computer_action.observation_id:
            raise StaleTargetError("Computer Use action is based on a stale observation")
        if before.app_id != lease.app_id:
            raise StaleTargetError("Computer Use application changed")
        payload = {
            "lease_id": lease.lease_id,
            "kind": computer_action.kind,
            "observation_id": computer_action.observation_id,
            "arguments": dict(computer_action.arguments),
        }
        action.validate_payload(
            payload,
            verifier=self.ledger.platform_signature_verifier,
            capability_lease_id=lease.lease_id,
            resource_refs=(lease.lease_id, before.app_id, before.window_id),
        )
        begin = self.ledger.begin(action)
        if not begin.created:
            if begin.record.result is None:
                raise CapabilityDenied("matching Computer Use action is already running")
            return ComputerActionResult.from_dict(begin.record.result)
        dispatched = False
        try:
            # Every input/action primitive is medium risk or above in this first
            # implementation. Missing callbacks/proofs always fail closed.
            self.ledger.mark_awaiting_approval(action.action_id)
            action.require_approval(
                target_snapshot_digest=before.digest,
                verifier=self.ledger.trusted_local_approval_verifier,
            )
            self.ledger.mark_dispatched(action.action_id)
            self.ledger.mark_running(action.action_id)
            # Driver calls can fail after partially injecting input. From this
            # point onward, only a fresh observation can prove a terminal result.
            dispatched = True
            self.driver.execute(computer_action)
            after = self.observe(token)
            self.ledger.mark_observed(action.action_id)
            result = ComputerActionResult(
                action.action_id,
                ActionStatus.SUCCEEDED.value,
                before.observation_id,
                after.observation_id,
                after.digest,
            )
            self.ledger.finish(action.action_id, ActionStatus.SUCCEEDED, asdict(result))
            return result
        except Exception as exc:
            terminal = ActionStatus.UNKNOWN if dispatched else ActionStatus.FAILED
            self.ledger.finish(
                action.action_id,
                terminal,
                {
                    "error_code": getattr(exc, "code", "computer_action_failed"),
                    "read_back_required": dispatched,
                    "replay_allowed": False,
                },
            )
            raise

    def stop(self, token: str | None = None) -> bool:
        with self._lock:
            lease = self._lease
            if lease is None:
                return False
            if token is not None and not secrets.compare_digest(token, lease.token):
                raise CapabilityDenied("Computer Use lease token is invalid")
            self._lease = None
            self._observation = None
        self.driver.stop()
        return True

    def emergency_stop(self) -> bool:
        """Trusted local UI entrypoint; deliberately does not require Web state."""
        return self.stop(None)
