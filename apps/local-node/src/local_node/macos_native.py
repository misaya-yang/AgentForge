"""Explicit macOS Accessibility/CoreGraphics backend for Computer Use.

The backend wraps the bundled native helper as a one-request subprocess.  It
does not listen on a port, inherit the shell environment, launch a model loop,
or infer OS permission.  Construction requires an absolute trusted helper path
and an explicit app/origin/session scope.
"""

from __future__ import annotations

import hashlib
import base64
import json
import math
import os
import re
import secrets
import signal
import stat
import subprocess
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping
from urllib.parse import urlsplit

from .computer import ComputerAction, ComputerObservation
from .errors import CapabilityDenied, DriverUnavailable, StaleTargetError
from .models import canonical_json, digest_payload


_MAX_HELPER_OUTPUT = 1024 * 1024
_BUNDLE_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9.-]{1,254}$")
_PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
_SECRET_MARKERS = (
    "-----begin private key-----",
    "bearer ",
    "sk-",
    "ghp_",
    "secret_canary",
    "secret-canary",
)
_SAFE_ENVIRONMENT = {
    "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
    "LANG": "C",
    "LC_ALL": "C",
}


class NativeComputerUseDenied(CapabilityDenied):
    code = "native_computer_use_denied"


class NativeTakeoverRequired(CapabilityDenied):
    code = "takeover_required"


@dataclass(frozen=True, slots=True)
class NativeObservationDetails:
    observation_id: str
    session_id: str
    app_id: str
    window_id: str
    window_title: str
    origin: str | None
    frontmost: bool
    window_x: float
    window_y: float
    accessibility_lines: tuple[str, ...]
    risk_flags: tuple[str, ...]
    screenshot_path: Path | None
    screenshot_sha256: str | None

    @property
    def accessibility_text(self) -> str:
        return "\n".join(self.accessibility_lines)


@dataclass(frozen=True, slots=True)
class NativeApprovalIntent:
    action_id: str
    run_id: str
    device_id: str
    app_id: str
    window_id: str
    origin: str | None
    operation: str
    arguments_digest: str
    target_snapshot_digest: str
    risk_reason: str

    def render_summary(self) -> str:
        values = (
            self.action_id,
            self.run_id,
            self.device_id,
            self.app_id,
            self.window_id,
            self.origin or "local desktop application",
            self.operation,
            self.arguments_digest,
            self.target_snapshot_digest,
            self.risk_reason,
        )
        if any(not value or len(value) > 1024 for value in values):
            raise NativeComputerUseDenied("local approval intent is invalid")
        return (
            f"Run: {self.run_id}\n"
            f"Device: {self.device_id}\n"
            f"Application: {self.app_id}\n"
            f"Window: {self.window_id}\n"
            f"Origin: {self.origin or 'local desktop application'}\n"
            f"Operation: {self.operation}\n"
            f"Arguments SHA-256: {self.arguments_digest}\n"
            f"Target SHA-256: {self.target_snapshot_digest}\n"
            f"Risk: {self.risk_reason}\n\n"
            "Approve this exact action once, or take over and deny."
        )


