"""Worker-owned expiry/deletion. Filesystem locks serialize admission and removal."""
import asyncio
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import time

SCOPE = re.compile(r"[0-9a-f]{64}\Z")
_LAST_SWEEP = None
_SWEEP_FAILED = False


def retention_ready():
    return not _SWEEP_FAILED and _LAST_SWEEP is not None and time.monotonic() - _LAST_SWEEP < 120


class SessionBusy(Exception):
    pass


class SessionCleanupRequired(SessionBusy):
    pass


class SessionDeleted(Exception):
    pass


def retention_seconds():
    value = int(os.environ["SDK_AGENT_RETENTION_SECONDS"])
    if not 3600 <= value <= 7776000:
        raise ValueError("sdk_retention_invalid")
    return value


@contextmanager
def admission_gate(root):
    gate = (root / ".admission.lock").open("a")
    try:
        try:
            fcntl.flock(gate, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise SessionBusy() from None
        yield
    finally:
        gate.close()


def marker(root, scope):
    if not SCOPE.fullmatch(scope):
        raise ValueError("sdk_scope_invalid")
    return root / ".tombstones" / (scope + ".json")


def touch_activity(state):
    (state / ".activity").touch(mode=0o600)


def acquire_session(root, scope):
    """Caller holds returned lock through child shutdown, never only through SSE yield."""
    with admission_gate(root):
        if marker(root, scope).exists():
            raise SessionDeleted()
        state = root / scope
        if state.is_symlink():
            raise ValueError("sdk_state_invalid")
        created = not state.exists()
        state.mkdir(mode=0o700, exist_ok=True)
        lock = (state / ".lock").open("a")
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            lock.close()
            raise SessionBusy() from None
        if cleanup_marker(root, scope).exists():
            lock.close()
            raise SessionCleanupRequired()
        if created:
            touch_activity(state)
            fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
        return state, lock


def cleanup_marker(root, scope):
    if not SCOPE.fullmatch(scope):
        raise ValueError("sdk_scope_invalid")
    return root / ".cleanup" / (scope + ".json")


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def begin_cleanup(state):
    path = cleanup_marker(state.parent, state.name)
    path.parent.mkdir(mode=0o700, exist_ok=True)
    if path.parent.is_symlink():
        raise ValueError("sdk_state_invalid")
    # Remove prior cleanup evidence before a new launch can begin.
    (state / ".cleanup-complete").unlink(missing_ok=True)
    sync_directory(state)
    temporary = path.with_suffix(".tmp")
    with temporary.open("w") as stream:
        json.dump({"schema": "sdk-cleanup/v1"}, stream)
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)
    sync_directory(path.parent)
    sync_directory(state.parent)


def complete_cleanup(state):
    # This receipt means only the known local cleanup protocol completed.
    proof = state / ".cleanup-complete"
    with proof.open("w") as stream:
        stream.write("sdk-cleanup/v1\n")
        stream.flush()
        os.fsync(stream.fileno())
    sync_directory(state)
    path = cleanup_marker(state.parent, state.name)
    if path.exists():
        path.unlink()
        sync_directory(path.parent)


def cleanup_unknown(root, scope, state):
    if cleanup_marker(root, scope).exists():
        return True
    # Legacy/malformed evidence never authorizes deletion of an unknown run.
    if (state / "pending.json").exists():
        proof = state / ".cleanup-complete"
        try:
            return proof.is_symlink() or proof.read_text() != "sdk-cleanup/v1\n"
        except OSError:
            return True
    return False


def tombstone(root, scope, reason):
    path = marker(root, scope)
    path.parent.mkdir(mode=0o700, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    with temporary.open("w") as stream:
        json.dump({"reason": reason, "deleted_at": int(time.time())}, stream)
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)
    # Persist the denial before removing session content; retries finish interrupted deletion.
    for directory in (path.parent, root):
        fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def remove_session(root, scope, reason="deleted", cutoff=None):
    with admission_gate(root):
        path = marker(root, scope)
        state = root / scope
        if state.is_symlink():
            raise ValueError("sdk_state_invalid")
        if cleanup_marker(root, scope).exists() and not state.exists():
            raise SessionCleanupRequired()
        if not state.exists():
            if cutoff is None and not path.exists():
                tombstone(root, scope, reason)
            return False
        lock = (state / ".lock").open("a")
        try:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise SessionBusy() from None
            if cleanup_unknown(root, scope, state):
                raise SessionCleanupRequired()
            activity = state / ".activity"
            changed = activity.stat().st_mtime if activity.exists() else state.stat().st_mtime
            if cutoff is not None and not path.exists() and changed > cutoff:
                return False
            unknown = (state / "pending.json").exists()
            # Expiry closes this scope permanently. Unknown work is never silently replayed.
            if not path.exists():
                tombstone(root, scope, reason + ("_unknown" if unknown else ""))
            shutil.rmtree(state)
            fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
            return True
        finally:
            lock.close()


