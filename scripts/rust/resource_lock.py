#!/usr/bin/env python3
"""Shared Git-common-dir locks for singleton integration and Rust builds."""

from __future__ import annotations

import argparse
import contextlib
import datetime as dt
import json
import math
import os
import re
import signal
import socket
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path
from typing import Any

SCHEMA_VERSION = "ai-gateway/resource-lock/v1"
RESOURCES = ("integration-runtime", "rust-build")
LOCK_ROOT_NAME = "ai-gateway-locks"
MUTEX_NAME = ".low-memory"
OWNER_FILE = "owner.json"
REDACTED = "<redacted>"
SENSITIVE_NAMES = (
    "KEY",
    "KEYS",
    "TOKEN",
    "TOKENS",
    "SECRET",
    "SECRETS",
    "PASSWORD",
    "PASSWORDS",
    "DSN",
    "CREDENTIAL",
    "CREDENTIALS",
)
URL_USERINFO = re.compile(
    r"(?P<scheme>[A-Za-z][A-Za-z0-9+.-]*://)(?P<userinfo>[^/@\s]+)@"
)
EMBEDDED_ASSIGNMENT = re.compile(
    r"(?P<name>\b[A-Za-z_][A-Za-z0-9_]*)=(?P<value>[^\s,;]+)"
)
EMBEDDED_FLAG_VALUE = re.compile(
    r"(?P<flag>--[A-Za-z0-9_-]+)(?P<separator>=|\s+)(?P<value>[^\s,;]+)"
)
MAX_RUNTIME_IDENTITY_BYTES = 64 * 1024
MAX_RUNTIME_IDENTITY_DEPTH = 8
CHILD_TERMINATE_GRACE_SECONDS = 2.0


class LockError(RuntimeError):
    """Fail-closed lock acquisition, ownership, or cleanup error."""


def _sensitive_name(value: str) -> bool:
    expanded = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", value)
    normalized = re.sub(r"[^A-Za-z0-9]+", "_", expanded).upper().strip("_")
    components = set(normalized.split("_"))
    return any(
        normalized == name
        or name in components
        or normalized.endswith(f"_{name}")
        or normalized.endswith(name)
        for name in SENSITIVE_NAMES
    )


def _redact_urls(value: str) -> str:
    return URL_USERINFO.sub(lambda match: f"{match.group('scheme')}{REDACTED}@", value)


def _redact_text(value: str) -> str:
    value = _redact_urls(value)

    def assignment(match: re.Match[str]) -> str:
        if not _sensitive_name(match.group("name")):
            return match.group(0)
        return f"{match.group('name')}={REDACTED}"

    def flag_value(match: re.Match[str]) -> str:
        if not _sensitive_name(match.group("flag").lstrip("-")):
            return match.group(0)
        return f"{match.group('flag')}{match.group('separator')}{REDACTED}"

    return EMBEDDED_FLAG_VALUE.sub(flag_value, EMBEDDED_ASSIGNMENT.sub(assignment, value))


def redact_command(command: list[str]) -> list[str]:
    """Return a deterministic receipt-safe copy without changing execution args."""

    redacted: list[str] = []
    redact_next = False
    for argument in command:
        if redact_next:
            redacted.append(REDACTED)
            redact_next = False
            continue
        if "=" in argument and not argument.startswith(("-", "http://", "https://")):
            name, value = argument.split("=", 1)
            if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name) and _sensitive_name(name):
                redacted.append(f"{name}={REDACTED}")
                continue
            argument = f"{name}={_redact_text(value)}"
        if argument.startswith("-"):
            flag, separator, value = argument.partition("=")
            if _sensitive_name(flag.lstrip("-")):
                if separator:
                    redacted.append(f"{flag}={REDACTED}")
                else:
                    redacted.append(flag)
                    redact_next = True
                continue
        redacted.append(_redact_text(argument))
    return redacted


