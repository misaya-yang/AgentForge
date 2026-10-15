"""Metadata-and-hash polling watcher for explicitly granted directories."""

from __future__ import annotations

import threading
import time
from dataclasses import dataclass
from typing import Callable

from .files import LocalFileService


@dataclass(frozen=True, slots=True)
class WatchEvent:
    sequence: int
    kind: str
    relative_path: str
    previous_path: str | None
    sha256: str | None
    size: int | None
    observed_at: float


class DirectoryWatcher:
    def __init__(
        self,
        files: LocalFileService,
        grant_id: str,
        *,
        interval_seconds: float = 0.25,
        callback: Callable[[WatchEvent], None] | None = None,
    ) -> None:
        if interval_seconds <= 0:
            raise ValueError("watch interval must be positive")
        files.grants.get(grant_id, "watch")
        self.files = files
        self.grant_id = grant_id
        self.interval_seconds = interval_seconds
        self.callback = callback
        self._snapshot: dict[str, tuple[int, int, int, str]] | None = None
        self._sequence = 0
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def _capture(self) -> dict[str, tuple[int, int, int, str]]:
        # Re-resolve the grant on every poll. Revocation therefore stops
        # observation immediately instead of letting an already-started
        # watcher retain authority until its thread is restarted.
        self.files.grants.get(self.grant_id, "watch")
        captured: dict[str, tuple[int, int, int, str]] = {}
        workspace = self.files._workspace(self.grant_id, "watch")
        for entry in self.files.list_files(self.grant_id, recursive=True):
            if entry.kind != "file":
                continue
            try:
                with workspace.open_read(entry.relative_path) as stream:
                    info = __import__("os").fstat(stream.fileno())
                    content = stream.read(self.files.max_read_bytes + 1)
                    if len(content) > self.files.max_read_bytes:
                        continue
                    digest = __import__("hashlib").sha256(content).hexdigest()
                captured[entry.relative_path] = (info.st_dev, info.st_ino, info.st_size, digest)
            except (OSError, RuntimeError):
                continue
        return captured

    def scan_once(self) -> tuple[WatchEvent, ...]:
        current = self._capture()
        if self._snapshot is None:
            self._snapshot = current
            return ()
        previous = self._snapshot
        deleted = set(previous) - set(current)
        created = set(current) - set(previous)
        events: list[WatchEvent] = []
        deleted_by_inode = {(previous[p][0], previous[p][1]): p for p in deleted}
        for path in sorted(tuple(created)):
            inode = (current[path][0], current[path][1])
            old_path = deleted_by_inode.pop(inode, None)
            if old_path is not None:
                deleted.discard(old_path)
                events.append(self._event("rename", path, old_path, current[path]))
            else:
                events.append(self._event("create", path, None, current[path]))
        for path in sorted(deleted):
            events.append(self._event("delete", path, None, None))
        for path in sorted(set(previous) & set(current)):
            if previous[path] != current[path]:
                events.append(self._event("modify", path, None, current[path]))
        self._snapshot = current
        for event in events:
            if self.callback is not None:
                self.callback(event)
        return tuple(events)

    def _event(
        self,
        kind: str,
        path: str,
        previous_path: str | None,
        snapshot: tuple[int, int, int, str] | None,
    ) -> WatchEvent:
        self._sequence += 1
        return WatchEvent(
            self._sequence,
            kind,
            path,
            previous_path,
            None if snapshot is None else snapshot[3],
            None if snapshot is None else snapshot[2],
            time.time(),
        )

    def start(self) -> None:
        if self._thread is not None and self._thread.is_alive():
            return
        self._stop.clear()
        self.scan_once()
        self._thread = threading.Thread(target=self._run, daemon=True, name="local-node-watcher")
        self._thread.start()

    def _run(self) -> None:
        while not self._stop.wait(self.interval_seconds):
            self.scan_once()

    def stop(self, timeout: float = 2.0) -> bool:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout)
            return not self._thread.is_alive()
        return True
