"""Granted local file operations with exact hashes and recoverable writes."""

from __future__ import annotations

import base64
import fnmatch
import hashlib
import json
import os
import secrets
import stat
import time
from dataclasses import asdict, dataclass
from pathlib import Path, PurePath
from typing import Any

from .errors import CapabilityDenied, PathEscapeError, StaleTargetError
from .grants import DirectoryGrantStore
from .ledger import ActionLedger
from .models import ActionContext, ActionStatus
from .workspace import SecureWorkspace, _O_CLOEXEC, _O_NOFOLLOW


MISSING_DIGEST = "missing"


def sha256_bytes(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


@dataclass(frozen=True, slots=True)
class FileEntry:
    relative_path: str
    kind: str
    size: int
    modified_ns: int


@dataclass(frozen=True, slots=True)
class FileReadResult:
    relative_path: str
    content: bytes
    sha256: str
    size: int
    modified_ns: int
    encoding: str | None


@dataclass(frozen=True, slots=True)
class SearchMatch:
    relative_path: str
    line: int
    column: int
    preview: str
    file_sha256: str


@dataclass(frozen=True, slots=True)
class WriteReceipt:
    action_id: str
    grant_id: str
    relative_path: str
    before_sha256: str
    after_sha256: str
    bytes_written: int
    rollback_ref: str
    restored_from: str | None = None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "WriteReceipt":
        return cls(**value)


@dataclass(frozen=True, slots=True)
class FileAnalysisResult:
    """Exact, citeable result for one explicitly selected file.

    Content stays local in ``read``. Search citations carry path, line, column,
    and the exact file hash so a later read can detect a stale citation.
    """

    read: FileReadResult
    matches: tuple[SearchMatch, ...]


class LocalFileService:
    def __init__(
        self,
        grants: DirectoryGrantStore,
        rollback_dir: Path,
        ledger: ActionLedger,
        *,
        max_read_bytes: int = 8 * 1024 * 1024,
    ) -> None:
        self.grants = grants
        self.rollback_dir = rollback_dir
        self.rollback_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        os.chmod(self.rollback_dir, 0o700)
        self.ledger = ledger
        self.max_read_bytes = max_read_bytes

    def _workspace(
        self, grant_id: str, capability: str, action: ActionContext | None = None
    ) -> SecureWorkspace:
        grant = self.grants.get(
            grant_id,
            capability,
            tenant_id=None if action is None else action.tenant_id,
            user_id=None if action is None else action.user_id,
        )
        return SecureWorkspace(grant)

    def list_files(
        self, grant_id: str, relative: str = ".", *, recursive: bool = False
    ) -> tuple[FileEntry, ...]:
        workspace = self._workspace(grant_id, "list")
        SecureWorkspace.assert_safe_relative(relative)
        queue = [relative]
        entries: list[FileEntry] = []
        while queue:
            directory = queue.pop(0)
            fd = workspace.open_dir(directory)
            try:
                names = sorted(os.listdir(fd))
                for name in names:
                    child = name if directory == "." else str(PurePath(directory) / name)
                    try:
                        SecureWorkspace.assert_safe_relative(child)
                        info = os.stat(name, dir_fd=fd, follow_symlinks=False)
                    except (OSError, CapabilityDenied):
                        continue
                    if stat.S_ISLNK(info.st_mode):
                        continue
                    if stat.S_ISDIR(info.st_mode):
                        entries.append(FileEntry(child, "directory", 0, info.st_mtime_ns))
                        if recursive:
                            queue.append(child)
                    elif stat.S_ISREG(info.st_mode) and info.st_nlink == 1:
                        entries.append(FileEntry(child, "file", info.st_size, info.st_mtime_ns))
            finally:
                os.close(fd)
        return tuple(entries)

    def read_file(self, grant_id: str, relative: str) -> FileReadResult:
        workspace = self._workspace(grant_id, "read")
        with workspace.open_read(relative) as stream:
            info = os.fstat(stream.fileno())
            if info.st_size > self.max_read_bytes:
                raise CapabilityDenied("file exceeds local read budget")
            content = stream.read(self.max_read_bytes + 1)
        if len(content) > self.max_read_bytes:
            raise CapabilityDenied("file changed beyond local read budget")
        encoding: str | None
        try:
            content.decode("utf-8")
            encoding = "utf-8"
        except UnicodeDecodeError:
            encoding = None
        return FileReadResult(
            relative_path=relative,
            content=content,
            sha256=sha256_bytes(content),
            size=len(content),
            modified_ns=info.st_mtime_ns,
            encoding=encoding,
        )

    def search(
        self,
        grant_id: str,
        query: str,
        *,
        glob: str = "*",
        max_matches: int = 200,
    ) -> tuple[SearchMatch, ...]:
        self.grants.get(grant_id, "search")
        if not query or max_matches <= 0:
            return ()
        matches: list[SearchMatch] = []
        for entry in self.list_files(grant_id, recursive=True):
            if entry.kind != "file" or not fnmatch.fnmatch(entry.relative_path, glob):
                continue
            try:
                result = self.read_file(grant_id, entry.relative_path)
                if result.encoding is None:
                    continue
                text = result.content.decode(result.encoding)
            except CapabilityDenied:
                continue
            for line_number, line in enumerate(text.splitlines(), start=1):
                offset = line.find(query)
                if offset >= 0:
                    matches.append(
                        SearchMatch(
                            entry.relative_path,
                            line_number,
                            offset + 1,
                            line[:300],
                            result.sha256,
                        )
                    )
                    if len(matches) >= max_matches:
                        return tuple(matches)
        return tuple(matches)

    def analyze_files(
        self,
        grant_id: str,
        relative_paths: tuple[str, ...],
        *,
        query: str | None = None,
        max_files: int = 100,
        max_matches_per_file: int = 200,
    ) -> tuple[FileAnalysisResult, ...]:
        """Read/hash a bounded explicit file set and optionally grep it.

        This is intentionally not whole-disk awareness. Every path is
        descriptor-contained under one opaque directory grant; duplicates,
        unsafe paths, and budget expansion fail before any content is returned.
        """
        self.grants.get(grant_id, "read")
        if query is not None:
            self.grants.get(grant_id, "search")
        if (
            not relative_paths
            or len(relative_paths) > max_files
            or max_files <= 0
            or max_files > 1000
            or max_matches_per_file <= 0
            or max_matches_per_file > 1000
        ):
            raise CapabilityDenied("multi-file analysis budget is invalid")
        if len(set(relative_paths)) != len(relative_paths):
            raise CapabilityDenied("multi-file analysis paths must be unique")
        if query is not None and (not query or len(query) > 10_000):
            raise CapabilityDenied("multi-file search query is invalid")
        try:
            for relative_path in relative_paths:
                SecureWorkspace.assert_safe_relative(relative_path)
        except (TypeError, ValueError, PathEscapeError) as exc:
            raise CapabilityDenied("multi-file analysis path is invalid") from exc

        results: list[FileAnalysisResult] = []
        for relative_path in relative_paths:
            read = self.read_file(grant_id, relative_path)
            matches: list[SearchMatch] = []
            if query is not None and read.encoding is not None:
                text = read.content.decode(read.encoding)
                for line_number, line in enumerate(text.splitlines(), start=1):
                    offset = line.find(query)
                    if offset < 0:
                        continue
                    matches.append(
                        SearchMatch(
                            relative_path,
                            line_number,
                            offset + 1,
                            line[:300],
                            read.sha256,
                        )
                    )
                    if len(matches) >= max_matches_per_file:
                        break
            results.append(FileAnalysisResult(read, tuple(matches)))
        return tuple(results)

    def _read_optional(
        self, workspace: SecureWorkspace, relative: str
    ) -> tuple[bytes | None, os.stat_result | None]:
        try:
            with workspace.open_read(relative) as stream:
                info = os.fstat(stream.fileno())
                content = stream.read(self.max_read_bytes + 1)
        except FileNotFoundError:
            return None, None
        if len(content) > self.max_read_bytes:
            raise CapabilityDenied("file exceeds local write/rollback budget")
        return content, info

    def _save_rollback(
        self, grant_id: str, relative: str, before: bytes | None, before_mode: int | None
    ) -> str:
        rollback_ref = "rollback_" + secrets.token_urlsafe(18)
        payload = {
            "grant_id": grant_id,
            "relative_path": relative,
            "before": None if before is None else base64.b64encode(before).decode("ascii"),
            "before_sha256": MISSING_DIGEST if before is None else sha256_bytes(before),
            "before_mode": before_mode,
            "created_at": time.time(),
        }
        path = self.rollback_dir / f"{rollback_ref}.json"
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        try:
            os.write(fd, json.dumps(payload, separators=(",", ":")).encode())
            os.fsync(fd)
        finally:
            os.close(fd)
        return rollback_ref

    def _atomic_replace(
        self,
        workspace: SecureWorkspace,
        relative: str,
        content: bytes,
        *,
        expected_hash: str,
    ) -> tuple[str, int | None]:
        parent_fd, name = workspace.open_parent(relative)
        temporary = f".local-node-{secrets.token_hex(12)}.tmp"
        before_mode: int | None = None
        try:
            try:
                target_stat = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
                if stat.S_ISLNK(target_stat.st_mode) or not stat.S_ISREG(target_stat.st_mode):
                    raise StaleTargetError("target is not a regular file")
                if target_stat.st_nlink != 1:
                    raise StaleTargetError("hard-linked targets cannot be replaced")
                before_mode = stat.S_IMODE(target_stat.st_mode)
                target_fd = os.open(name, os.O_RDONLY | _O_CLOEXEC | _O_NOFOLLOW, dir_fd=parent_fd)
                try:
                    before_now = os.read(target_fd, self.max_read_bytes + 1)
                    opened_stat = os.fstat(target_fd)
                finally:
                    os.close(target_fd)
                if (
                    len(before_now) > self.max_read_bytes
                    or sha256_bytes(before_now) != expected_hash
                ):
                    raise StaleTargetError("target changed before write")
                current_stat = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
                if (
                    current_stat.st_dev,
                    current_stat.st_ino,
                    current_stat.st_size,
                    current_stat.st_mtime_ns,
                ) != (
                    opened_stat.st_dev,
                    opened_stat.st_ino,
                    opened_stat.st_size,
                    opened_stat.st_mtime_ns,
                ):
                    raise StaleTargetError("target identity changed before write")
            except FileNotFoundError:
                if expected_hash != MISSING_DIGEST:
                    raise StaleTargetError("target was removed before write")
            temp_fd = os.open(
                temporary,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | _O_CLOEXEC,
                0o600 if before_mode is None else before_mode,
                dir_fd=parent_fd,
            )
            try:
                view = memoryview(content)
                while view:
                    written = os.write(temp_fd, view)
                    view = view[written:]
                os.fsync(temp_fd)
            finally:
                os.close(temp_fd)
            os.replace(temporary, name, src_dir_fd=parent_fd, dst_dir_fd=parent_fd)
            os.fsync(parent_fd)
        finally:
            try:
                os.unlink(temporary, dir_fd=parent_fd)
            except FileNotFoundError:
                pass
            os.close(parent_fd)
        return sha256_bytes(content), before_mode

    def write_atomic(
        self,
        grant_id: str,
        relative: str,
        content: bytes | str,
        expected_hash: str | None,
        action: ActionContext,
    ) -> WriteReceipt:
        raw = content.encode("utf-8") if isinstance(content, str) else bytes(content)
        payload = {
            "grant_id": grant_id,
            "relative_path": relative,
            "content_sha256": sha256_bytes(raw),
            "expected_hash": expected_hash,
        }
        action.validate_payload(
            payload,
            verifier=self.ledger.platform_signature_verifier,
            capability_lease_id=grant_id,
            resource_refs=(grant_id, relative),
        )
        if action.capability != "file.write":
            raise CapabilityDenied("action envelope lacks file.write")
        if action.operation != "file.write":
            raise CapabilityDenied("action envelope operation mismatch")
        begin = self.ledger.begin(action)
        if not begin.created:
            if begin.record.result is None:
                raise CapabilityDenied("matching action is already in progress")
            return WriteReceipt.from_dict(begin.record.result)
        workspace = self._workspace(grant_id, "write", action)
        try:
            before, before_info = self._read_optional(workspace, relative)
            before_hash = MISSING_DIGEST if before is None else sha256_bytes(before)
            if expected_hash is None:
                expected_hash = MISSING_DIGEST
            if before_hash != expected_hash:
                raise StaleTargetError("expected hash does not match current target")
            self.ledger.mark_awaiting_approval(action.action_id)
            action.require_approval(
                target_snapshot_digest=before_hash,
                verifier=self.ledger.trusted_local_approval_verifier,
            )
            self.ledger.mark_dispatched(action.action_id)
            self.ledger.mark_running(action.action_id)
            rollback_ref = self._save_rollback(
                grant_id,
                relative,
                before,
                None if before_info is None else stat.S_IMODE(before_info.st_mode),
            )
            after_hash, _ = self._atomic_replace(
                workspace, relative, raw, expected_hash=before_hash
            )
            receipt = WriteReceipt(
                action.action_id,
                grant_id,
                relative,
                before_hash,
                after_hash,
                len(raw),
                rollback_ref,
            )
            self.ledger.finish(action.action_id, ActionStatus.SUCCEEDED, asdict(receipt))
            return receipt
        except Exception as exc:
            self.ledger.finish(
                action.action_id,
                ActionStatus.FAILED,
                {"error_code": getattr(exc, "code", "file_write_failed")},
            )
            raise

    def rollback(self, rollback_ref: str, action: ActionContext) -> WriteReceipt:
        action.verify_platform_signature(self.ledger.platform_signature_verifier)
        path = self.rollback_dir / f"{rollback_ref}.json"
        if path.parent != self.rollback_dir or path.is_symlink():
            raise CapabilityDenied("rollback reference is invalid")
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except (FileNotFoundError, json.JSONDecodeError) as exc:
            raise CapabilityDenied("rollback reference unavailable") from exc
        grant_id = payload["grant_id"]
        relative = payload["relative_path"]
        workspace = self._workspace(grant_id, "rollback", action)
        current, _ = self._read_optional(workspace, relative)
        current_hash = MISSING_DIGEST if current is None else sha256_bytes(current)
        args = {
            "rollback_ref": rollback_ref,
            "grant_id": grant_id,
            "relative_path": relative,
            "expected_current_hash": current_hash,
        }
        action.validate_payload(
            args,
            verifier=self.ledger.platform_signature_verifier,
            capability_lease_id=grant_id,
            resource_refs=(rollback_ref, grant_id, relative),
        )
        if action.capability != "file.write":
            raise CapabilityDenied("rollback requires signed file.write authority")
        if action.operation != "file.rollback":
            raise CapabilityDenied("rollback action operation mismatch")
        begin = self.ledger.begin(action)
        if not begin.created:
            if begin.record.result is None:
                raise CapabilityDenied("matching rollback is already in progress")
            return WriteReceipt.from_dict(begin.record.result)
        try:
            self.ledger.mark_awaiting_approval(action.action_id)
            action.require_approval(
                target_snapshot_digest=current_hash,
                verifier=self.ledger.trusted_local_approval_verifier,
            )
            self.ledger.mark_dispatched(action.action_id)
            self.ledger.mark_running(action.action_id)
            before_encoded = payload["before"]
            if before_encoded is None:
                parent_fd, name = workspace.open_parent(relative)
                try:
                    os.unlink(name, dir_fd=parent_fd)
                    os.fsync(parent_fd)
                finally:
                    os.close(parent_fd)
                restored = MISSING_DIGEST
                size = 0
            else:
                before = base64.b64decode(before_encoded, validate=True)
                restored, _ = self._atomic_replace(
                    workspace, relative, before, expected_hash=current_hash
                )
                size = len(before)
            receipt = WriteReceipt(
                action.action_id,
                grant_id,
                relative,
                current_hash,
                restored,
                size,
                rollback_ref,
                restored_from=rollback_ref,
            )
            self.ledger.finish(action.action_id, ActionStatus.SUCCEEDED, asdict(receipt))
            return receipt
        except Exception as exc:
            self.ledger.finish(
                action.action_id,
                ActionStatus.FAILED,
                {"error_code": getattr(exc, "code", "rollback_failed")},
            )
            raise