def parse_scope(body):
    fields = ("tenant_id", "user_id", "service_id", "framework", "session_id")
    if not isinstance(body, dict) or set(body) != set(fields):
        raise ValueError("sdk_delete_invalid")
    for name in fields:
        value = body[name]
        if not isinstance(value, str) or not value.strip() or len(value) > 256 or any(ord(c) < 32 for c in value):
            raise ValueError("sdk_delete_invalid")
    if body["framework"] not in {"openai_agents", "claude_agent_sdk"}:
        raise ValueError("sdk_delete_invalid")
    return hashlib.sha256(json.dumps([body[k] for k in fields], separators=(",", ":")).encode()).hexdigest()


async def delete_session(request):
    from starlette.responses import JSONResponse
    from .server import authorize
    if not authorize(request):
        return JSONResponse({"code": "sdk_worker_unauthorized"}, status_code=401)
    raw = bytearray()
    async for chunk in request.stream():
        raw.extend(chunk)
        if len(raw) > 8192:
            return JSONResponse({"code": "sdk_request_too_large"}, status_code=413)
    try:
        scope = parse_scope(json.loads(raw))
        await asyncio.to_thread(remove_session, Path(os.environ["SDK_AGENT_STATE_ROOT"]), scope)
    except SessionCleanupRequired:
        return JSONResponse({"code": "sdk_session_reconciliation_required"}, status_code=409)
    except SessionBusy:
        return JSONResponse({"code": "sdk_session_busy"}, status_code=409)
    except (ValueError, TypeError, KeyError):
        return JSONResponse({"code": "sdk_request_invalid"}, status_code=400)
    except Exception:
        return JSONResponse({"code": "sdk_deletion_incomplete"}, status_code=503)
    return JSONResponse({"deleted": True}, headers={"Cache-Control": "no-store"})


class Sweeper:
    def __init__(self, root, ttl):
        self.root, self.ttl, self.entries = root, ttl, None

    def close(self):
        if self.entries is not None:
            self.entries.close()
            self.entries = None

    def tick(self, batch=100):
        """Scan at most batch directory entries; resume scanning on the next tick."""
        if self.entries is None:
            self.entries = os.scandir(self.root)
        removed = 0
        for _ in range(batch):
            entry = next(self.entries, None)
            if entry is None:
                self.close()
                break
            if not SCOPE.fullmatch(entry.name) or not entry.is_dir(follow_symlinks=False):
                continue
            try:
                removed += remove_session(self.root, entry.name, "expired", time.time() - self.ttl)
            except SessionBusy:
                continue
        return removed


async def sweep_forever():
    global _LAST_SWEEP, _SWEEP_FAILED
    _LAST_SWEEP, _SWEEP_FAILED = None, False
    sweeper = Sweeper(Path(os.environ["SDK_AGENT_STATE_ROOT"]), retention_seconds())
    try:
        while True:
            try:
                task = asyncio.create_task(asyncio.to_thread(sweeper.tick))
                try:
                    await asyncio.shield(task)
                except asyncio.CancelledError:
                    # Finish the bounded filesystem operation before closing its iterator.
                    import anyio
                    with anyio.CancelScope(shield=True):
                        await task
                    raise
                _LAST_SWEEP = time.monotonic()
                _SWEEP_FAILED = False
            except Exception:
                _SWEEP_FAILED = True
                # No paths, prompts, tokens or provider details in logs.
                print("sdk_retention_sweep_failed", flush=True)
            await asyncio.sleep(30)
    finally:
        sweeper.close()