def _sanitize_runtime_value(value: Any, *, key: str | None, depth: int) -> Any:
    if depth > MAX_RUNTIME_IDENTITY_DEPTH:
        raise LockError("runtime identity exceeds the maximum nesting depth")
    if key is not None and _sensitive_name(key):
        return REDACTED
    if isinstance(value, dict):
        if len(value) > 256:
            raise LockError("runtime identity object is too large")
        sanitized: dict[str, Any] = {}
        for child_key, child_value in value.items():
            if not isinstance(child_key, str) or not child_key or len(child_key) > 128:
                raise LockError("runtime identity has an invalid key")
            if any(ord(character) < 32 for character in child_key):
                raise LockError("runtime identity has an invalid key")
            sanitized[child_key] = _sanitize_runtime_value(
                child_value, key=child_key, depth=depth + 1
            )
        return sanitized
    if isinstance(value, list):
        if len(value) > 256:
            raise LockError("runtime identity array is too large")
        return [
            _sanitize_runtime_value(item, key=None, depth=depth + 1) for item in value
        ]
    if isinstance(value, str):
        if len(value) > 4096 or any(ord(character) == 0 for character in value):
            raise LockError("runtime identity string is invalid")
        return _redact_text(value)
    if isinstance(value, float) and not math.isfinite(value):
        raise LockError("runtime identity contains a non-finite number")
    if value is None or isinstance(value, (bool, int, float)):
        return value
    raise LockError("runtime identity contains an unsupported JSON value")


def parse_runtime_identity(raw: str | None) -> dict[str, Any] | None:
    if raw is None:
        return None
    if not raw or len(raw.encode("utf-8")) > MAX_RUNTIME_IDENTITY_BYTES:
        raise LockError("runtime identity JSON is empty or too large")

    def reject_constant(value: str) -> None:
        raise LockError(f"runtime identity contains non-finite value: {value}")

    def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise LockError(f"runtime identity contains duplicate key: {key}")
            result[key] = value
        return result

    try:
        value = json.loads(
            raw,
            parse_constant=reject_constant,
            object_pairs_hook=reject_duplicate_keys,
        )
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise LockError("runtime identity is not valid JSON") from exc
    if not isinstance(value, dict):
        raise LockError("runtime identity must be a JSON object")
    sanitized = _sanitize_runtime_value(value, key=None, depth=0)
    assert isinstance(sanitized, dict)
    return sanitized


def _now() -> dt.datetime:
    return dt.datetime.now(dt.timezone.utc)