class MacOSKeychainApprovalSigner:
    """Exact-intent signer backed by a native Keychain HMAC key.

    The Swift helper shows the trusted local prompt and only then creates or
    uses the Keychain key. The key bytes never leave the helper process; Python
    receives only the signature. Verification also happens inside the helper.
    """

    def __init__(
        self,
        *,
        device_id: str,
        helper_path: Path,
        service: str = "ai-platform.local-node.trusted-approval.v1",
        account: str | None = None,
    ) -> None:
        if not device_id or len(device_id) > 512:
            raise NativeComputerUseDenied("Keychain approval device id is invalid")
        if not service or len(service) > 255 or any(ord(item) < 0x20 for item in service):
            raise NativeComputerUseDenied("Keychain approval service is invalid")
        resolved_account = device_id if account is None else account
        if (
            not resolved_account
            or len(resolved_account) > 255
            or any(ord(item) < 0x20 for item in resolved_account)
        ):
            raise NativeComputerUseDenied("Keychain approval account is invalid")
        self.device_id = device_id
        self.helper_path = MacOSNativeComputerBackend._validate_helper(helper_path)
        self.service = service
        self.account = resolved_account
        self._lock = threading.RLock()

    def _invoke(self, request: Mapping[str, Any], *, timeout_seconds: float) -> Mapping[str, Any]:
        encoded = canonical_json(dict(request)).encode("utf-8")
        if len(encoded) > 2 * 1024 * 1024:
            raise NativeComputerUseDenied("local approval helper request is too large")
        with self._lock:
            completed = subprocess.run(
                (str(self.helper_path),),
                input=encoded,
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                timeout=timeout_seconds,
                env=dict(_SAFE_ENVIRONMENT),
                shell=False,
            )
        try:
            response = json.loads(completed.stdout)
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise NativeComputerUseDenied("local approval helper returned invalid JSON") from exc
        if not isinstance(response, dict) or response.get("ok") is not True:
            code = (
                str(response.get("code") or "native_approval_failed")
                if isinstance(response, dict)
                else "native_approval_failed"
            )
            raise NativeComputerUseDenied(f"local approval Keychain operation failed: {code}")
        return response

    def prompt_and_sign(
        self,
        *,
        payload: bytes,
        intent: NativeApprovalIntent,
        timeout_seconds: float = 60,
    ) -> str | None:
        if not payload or len(payload) > 1024 * 1024:
            raise NativeComputerUseDenied("local approval payload is invalid")
        summary = intent.render_summary()
        _assert_nonsecret(summary, field="local approval summary")
        response = self._invoke(
            {
                "command": "approval_sign",
                "title": "AI--Platfform Local Node approval",
                "summary": summary,
                "timeout_seconds": timeout_seconds,
                "keychain_service": self.service,
                "keychain_account": self.account,
                "approval_payload_base64": base64.b64encode(payload).decode("ascii"),
            },
            timeout_seconds=timeout_seconds + 5,
        )
        if response.get("approved") is not True or response.get("timed_out") is True:
            return None
        signature = response.get("signature")
        if not isinstance(signature, str) or len(signature) != 64:
            raise NativeComputerUseDenied("native approval signature is invalid")
        return signature

    def verify(self, payload: bytes, signature: str) -> bool:
        if not payload or len(payload) > 1024 * 1024 or len(signature) != 64:
            return False
        try:
            response = self._invoke(
                {
                    "command": "approval_verify",
                    "keychain_service": self.service,
                    "keychain_account": self.account,
                    "approval_payload_base64": base64.b64encode(payload).decode("ascii"),
                    "signature": signature,
                },
                timeout_seconds=10,
            )
        except (DriverUnavailable, NativeComputerUseDenied):
            return False
        return response.get("verified") is True


def _canonical_origin(value: str) -> str:
    parsed = urlsplit(value)
    scheme = parsed.scheme.casefold()
    if scheme == "file":
        if parsed.netloc not in {"", "localhost"}:
            raise NativeComputerUseDenied("file URL host is outside the local scope")
        return "file://"
    if scheme not in {"http", "https"} or not parsed.hostname:
        raise NativeComputerUseDenied("browser origin is unsupported")
    if parsed.username or parsed.password:
        raise NativeComputerUseDenied("browser URL cannot contain credentials")
    host = parsed.hostname.casefold()
    try:
        port = parsed.port
    except ValueError as exc:
        raise NativeComputerUseDenied("browser origin port is invalid") from exc
    default_port = 80 if scheme == "http" else 443
    return f"{scheme}://{host}" + ("" if port in {None, default_port} else f":{port}")


def _assert_nonsecret(value: str, *, field: str) -> None:
    lowered = value.casefold()
    if any(marker in lowered for marker in _SECRET_MARKERS):
        raise NativeComputerUseDenied(f"{field} contains secret-like material")


