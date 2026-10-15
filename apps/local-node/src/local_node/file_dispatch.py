"""Bounded file-only handlers for signed outbound claim commands."""

from __future__ import annotations

import time
from dataclasses import asdict
from typing import Any, Mapping

from .errors import CapabilityDenied
from .files import LocalFileService
from .models import ActionContext, ActionStatus
from .transport import DispatchOutcome
from .watcher import DirectoryWatcher
from .watcher import WatchEvent
from .workspace import SecureWorkspace

_TOOLS = {
    "file.list": "local_file_list",
    "file.read": "local_file_read",
    "file.hash": "local_file_hash",
    "file.search": "local_file_search",
    "file.watch": "local_file_watch",
}


def _bounded_int(
    value: Any,
    *,
    minimum: int,
    maximum: int,
    default: int,
) -> int:
    if value is None:
        return default
    if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
        raise CapabilityDenied("file action budget is invalid")
    return value


def _arguments(
    action: ActionContext,
    raw: Mapping[str, Any],
    *,
    allowed: frozenset[str],
    operation: str,
) -> tuple[str, str, dict[str, Any]]:
    if action.operation != operation or action.tool_name != _TOOLS[operation]:
        raise CapabilityDenied("signed file action operation or tool changed")
    if set(raw) - allowed or not {"grant_id", "path"}.issubset(raw):
        raise CapabilityDenied("file action arguments are invalid")
    arguments = dict(raw)
    grant_id = arguments.get("grant_id")
    relative = arguments.get("path")
    if not isinstance(grant_id, str) or not grant_id or not isinstance(relative, str):
        raise CapabilityDenied("file action arguments are invalid")
    if action.capability_lease_id != grant_id or action.resource_refs != (grant_id, relative):
        raise CapabilityDenied("file action grant or resource binding changed")
    grant = action.capability
    if grant != ("file.read" if operation == "file.hash" else operation):
        raise CapabilityDenied("signed file capability changed")
    return grant_id, relative, arguments


class ReadOnlyFileActionHandlers:
    """Owner-bound handlers; no process, GUI, write, or ambient-path fallback."""

    def __init__(self, files: LocalFileService) -> None:
        self.files = files

    def as_mapping(self) -> dict[str, Any]:
        return {
            "file.list": self.list_files,
            "file.read": self.read_file,
            "file.search": self.search_files,
            "file.watch": self.watch_files,
        }

    def _grant(self, action: ActionContext, grant_id: str, capability: str) -> None:
        self.files.grants.get(
            grant_id,
            capability,
            tenant_id=action.tenant_id,
            user_id=action.user_id,
        )

    def list_files(
        self,
        action: ActionContext,
        raw: Mapping[str, Any],
    ) -> DispatchOutcome:
        operation = "file.list"
        grant_id, relative, arguments = _arguments(
            action,
            raw,
            allowed=frozenset({"grant_id", "path", "limit"}),
            operation=operation,
        )
        self._grant(action, grant_id, "list")
        SecureWorkspace.assert_safe_relative(relative)
        limit = _bounded_int(arguments.get("limit"), minimum=1, maximum=500, default=500)
        entries = self.files.list_files(grant_id, relative, recursive=False)[:limit]
        return DispatchOutcome(
            action.action_id,
            ActionStatus.SUCCEEDED,
            {"kind": "file_list", "entries": [asdict(item) for item in entries]},
        )

    def read_file(
        self,
        action: ActionContext,
        raw: Mapping[str, Any],
    ) -> DispatchOutcome:
        if action.operation not in {"file.read", "file.hash"}:
            raise CapabilityDenied("signed file read operation changed")
        grant_id, relative, arguments = _arguments(
            action,
            raw,
            allowed=frozenset({"grant_id", "path", "max_bytes"}),
            operation=action.operation,
        )
        self._grant(action, grant_id, "read")
        SecureWorkspace.assert_safe_relative(relative)
        max_bytes = _bounded_int(
            arguments.get("max_bytes"),
            minimum=1,
            maximum=8 * 1024 * 1024,
            default=8 * 1024 * 1024,
        )
        result = self.files.read_file(grant_id, relative)
        if result.size > max_bytes:
            raise CapabilityDenied("file exceeds signed read budget")
        common: dict[str, Any] = {
            "kind": "file_hash" if action.operation == "file.hash" else "file_read",
            "relative_path": result.relative_path,
            "encoding": result.encoding,
            "size": result.size,
            "sha256": result.sha256,
        }
        if action.operation == "file.read":
            common["content"] = (
                result.content.decode("utf-8") if result.encoding == "utf-8" else None
            )
        return DispatchOutcome(action.action_id, ActionStatus.SUCCEEDED, common)

    def search_files(
        self,
        action: ActionContext,
        raw: Mapping[str, Any],
    ) -> DispatchOutcome:
        operation = "file.search"
        grant_id, relative, arguments = _arguments(
            action,
            raw,
            allowed=frozenset({"grant_id", "path", "query", "limit"}),
            operation=operation,
        )
        self._grant(action, grant_id, "search")
        SecureWorkspace.assert_safe_relative(relative)
        query = arguments.get("query")
        if not isinstance(query, str) or not query or len(query) > 500 or "\x00" in query:
            raise CapabilityDenied("file search query is invalid")
        limit = _bounded_int(arguments.get("limit"), minimum=1, maximum=200, default=200)
        matches = self.files.search(
            grant_id,
            query,
            glob="*" if relative == "." else f"{relative.rstrip('/')}/*",
            max_matches=limit,
        )
        return DispatchOutcome(
            action.action_id,
            ActionStatus.SUCCEEDED,
            {"kind": "file_search", "matches": [asdict(item) for item in matches]},
        )

    def watch_files(
        self,
        action: ActionContext,
        raw: Mapping[str, Any],
    ) -> DispatchOutcome:
        operation = "file.watch"
        grant_id, relative, arguments = _arguments(
            action,
            raw,
            allowed=frozenset({"grant_id", "path", "after_revision", "timeout_ms"}),
            operation=operation,
        )
        self._grant(action, grant_id, "watch")
        SecureWorkspace.assert_safe_relative(relative)
        after_revision = arguments.get("after_revision")
        if after_revision not in {None, "0"}:
            raise CapabilityDenied("file watch cursor is unavailable for this session")
        timeout_ms = _bounded_int(
            arguments.get("timeout_ms"),
            minimum=1,
            maximum=30_000,
            default=1_000,
        )
        # Poll only for the signed bounded window. No background thread survives
        # the action and no file body enters the watch result.
        watcher = DirectoryWatcher(self.files, grant_id)
        watcher.scan_once()
        deadline = time.monotonic() + timeout_ms / 1000
        events: tuple[WatchEvent, ...] = ()
        while not events:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            time.sleep(min(0.05, remaining))
            events = watcher.scan_once()
        filtered: list[WatchEvent] = [
            event
            for event in events
            if relative == "."
            or event.relative_path == relative
            or event.relative_path.startswith(relative.rstrip("/") + "/")
        ]
        return DispatchOutcome(
            action.action_id,
            ActionStatus.SUCCEEDED,
            {"kind": "file_watch", "events": [asdict(item) for item in filtered]},
        )


__all__ = ["ReadOnlyFileActionHandlers"]