def _timestamp(value: dt.datetime) -> str:
    return value.astimezone(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def _parse_timestamp(value: Any) -> dt.datetime:
    if not isinstance(value, str) or not value:
        raise LockError("lock owner timestamp is missing")
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise LockError("lock owner timestamp is invalid") from exc
    if parsed.tzinfo is None:
        raise LockError("lock owner timestamp has no timezone")
    return parsed.astimezone(dt.timezone.utc)


def _git(cwd: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise LockError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def repository_identity(cwd: Path) -> tuple[Path, Path, str]:
    worktree = Path(_git(cwd, "rev-parse", "--show-toplevel")).resolve()
    common_raw = _git(worktree, "rev-parse", "--git-common-dir")
    common = Path(common_raw)
    if not common.is_absolute():
        common = worktree / common
    common = common.resolve()
    head = _git(worktree, "rev-parse", "HEAD")
    return worktree, common, head


def _load_owner(path: Path) -> dict[str, Any]:
    try:
        owner = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise LockError(f"lock owner receipt is missing or invalid: {path}") from exc
    required = {
        "schema_version": str,
        "owner_token": str,
        "resource": str,
        "hostname": str,
        "pid": int,
        "worktree_path": str,
        "git_head": str,
        "command": list,
        "start_time": str,
        "heartbeat_time": str,
        "timeout_seconds": int,
        "expected_end_condition": str,
    }
    if not isinstance(owner, dict):
        raise LockError(f"lock owner receipt is not an object: {path}")
    for key, expected_type in required.items():
        if not isinstance(owner.get(key), expected_type):
            raise LockError(f"lock owner receipt field {key!r} is invalid: {path}")
    if owner["schema_version"] != SCHEMA_VERSION or owner["resource"] not in RESOURCES:
        raise LockError(f"lock owner receipt schema/resource is invalid: {path}")
    if owner["pid"] <= 0 or owner["timeout_seconds"] <= 0 or not owner["owner_token"]:
        raise LockError(f"lock owner receipt bounds are invalid: {path}")
    if not all(isinstance(argument, str) for argument in owner["command"]):
        raise LockError(f"lock owner command is invalid: {path}")
    if redact_command(owner["command"]) != owner["command"]:
        raise LockError(f"lock owner command contains unredacted sensitive data: {path}")
    if _redact_text(owner["expected_end_condition"]) != owner["expected_end_condition"]:
        raise LockError(f"lock end condition contains unredacted sensitive data: {path}")
    runtime_identity = owner.get("runtime_identity")
    if runtime_identity is not None:
        if not isinstance(runtime_identity, dict):
            raise LockError(f"lock runtime identity is invalid: {path}")
        sanitized = _sanitize_runtime_value(runtime_identity, key=None, depth=0)
        if sanitized != runtime_identity:
            raise LockError(f"lock runtime identity contains unredacted sensitive data: {path}")
    _parse_timestamp(owner["start_time"])
    _parse_timestamp(owner["heartbeat_time"])
    return owner


def _atomic_write_owner(lock_dir: Path, owner: dict[str, Any]) -> None:
    token = owner["owner_token"]
    temporary = lock_dir / f".owner-{token}-{os.getpid()}.tmp"
    payload = json.dumps(owner, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    try:
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, lock_dir / OWNER_FILE)
    finally:
        with contextlib.suppress(FileNotFoundError):
            temporary.unlink()


def _pid_state(pid: int) -> str:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return "dead"
    except PermissionError:
        return "alive"
    except OSError:
        return "ambiguous"
    return "alive"


def stale_reason(
    owner: dict[str, Any], *, hostname: str | None = None, now: dt.datetime | None = None
) -> tuple[bool, str]:
    current_host = hostname or socket.gethostname()
    if owner["hostname"] != current_host:
        return False, "cross-host owner cannot be reaped automatically"
    process_state = _pid_state(owner["pid"])
    if process_state != "dead":
        return False, f"owner process is {process_state}"
    heartbeat = _parse_timestamp(owner["heartbeat_time"])
    age_seconds = ((now or _now()) - heartbeat).total_seconds()
    if age_seconds <= owner["timeout_seconds"]:
        return False, "dead owner has not exceeded its timeout"
    return True, "same-host owner is dead and heartbeat timeout elapsed"


class ResourceLock:
    """One low-memory singleton lock with a resource-specific receipt."""

    def __init__(
        self,
        resource: str,
        *,
        command: list[str],
        timeout_seconds: int,
        expected_end_condition: str,
        runtime_identity: dict[str, Any] | None = None,
        cwd: Path | None = None,
    ) -> None:
        if resource not in RESOURCES:
            raise LockError(f"unknown lock resource: {resource}")
        if timeout_seconds <= 0:
            raise LockError("lock timeout must be positive")
        if not command or not all(isinstance(item, str) and item for item in command):
            raise LockError("lock command must be a non-empty string array")
        if not expected_end_condition.strip():
            raise LockError("expected end condition is required")
        worktree, common, head = repository_identity((cwd or Path.cwd()).resolve())
        self.resource = resource
        self.command = list(command)
        self.timeout_seconds = timeout_seconds
        self.expected_end_condition = expected_end_condition
        sanitized_identity = (
            _sanitize_runtime_value(runtime_identity, key=None, depth=0)
            if runtime_identity is not None
            else None
        )
        if sanitized_identity is not None and not isinstance(sanitized_identity, dict):
            raise LockError("runtime identity must be a JSON object")
        self.runtime_identity = sanitized_identity
        self.worktree = worktree
        self.git_common_dir = common
        self.git_head = head
        self.lock_root = common / LOCK_ROOT_NAME
        self.mutex_dir = self.lock_root / MUTEX_NAME
        self.resource_dir = self.lock_root / resource
        self.owner: dict[str, Any] | None = None
        self.acquired = False

    def _new_owner(self) -> dict[str, Any]:
        timestamp = _timestamp(_now())
        owner = {
            "schema_version": SCHEMA_VERSION,
            "owner_token": uuid.uuid4().hex,
            "resource": self.resource,
            "hostname": socket.gethostname(),
            "pid": os.getpid(),
            "worktree_path": str(self.worktree),
            "git_head": self.git_head,
            "command": redact_command(self.command),
            "start_time": timestamp,
            "heartbeat_time": timestamp,
            "timeout_seconds": self.timeout_seconds,
            "expected_end_condition": _redact_text(self.expected_end_condition),
        }
        if self.runtime_identity is not None:
            owner["runtime_identity"] = self.runtime_identity
        return owner

    @staticmethod
    def _allowed_entries(lock_dir: Path, token: str) -> bool:
        if lock_dir.is_symlink() or not lock_dir.is_dir():
            return False
        try:
            entries = list(lock_dir.iterdir())
        except OSError:
            return False
        return all(
            not entry.is_symlink()
            and entry.is_file()
            and (
                entry.name == OWNER_FILE
                or re.fullmatch(rf"\.owner-{re.escape(token)}-[0-9]+\.tmp", entry.name)
                is not None
            )
            for entry in entries
        )

    @staticmethod
    def _remove_known_dir(lock_dir: Path, token: str) -> None:
        if not ResourceLock._allowed_entries(lock_dir, token):
            raise LockError(f"lock directory contains ambiguous files: {lock_dir}")
        for entry in list(lock_dir.iterdir()):
            entry.unlink()
        lock_dir.rmdir()

    def _resource_receipt_matches(self, owner: dict[str, Any]) -> bool:
        if not self.resource_dir.exists():
            return True
        owner_path = self.resource_dir / OWNER_FILE
        if not owner_path.exists():
            return not any(self.resource_dir.iterdir())
        try:
            resource_owner = _load_owner(owner_path)
        except LockError:
            return False
        keys = ("owner_token", "resource", "hostname", "pid")
        return all(resource_owner[key] == owner[key] for key in keys) and self._allowed_entries(
            self.resource_dir, owner["owner_token"]
        )

    def _try_reap_stale_mutex(self) -> bool:
        if self.mutex_dir.is_symlink() or not self.mutex_dir.is_dir():
            raise LockError("existing low-memory lock path is ambiguous")
        try:
            owner = _load_owner(self.mutex_dir / OWNER_FILE)
        except LockError as exc:
            raise LockError(f"existing low-memory lock is ambiguous: {exc}") from exc
        stale, reason = stale_reason(owner)
        if not stale:
            raise LockError(
                f"{owner['resource']} lock is busy ({reason}); "
                f"owner={owner['hostname']}:{owner['pid']} worktree={owner['worktree_path']}"
            )
        other_resources = [
            self.lock_root / resource
            for resource in RESOURCES
            if resource != owner["resource"] and (self.lock_root / resource).exists()
        ]
        owner_resource_dir = self.lock_root / owner["resource"]
        original_resource_dir = self.resource_dir
        self.resource_dir = owner_resource_dir
        try:
            if other_resources or not self._resource_receipt_matches(owner):
                raise LockError("stale lock has ambiguous resource receipts; refusing auto-release")
        finally:
            self.resource_dir = original_resource_dir

        quarantine = self.lock_root / f".stale-{owner['owner_token']}"
        try:
            self.mutex_dir.rename(quarantine)
        except FileNotFoundError:
            return True
        except OSError as exc:
            raise LockError("failed to claim stale lock for cleanup") from exc
        try:
            if owner_resource_dir.exists():
                self._remove_known_dir(owner_resource_dir, owner["owner_token"])
            self._remove_known_dir(quarantine, owner["owner_token"])
        except Exception:
            if quarantine.exists() and not self.mutex_dir.exists():
                with contextlib.suppress(OSError):
                    quarantine.rename(self.mutex_dir)
            raise
        return True

    def acquire(self) -> dict[str, Any]:
        if self.acquired:
            raise LockError("lock is already acquired")
        self.lock_root.mkdir(mode=0o700, parents=True, exist_ok=True)
        for _attempt in range(3):
            try:
                self.mutex_dir.mkdir(mode=0o700)
            except FileExistsError:
                if self._try_reap_stale_mutex():
                    continue
                raise LockError("failed to reap stale low-memory lock")
            owner = self._new_owner()
            resource_created = False
            try:
                _atomic_write_owner(self.mutex_dir, owner)
                existing = [
                    self.lock_root / resource
                    for resource in RESOURCES
                    if (self.lock_root / resource).exists()
                ]
                if existing:
                    raise LockError(
                        "resource lock exists without the low-memory owner; refusing ambiguous cleanup"
                    )
                self.resource_dir.mkdir(mode=0o700)
                resource_created = True
                _atomic_write_owner(self.resource_dir, owner)
            except Exception:
                if resource_created and self.resource_dir.exists() and not any(
                    self.resource_dir.iterdir()
                ):
                    self.resource_dir.rmdir()
                with contextlib.suppress(Exception):
                    self._remove_known_dir(self.mutex_dir, owner["owner_token"])
                raise
            self.owner = owner
            self.acquired = True
            return dict(owner)
        raise LockError("lock acquisition did not converge after stale cleanup")

    def _assert_owned(self) -> dict[str, Any]:
        if not self.acquired or self.owner is None:
            raise LockError("lock is not acquired by this process")
        expected = self.owner["owner_token"]
        for directory in (self.mutex_dir, self.resource_dir):
            current = _load_owner(directory / OWNER_FILE)
            if current["owner_token"] != expected or current["resource"] != self.resource:
                raise LockError("lock ownership changed while held")
        return self.owner

    def heartbeat(self) -> None:
        owner = self._assert_owned()
        owner["heartbeat_time"] = _timestamp(_now())
        _atomic_write_owner(self.resource_dir, owner)
        _atomic_write_owner(self.mutex_dir, owner)

    def release(self) -> None:
        owner = self._assert_owned()
        token = owner["owner_token"]
        self._remove_known_dir(self.resource_dir, token)
        self._remove_known_dir(self.mutex_dir, token)
        self.owner = None
        self.acquired = False


def force_release(
    cwd: Path,
    *,
    resource: str,
    owner_token: str,
    reason: str,
    allow_cross_host: bool = False,
    allow_live_owner: bool = False,
) -> dict[str, Any]:
    if resource not in RESOURCES:
        raise LockError(f"unknown lock resource: {resource}")
    if not owner_token or len(owner_token) > 128 or any(
        ord(character) < 32 for character in owner_token
    ):
        raise LockError("force-release requires the exact owner token")
    if not reason.strip() or len(reason) > 1000 or any(
        ord(character) < 32 and character not in "\t\n" for character in reason
    ):
        raise LockError("force-release requires a non-empty audit reason")

    _worktree, common, _head = repository_identity(cwd)
    lock_root = common / LOCK_ROOT_NAME
    mutex_dir = lock_root / MUTEX_NAME
    resource_dir = lock_root / resource
    opposite_dirs = [
        lock_root / candidate
        for candidate in RESOURCES
        if candidate != resource and (lock_root / candidate).exists()
    ]
    if opposite_dirs:
        raise LockError("opposite resource receipt exists; refusing ambiguous force-release")
    existing_dirs = [directory for directory in (mutex_dir, resource_dir) if directory.exists()]
    if not existing_dirs:
        raise LockError("requested lock does not exist")

    valid_owners: list[dict[str, Any]] = []
    invalid_receipts: list[str] = []
    for directory in existing_dirs:
        if not ResourceLock._allowed_entries(directory, owner_token):
            raise LockError(f"lock directory contains unknown files: {directory}")
        owner_path = directory / OWNER_FILE
        if not owner_path.exists():
            invalid_receipts.append(str(owner_path))
            continue
        try:
            owner = _load_owner(owner_path)
        except LockError:
            invalid_receipts.append(str(owner_path))
            continue
        if owner["owner_token"] != owner_token:
            raise LockError("force-release requires the exact owner token")
        if owner["resource"] != resource:
            raise LockError("force-release resource does not match the receipt")
        valid_owners.append(owner)
    if not valid_owners:
        raise LockError("no valid owner receipt can verify the exact owner token")

    current_host = socket.gethostname()
    cross_host = any(owner["hostname"] != current_host for owner in valid_owners)
    if cross_host and not allow_cross_host:
        raise LockError("cross-host force-release requires --allow-cross-host")
    live_owner = any(
        owner["hostname"] == current_host and _pid_state(owner["pid"]) != "dead"
        for owner in valid_owners
    )
    if live_owner and not allow_live_owner:
        raise LockError("live-owner force-release requires --allow-live-owner")

    if resource_dir.exists():
        ResourceLock._remove_known_dir(resource_dir, owner_token)
    if mutex_dir.exists():
        ResourceLock._remove_known_dir(mutex_dir, owner_token)
    return {
        "action": "force-release",
        "resource": resource,
        "owner_token": owner_token,
        "reason": _redact_text(reason.strip()),
        "cross_host_override": cross_host,
        "live_owner_override": live_owner,
        "ambiguous_receipts_removed": len(invalid_receipts),
    }


def _child_tree_alive(process: subprocess.Popen[Any]) -> bool:
    if os.name != "posix":
        return process.poll() is None
    try:
        os.killpg(process.pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _signal_child_tree(process: subprocess.Popen[Any], signum: int) -> None:
    if os.name == "posix":
        os.killpg(process.pid, signum)
    else:
        process.send_signal(signum)


def _wait_for_child_tree(process: subprocess.Popen[Any], timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while _child_tree_alive(process):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        with contextlib.suppress(subprocess.TimeoutExpired):
            process.wait(timeout=min(0.05, remaining))
        time.sleep(min(0.01, max(0.0, remaining)))
    with contextlib.suppress(subprocess.TimeoutExpired):
        process.wait(timeout=0)
    return True


def _terminate_child(process: subprocess.Popen[Any], *, context: str) -> None:
    if not _child_tree_alive(process):
        return
    with contextlib.suppress(ProcessLookupError):
        _signal_child_tree(process, signal.SIGTERM)
    if _wait_for_child_tree(process, CHILD_TERMINATE_GRACE_SECONDS):
        return
    with contextlib.suppress(ProcessLookupError):
        _signal_child_tree(process, signal.SIGKILL)
    if not _wait_for_child_tree(process, CHILD_TERMINATE_GRACE_SECONDS):
        raise LockError(f"{context}: child process group did not exit after SIGKILL")


def run_locked(
    lock: ResourceLock,
    command: list[str],
    *,
    heartbeat_seconds: float,
) -> int:
    if heartbeat_seconds <= 0 or heartbeat_seconds >= lock.timeout_seconds:
        raise LockError("heartbeat interval must be positive and below the lock timeout")
    stop = threading.Event()
    heartbeat_errors: list[BaseException] = []

    def heartbeat_loop() -> None:
        while not stop.wait(heartbeat_seconds):
            try:
                lock.heartbeat()
            except BaseException as exc:  # noqa: BLE001 - propagated to the supervisor loop
                heartbeat_errors.append(exc)
                stop.set()
                return

    heartbeat_thread: threading.Thread | None = None
    process: subprocess.Popen[Any] | None = None
    received_signal: list[int] = []
    previous_handlers: dict[int, Any] = {}
    acquired = False

    def handle_signal(signum: int, _frame: Any) -> None:
        received_signal.append(signum)
        if process is not None and _child_tree_alive(process):
            with contextlib.suppress(ProcessLookupError):
                _signal_child_tree(process, signum)

    for signum in (signal.SIGINT, signal.SIGTERM):
        previous_handlers[signum] = signal.getsignal(signum)
        signal.signal(signum, handle_signal)
    try:
        lock.acquire()
        acquired = True
        if received_signal:
            return 128 + received_signal[-1]
        heartbeat_thread = threading.Thread(
            target=heartbeat_loop, name="resource-lock-heartbeat"
        )
        heartbeat_thread.start()
        process = subprocess.Popen(
            command,
            cwd=lock.worktree,
            start_new_session=os.name == "posix",
        )
        while True:
            if received_signal:
                _terminate_child(process, context="signal cleanup failed")
                return 128 + received_signal[-1]
            if heartbeat_errors:
                _terminate_child(process, context="heartbeat cleanup failed")
                raise LockError(
                    f"lock heartbeat failed; child was terminated: {heartbeat_errors[0]}"
                )
            try:
                return_code = process.wait(timeout=0.2)
                break
            except subprocess.TimeoutExpired:
                continue
        if received_signal:
            return 128 + received_signal[-1]
        return return_code
    finally:
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)
        if process is not None and _child_tree_alive(process):
            _terminate_child(process, context="final lock cleanup failed")
        stop.set()
        if heartbeat_thread is not None:
            heartbeat_thread.join(timeout=max(1.0, heartbeat_seconds * 2))
        if acquired:
            lock.release()


def status(cwd: Path) -> int:
    _worktree, common, _head = repository_identity(cwd)
    lock_root = common / LOCK_ROOT_NAME
    result: dict[str, Any] = {}
    for name in (MUTEX_NAME, *RESOURCES):
        owner_path = lock_root / name / OWNER_FILE
        if not owner_path.exists():
            result[name] = None
            continue
        try:
            result[name] = _load_owner(owner_path)
        except LockError as exc:
            result[name] = {"error": str(exc)}
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="action", required=True)
    run_parser = subparsers.add_parser("run", help="run one command under a shared lock")
    run_parser.add_argument("--resource", choices=RESOURCES, required=True)
    run_parser.add_argument("--timeout-seconds", type=int, default=7200)
    run_parser.add_argument("--heartbeat-seconds", type=float, default=10.0)
    run_parser.add_argument("--expected-end-condition", required=True)
    run_parser.add_argument("--runtime-identity-json")
    run_parser.add_argument("command", nargs=argparse.REMAINDER)
    subparsers.add_parser("status", help="show both resource receipts")
    force_parser = subparsers.add_parser(
        "force-release", help="explicitly release an exact known owner receipt"
    )
    force_parser.add_argument("--resource", choices=RESOURCES, required=True)
    force_parser.add_argument("--owner-token", required=True)
    force_parser.add_argument("--reason", required=True)
    force_parser.add_argument("--allow-cross-host", action="store_true")
    force_parser.add_argument("--allow-live-owner", action="store_true")
    args = parser.parse_args(argv)

    if args.action == "status":
        try:
            return status(Path.cwd())
        except LockError as exc:
            print(f"LOCK ERROR: {exc}", file=sys.stderr)
            return 2

    if args.action == "force-release":
        try:
            summary = force_release(
                Path.cwd(),
                resource=args.resource,
                owner_token=args.owner_token,
                reason=args.reason,
                allow_cross_host=args.allow_cross_host,
                allow_live_owner=args.allow_live_owner,
            )
        except LockError as exc:
            print(f"LOCK ERROR: {exc}", file=sys.stderr)
            return 2
        print(json.dumps(summary, ensure_ascii=False, sort_keys=True))
        return 0

    command = list(args.command)
    if command and command[0] == "--":
        command.pop(0)
    if not command:
        parser.error("run requires a command after --")
    try:
        runtime_identity = parse_runtime_identity(args.runtime_identity_json)
        lock = ResourceLock(
            args.resource,
            command=command,
            timeout_seconds=args.timeout_seconds,
            expected_end_condition=args.expected_end_condition,
            runtime_identity=runtime_identity,
        )
        return run_locked(lock, command, heartbeat_seconds=args.heartbeat_seconds)
    except LockError as exc:
        print(f"LOCK ERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