class MacOSNativeComputerBackend:
    """One explicitly scoped native backend instance for one Assistant session."""

    name = "macos-native-ax-cgevent-v1"

    def __init__(
        self,
        *,
        helper_path: Path,
        session_id: str,
        allowed_apps: frozenset[str],
        allowed_origins: frozenset[str] = frozenset(),
        screenshot_dir: Path | None = None,
        screen_observe: bool = False,
        screen_share: bool = False,
        helper_timeout_seconds: float = 15,
    ) -> None:
        self.helper_path = self._validate_helper(helper_path)
        if not session_id or len(session_id) > 512:
            raise NativeComputerUseDenied("native Computer Use session id is invalid")
        if not allowed_apps or any(_BUNDLE_ID.fullmatch(item) is None for item in allowed_apps):
            raise NativeComputerUseDenied("native Computer Use app scope is invalid")
        canonical_origins = frozenset(_canonical_origin(item) for item in allowed_origins)
        if screen_share and not screen_observe:
            raise NativeComputerUseDenied("screen sharing requires screen observation authority")
        if screen_observe and screenshot_dir is None:
            raise NativeComputerUseDenied("screen observation requires an artifact directory")
        if not math.isfinite(helper_timeout_seconds) or not 1 <= helper_timeout_seconds <= 60:
            raise NativeComputerUseDenied("native helper timeout is invalid")
        self.session_id = session_id
        self.allowed_apps = frozenset(allowed_apps)
        self.allowed_origins = canonical_origins
        self.screen_observe = screen_observe
        self.screen_share = screen_share
        self.helper_timeout_seconds = helper_timeout_seconds
        self.screenshot_dir = (
            None if screenshot_dir is None else self._prepare_screenshot_dir(screenshot_dir)
        )
        self._lock = threading.RLock()
        self._active_process: subprocess.Popen[bytes] | None = None
        self._last_details: NativeObservationDetails | None = None
        self._stopped = False

    @staticmethod
    def _validate_helper(path: Path) -> Path:
        supplied = Path(path)
        if not supplied.is_absolute() or supplied.is_symlink():
            raise DriverUnavailable("native helper must be an absolute non-symlink path")
        try:
            info = supplied.stat(follow_symlinks=False)
        except OSError as exc:
            raise DriverUnavailable("native macOS helper is unavailable") from exc
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_nlink != 1
            or not os.access(supplied, os.X_OK)
            or info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
        ):
            raise DriverUnavailable("native macOS helper is not a trusted executable")
        return supplied.resolve(strict=True)

    @staticmethod
    def _prepare_screenshot_dir(path: Path) -> Path:
        supplied = Path(path)
        if not supplied.is_absolute() or supplied.is_symlink():
            raise NativeComputerUseDenied("screenshot directory must be absolute and local")
        supplied.mkdir(parents=True, exist_ok=True, mode=0o700)
        resolved = supplied.resolve(strict=True)
        info = resolved.stat(follow_symlinks=False)
        if not stat.S_ISDIR(info.st_mode) or info.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
            raise NativeComputerUseDenied("screenshot directory permissions are unsafe")
        os.chmod(resolved, 0o700)
        return resolved

    @property
    def last_details(self) -> NativeObservationDetails | None:
        with self._lock:
            return self._last_details

    def _invoke(
        self,
        request: Mapping[str, Any],
        *,
        timeout_seconds: float | None = None,
    ) -> Mapping[str, Any]:
        try:
            encoded = canonical_json(dict(request)).encode("utf-8")
        except (TypeError, ValueError) as exc:
            raise NativeComputerUseDenied("native helper request is invalid") from exc
        if len(encoded) > 1024 * 1024:
            raise NativeComputerUseDenied("native helper request exceeds one MiB")
        with self._lock:
            if self._active_process is not None:
                raise NativeComputerUseDenied("another native Computer Use operation is active")
            process = subprocess.Popen(
                (str(self.helper_path),),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                env=dict(_SAFE_ENVIRONMENT),
                shell=False,
                start_new_session=True,
            )
            self._active_process = process
        try:
            stdout, _ = process.communicate(
                encoded,
                timeout=self.helper_timeout_seconds if timeout_seconds is None else timeout_seconds,
            )
        except subprocess.TimeoutExpired as exc:
            self._terminate(process)
            raise NativeComputerUseDenied("native Computer Use operation timed out") from exc
        finally:
            with self._lock:
                if self._active_process is process:
                    self._active_process = None
        if len(stdout) > _MAX_HELPER_OUTPUT:
            raise NativeComputerUseDenied("native helper response exceeds one MiB")
        try:
            decoded = json.loads(stdout)
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise NativeComputerUseDenied("native helper returned invalid JSON") from exc
        if not isinstance(decoded, dict):
            raise NativeComputerUseDenied("native helper response must be an object")
        if decoded.get("ok") is not True:
            code = str(decoded.get("code") or "native_backend_failed")
            if code in {"stale_window", "stale_origin"}:
                raise StaleTargetError("native Computer Use target changed")
            if code == "takeover_required":
                raise NativeTakeoverRequired(
                    "password, MFA, permission, or security dialog requires user takeover"
                )
            if code in {"accessibility_denied", "screen_recording_denied"}:
                raise DriverUnavailable("required macOS permission is unavailable")
            raise NativeComputerUseDenied(f"native Computer Use denied: {code}")
        return decoded

    @staticmethod
    def _terminate(process: subprocess.Popen[bytes]) -> None:
        if process.poll() is not None:
            return
        try:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=1)
        except (ProcessLookupError, subprocess.TimeoutExpired):
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=1)

    def permission_health(self) -> tuple[bool, bool]:
        response = self._invoke({"command": "doctor"})
        accessibility = response.get("accessibility") is True
        screen_recording = response.get("screen_recording") is True
        return accessibility, screen_recording

    def accessibility_ready(self) -> bool:
        return self.permission_health()[0]

    def screen_recording_ready(self) -> bool:
        return self.permission_health()[1]

    def _capture_screenshot(self, window_id: str, risk_flags: tuple[str, ...]) -> tuple[Path, str]:
        if not self.screen_observe or self.screenshot_dir is None:
            raise NativeComputerUseDenied("screen observation is outside the native scope")
        if risk_flags:
            raise NativeTakeoverRequired("sensitive or modal UI cannot be captured")
        if not window_id.isdecimal() or len(window_id) > 20:
            raise NativeComputerUseDenied("native window id is invalid")
        path = self.screenshot_dir / f"screen-{secrets.token_urlsafe(18)}.png"
        completed = subprocess.run(
            ("/usr/sbin/screencapture", "-x", "-o", "-l", window_id, str(path)),
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            env=dict(_SAFE_ENVIRONMENT),
            shell=False,
            timeout=10,
        )
        if completed.returncode != 0:
            raise DriverUnavailable("native screen capture failed")
        try:
            content = path.read_bytes()
        except OSError as exc:
            raise DriverUnavailable("native screenshot artifact is unavailable") from exc
        if len(content) < len(_PNG_SIGNATURE) or not content.startswith(_PNG_SIGNATURE):
            path.unlink(missing_ok=True)
            raise DriverUnavailable("native screenshot artifact is invalid")
        os.chmod(path, 0o600)
        return path, hashlib.sha256(content).hexdigest()

    def observe(self, app_id: str, window_id: str | None) -> ComputerObservation:
        if app_id not in self.allowed_apps:
            raise NativeComputerUseDenied("application is outside the injected native scope")
        request: dict[str, Any] = {"command": "observe", "app_id": app_id}
        if window_id is not None:
            request["window_id"] = window_id
        response = self._invoke(request)
        if response.get("app_id") != app_id:
            raise StaleTargetError("native helper observed another application")
        observed_window = response.get("window_id")
        window_x = response.get("x")
        window_y = response.get("y")
        width = response.get("width")
        height = response.get("height")
        if (
            not isinstance(observed_window, str)
            or not observed_window.isdecimal()
            or isinstance(window_x, bool)
            or not isinstance(window_x, (int, float))
            or isinstance(window_y, bool)
            or not isinstance(window_y, (int, float))
            or isinstance(width, bool)
            or not isinstance(width, (int, float))
            or isinstance(height, bool)
            or not isinstance(height, (int, float))
            or not math.isfinite(float(width))
            or not math.isfinite(float(height))
            or not math.isfinite(float(window_x))
            or not math.isfinite(float(window_y))
            or width <= 0
            or height <= 0
        ):
            raise NativeComputerUseDenied("native helper returned invalid window geometry")
        raw_origin = response.get("origin")
        origin = None if raw_origin is None else _canonical_origin(str(raw_origin))
        if origin is not None and origin not in self.allowed_origins:
            raise NativeComputerUseDenied("browser origin is outside the injected native scope")
        if self.allowed_origins and origin is None:
            raise NativeComputerUseDenied("browser origin could not be proven")
        raw_lines = response.get("accessibility_lines")
        raw_flags = response.get("risk_flags")
        if (
            not isinstance(raw_lines, list)
            or len(raw_lines) > 800
            or any(not isinstance(item, str) or len(item) > 2000 for item in raw_lines)
            or not isinstance(raw_flags, list)
            or any(not isinstance(item, str) or len(item) > 100 for item in raw_flags)
        ):
            raise NativeComputerUseDenied("native accessibility response is invalid")
        lines = tuple(raw_lines)
        flags = tuple(sorted(set(raw_flags)))
        if flags:
            # Never materialize accessibility text or screenshot artifacts for
            # secure fields, MFA, permission, or security/modal dialogs.
            raise NativeTakeoverRequired(
                "password, MFA, permission, or security dialog requires user takeover"
            )
        accessibility_text = "\n".join(lines)
        _assert_nonsecret(accessibility_text, field="accessibility snapshot")
        accessibility_digest = digest_payload(list(lines))
        screenshot_path: Path | None = None
        screenshot_sha256: str | None = None
        if self.screen_observe:
            screenshot_path, screenshot_sha256 = self._capture_screenshot(
                observed_window,
                flags,
            )
        observed_at = time.time()
        observation_id = (
            "obs_"
            + digest_payload(
                {
                    "session_id": self.session_id,
                    "app_id": app_id,
                    "window_id": observed_window,
                    "origin": origin,
                    "width": int(width),
                    "height": int(height),
                    "accessibility_digest": accessibility_digest,
                    "screenshot_sha256": screenshot_sha256,
                    "observed_at_ns": time.time_ns(),
                }
            )[:32]
        )
        details = NativeObservationDetails(
            observation_id=observation_id,
            session_id=self.session_id,
            app_id=app_id,
            window_id=observed_window,
            window_title=str(response.get("window_title") or "")[:500],
            origin=origin,
            frontmost=response.get("frontmost") is True,
            window_x=float(window_x),
            window_y=float(window_y),
            accessibility_lines=lines,
            risk_flags=flags,
            screenshot_path=screenshot_path,
            screenshot_sha256=screenshot_sha256,
        )
        with self._lock:
            self._last_details = details
            self._stopped = False
        return ComputerObservation(
            observation_id=observation_id,
            app_id=app_id,
            window_id=observed_window,
            origin=origin,
            width=int(width),
            height=int(height),
            screenshot_ref=(
                str(screenshot_path) if self.screen_share and screenshot_path is not None else None
            ),
            accessibility_digest=accessibility_digest,
            observed_at=observed_at,
        )

    def _validated_arguments(
        self,
        action: ComputerAction,
        details: NativeObservationDetails,
    ) -> dict[str, Any]:
        arguments = dict(action.arguments)
        if action.kind in {"click", "double_click"}:
            if set(arguments) != {"x", "y"}:
                raise NativeComputerUseDenied("click requires exact x/y arguments")
        elif action.kind == "type_text":
            if set(arguments) - {"text", "delay_ms"} or not isinstance(arguments.get("text"), str):
                raise NativeComputerUseDenied("type_text arguments are invalid")
            text = str(arguments["text"])
            if len(text.encode("utf-8")) > 10_000:
                raise NativeComputerUseDenied("type_text exceeds the input budget")
            _assert_nonsecret(text, field="typed text")
        elif action.kind == "key_press":
            if set(arguments) != {"key"} or not isinstance(arguments.get("key"), str):
                raise NativeComputerUseDenied("key_press requires one bounded key")
        elif action.kind == "scroll":
            if set(arguments) - {"x", "y", "scroll_y"} or "scroll_y" not in arguments:
                raise NativeComputerUseDenied("scroll arguments are invalid")
        elif action.kind == "drag":
            if set(arguments) != {"from_x", "from_y", "to_x", "to_y"}:
                raise NativeComputerUseDenied("drag arguments are invalid")
        elif action.kind == "wait":
            if set(arguments) != {"duration_ms"}:
                raise NativeComputerUseDenied("wait requires duration_ms")
        elif action.kind == "open_app":
            if set(arguments) - {"url"}:
                raise NativeComputerUseDenied("open_app arguments are invalid")
            if "url" in arguments:
                raw_url = arguments["url"]
                if (
                    not isinstance(raw_url, str)
                    or _canonical_origin(raw_url) not in self.allowed_origins
                ):
                    raise NativeComputerUseDenied("open_app URL is outside the allowed origin")
        elif action.kind == "focus_app":
            if arguments:
                raise NativeComputerUseDenied("focus_app does not accept arguments")
        else:
            raise NativeComputerUseDenied("native Computer Use action is unsupported")
        if details.risk_flags:
            raise NativeTakeoverRequired(
                "password, MFA, permission, or security dialog requires user takeover"
            )
        return arguments

    def execute(self, action: ComputerAction) -> None:
        with self._lock:
            details = self._last_details
            stopped = self._stopped
        if stopped or details is None:
            raise StaleTargetError("native Computer Use lease was stopped or not observed")
        if action.observation_id != details.observation_id:
            raise StaleTargetError("native Computer Use action uses a stale observation")
        arguments = self._validated_arguments(action, details)
        self._invoke(
            {
                "command": "action",
                "app_id": details.app_id,
                "window_id": details.window_id,
                "expected_origin": details.origin,
                "kind": action.kind,
                "arguments": arguments,
            },
            timeout_seconds=min(
                60,
                self.helper_timeout_seconds + (float(arguments.get("duration_ms", 0)) / 1000),
            ),
        )

    def stop(self) -> None:
        with self._lock:
            self._stopped = True
            process = self._active_process
            self._last_details = None
        if process is not None:
            self._terminate(process)
        try:
            self._invoke({"command": "stop"}, timeout_seconds=2)
        except NativeComputerUseDenied:
            # The controller lease is already invalid. Failure to emit release
            # events is reported as a local degradation, never as continued authority.
            return
