"""Structured, allowlisted local process execution.

This is intentionally not an ambient shell. The caller supplies an argv vector,
an already-granted cwd, and an executable that the local user configured in the
process policy. No host environment is inherited.
"""

from __future__ import annotations

import os
import selectors
import shutil
import signal
import subprocess
import threading
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Callable, Mapping

from .errors import CapabilityDenied, ProcessPolicyError
from .grants import DirectoryGrantStore
from .ledger import ActionLedger
from .models import ActionContext, ActionStatus, digest_payload
from .workspace import SecureWorkspace


_SECRET_ENV_FRAGMENTS = (
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "AUTH",
    "COOKIE",
    "SSH_",
    "AWS_",
    "AZURE_",
    "GOOGLE_",
    "OPENAI_",
    "DASHSCOPE_",
)


@dataclass(frozen=True, slots=True)
class ProcessPolicy:
    allowed_executables: frozenset[Path] = field(default_factory=frozenset)
    allowed_env_names: frozenset[str] = field(default_factory=frozenset)
    path: str = "/usr/bin:/bin"
    max_timeout_seconds: float = 300
    max_output_bytes: int = 2 * 1024 * 1024
    allow_inherited_network: bool = False

    def normalized_executables(self) -> frozenset[Path]:
        return frozenset(Path(item).resolve(strict=True) for item in self.allowed_executables)


@dataclass(frozen=True, slots=True)
class ProcessRequest:
    argv: tuple[str, ...]
    grant_id: str
    cwd: str = "."
    timeout_seconds: float = 60
    env: Mapping[str, str] = field(default_factory=dict)
    network_policy: str = "deny"

    def normalized_arguments(self) -> dict[str, object]:
        return {
            "argv": list(self.argv),
            "grant_id": self.grant_id,
            "cwd": self.cwd,
            "timeout_seconds": self.timeout_seconds,
            "env_names": sorted(self.env),
            "network_policy": self.network_policy,
        }


@dataclass(frozen=True, slots=True)
class ProcessResult:
    action_id: str
    status: str
    exit_code: int | None
    stdout: str
    stderr: str
    started_at: float
    ended_at: float
    output_truncated: bool
    error_code: str | None = None

    @classmethod
    def from_dict(cls, value: dict[str, object]) -> "ProcessResult":
        return cls(**value)  # type: ignore[arg-type]


class ProcessRunner:
    def __init__(
        self,
        grants: DirectoryGrantStore,
        policy: ProcessPolicy,
        ledger: ActionLedger,
    ) -> None:
        self.grants = grants
        self.policy = policy
        self.ledger = ledger
        self._lock = threading.RLock()
        self._processes: dict[str, subprocess.Popen[bytes]] = {}
        self._cancelled: set[str] = set()

    def _resolve_executable(self, argv0: str) -> Path:
        if os.path.sep in argv0:
            candidate = Path(argv0).resolve(strict=True)
        else:
            found = shutil.which(argv0, path=self.policy.path)
            if found is None:
                raise ProcessPolicyError("executable was not found in the sanitized PATH")
            candidate = Path(found).resolve(strict=True)
        if candidate not in self.policy.normalized_executables():
            raise ProcessPolicyError("executable is not in the local allowlist")
        return candidate

    def _environment(self, requested: Mapping[str, str]) -> dict[str, str]:
        result = {"PATH": self.policy.path, "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8"}
        for name, value in requested.items():
            upper = name.upper()
            if name not in self.policy.allowed_env_names:
                raise ProcessPolicyError(f"environment variable {name!r} is not allowlisted")
            if any(fragment in upper for fragment in _SECRET_ENV_FRAGMENTS):
                raise ProcessPolicyError("secret-like environment variables cannot be injected")
            if "\x00" in name or "\x00" in value or "=" in name:
                raise ProcessPolicyError("invalid environment entry")
            result[name] = value
        return result

    def cwd_snapshot_digest(
        self,
        request: ProcessRequest,
        *,
        tenant_id: str,
        user_id: str,
    ) -> tuple[Path, str]:
        grant = self.grants.get(
            request.grant_id,
            "read",
            tenant_id=tenant_id,
            user_id=user_id,
        )
        cwd = SecureWorkspace(grant).resolve(request.cwd)
        if not cwd.is_dir():
            raise ProcessPolicyError("process cwd must be a granted directory")
        info = cwd.stat(follow_symlinks=False)
        digest = digest_payload(
            {
                "grant_id": request.grant_id,
                "cwd": request.cwd,
                "device": info.st_dev,
                "inode": info.st_ino,
            }
        )
        return cwd, digest

    def run(
        self,
        request: ProcessRequest,
        action: ActionContext,
        *,
        on_output: Callable[[str, bytes], None] | None = None,
    ) -> ProcessResult:
        if action.capability != "process.run":
            raise CapabilityDenied("action envelope lacks process.run")
        if action.operation != "process.run":
            raise CapabilityDenied("action envelope operation mismatch")
        if not request.argv or any("\x00" in arg for arg in request.argv):
            raise ProcessPolicyError("argv must be a non-empty NUL-free vector")
        action.validate_payload(
            request.normalized_arguments(),
            verifier=self.ledger.platform_signature_verifier,
            capability_lease_id=request.grant_id,
            resource_refs=(request.grant_id, request.cwd),
        )
        if (
            request.timeout_seconds <= 0
            or request.timeout_seconds > self.policy.max_timeout_seconds
        ):
            raise ProcessPolicyError("process timeout exceeds local policy")
        if request.network_policy == "inherit":
            if not self.policy.allow_inherited_network:
                raise ProcessPolicyError("host network inheritance is not allowed")
        elif request.network_policy == "deny":
            # A dependency-free host subprocess cannot enforce a network
            # namespace or seatbelt profile.  Treat the requested policy as a
            # hard contract instead of silently running with ambient network.
            # A future sandbox backend can explicitly satisfy this branch.
            raise ProcessPolicyError(
                "network-denied execution requires a configured sandbox backend"
            )
        else:
            raise ProcessPolicyError("unsupported network policy")
        executable = self._resolve_executable(request.argv[0])
        cwd, cwd_digest = self.cwd_snapshot_digest(
            request, tenant_id=action.tenant_id, user_id=action.user_id
        )
        environment = self._environment(request.env)
        begin = self.ledger.begin(action)
        if not begin.created:
            if begin.record.result is None:
                raise CapabilityDenied("matching process action is already running")
            return ProcessResult.from_dict(begin.record.result)
        started_at = time.time()
        process: subprocess.Popen[bytes] | None = None
        try:
            self.ledger.mark_awaiting_approval(action.action_id)
            action.require_approval(
                target_snapshot_digest=cwd_digest,
                verifier=self.ledger.trusted_local_approval_verifier,
            )
            self.ledger.mark_dispatched(action.action_id)
            process = subprocess.Popen(
                (str(executable), *request.argv[1:]),
                cwd=cwd,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                shell=False,
                start_new_session=True,
            )
            with self._lock:
                self._processes[action.action_id] = process
            self.ledger.mark_running(action.action_id)
            stdout, stderr, truncated = self._collect(
                process,
                action.action_id,
                timeout=request.timeout_seconds,
                on_output=on_output,
            )
            cancelled = action.action_id in self._cancelled
            status = ActionStatus.CANCELLED if cancelled else ActionStatus.SUCCEEDED
            result = ProcessResult(
                action.action_id,
                status.value,
                process.returncode,
                stdout.decode("utf-8", errors="replace"),
                stderr.decode("utf-8", errors="replace"),
                started_at,
                time.time(),
                truncated,
                "cancelled" if cancelled else None,
            )
            self.ledger.finish(action.action_id, status, asdict(result))
            return result
        except subprocess.TimeoutExpired:
            if process is not None:
                self._terminate(process)
            result = ProcessResult(
                action.action_id,
                ActionStatus.FAILED.value,
                None if process is None else process.returncode,
                "",
                "",
                started_at,
                time.time(),
                False,
                "timeout",
            )
            self.ledger.finish(action.action_id, ActionStatus.FAILED, asdict(result))
            return result
        except Exception as exc:
            if process is not None:
                self._terminate(process)
            self.ledger.finish(
                action.action_id,
                ActionStatus.FAILED,
                {"error_code": getattr(exc, "code", "process_failed")},
            )
            raise
        finally:
            with self._lock:
                self._processes.pop(action.action_id, None)

    def _collect(
        self,
        process: subprocess.Popen[bytes],
        action_id: str,
        *,
        timeout: float,
        on_output: Callable[[str, bytes], None] | None,
    ) -> tuple[bytes, bytes, bool]:
        assert process.stdout is not None and process.stderr is not None
        selector = selectors.DefaultSelector()
        streams = {process.stdout: "stdout", process.stderr: "stderr"}
        for stream in streams:
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ)
        chunks: dict[str, list[bytes]] = {"stdout": [], "stderr": []}
        used = 0
        truncated = False
        deadline = time.monotonic() + timeout
        try:
            while selector.get_map():
                if time.monotonic() >= deadline:
                    raise subprocess.TimeoutExpired(process.args, timeout)
                events = selector.select(timeout=min(0.05, max(0, deadline - time.monotonic())))
                for key, _ in events:
                    data = os.read(key.fd, 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                        continue
                    room = self.policy.max_output_bytes - used
                    kept = data[: max(0, room)]
                    if kept:
                        channel = streams[key.fileobj]  # type: ignore[index]
                        chunks[channel].append(kept)
                        used += len(kept)
                        if on_output is not None:
                            on_output(channel, kept)
                    if len(kept) != len(data):
                        truncated = True
                with self._lock:
                    cancelled = action_id in self._cancelled
                if cancelled and process.poll() is None:
                    self._terminate(process)
            process.wait(timeout=max(0.1, deadline - time.monotonic()))
        finally:
            selector.close()
        return b"".join(chunks["stdout"]), b"".join(chunks["stderr"]), truncated

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

    def cancel(self, action_id: str) -> bool:
        with self._lock:
            self._cancelled.add(action_id)
            process = self._processes.get(action_id)
        if process is None:
            return False
        self._terminate(process)
        return True

    def cancel_all(self) -> tuple[str, ...]:
        with self._lock:
            action_ids = tuple(self._processes)
        for action_id in action_ids:
            self.cancel(action_id)
        return action_ids
