"""Private ASGI host. Install behind operator TLS/mTLS, never publish directly."""
import asyncio
import errno
import anyio
from contextlib import aclosing, asynccontextmanager
import hashlib
import hmac
import json
import os
from pathlib import Path
import signal
import sys
from starlette.applications import Starlette
from starlette.requests import Request
from starlette.responses import JSONResponse, StreamingResponse
from starlette.routing import Route
from .contract import Launch, PROTOCOL
from .launch_frame import encode_launch_frame, LaunchFrameTooLarge
from .lifecycle import (acquire_session, SessionBusy, SessionCleanupRequired, SessionDeleted, touch_activity,
                        delete_session, sweep_forever, retention_ready)
from .supervision import RunSupervisor
from .runtime import configuration
from .knowledge import ensure_profile, ensure_access
from .capabilities import ensure_access as ensure_execution_active
from urllib.error import HTTPError

MAX_BODY = 393216
MAX_EVENT = 1048576
MAX_OUTPUT = 4194304
SUPERVISOR = RunSupervisor()

def authorize(request):
    token = os.environ.get("SDK_AGENT_WORKER_TOKEN", "")
    return len(token) >= 32 and hmac.compare_digest(request.headers.get("authorization", ""), "Bearer " + token)

def atomic_json(path, value):
    tmp = path.with_suffix(".tmp")
    with tmp.open("w") as target:
        json.dump(value, target, separators=(",", ":"))
        target.flush()
        os.fsync(target.fileno())
    tmp.replace(path)
    fsync_directory(path.parent)


def fsync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)

async def stop_child(child):
    # Descendants may outlive the Python leader. Address the entire process group
    # even when returncode is already set, and always follow TERM with bounded KILL.
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        await child.wait()
        return
    await asyncio.sleep(3)
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    await child.wait()

class OwnedStreamingResponse(StreamingResponse):
    def __init__(self, *args, release=None, **kwargs):
        super().__init__(*args, **kwargs)
        self.release = release

    async def __call__(self, scope, receive, send):
        try:
            await super().__call__(scope, receive, send)
        finally:
            with anyio.CancelScope(shield=True):
                try:
                    close = getattr(self.body_iterator, "aclose", None)
                    if close is not None:
                        await close()
                finally:
                    if self.release is not None:
                        self.release()

async def runs(request: Request):
    if not authorize(request):
        return JSONResponse({"code": "sdk_worker_unauthorized"}, status_code=401)
    raw = bytearray()
    async for piece in request.stream():
        if len(piece) > MAX_BODY - len(raw):
            return JSONResponse({"code": "sdk_request_too_large"}, status_code=413)
        raw.extend(piece)
    try:
        body = json.loads(raw)
        launch = Launch.parse(body, request.headers.get("x-sdk-provider-key", ""))
        launch_frame = encode_launch_frame(body, launch.api_key)
    except LaunchFrameTooLarge:
        return JSONResponse({"code": "sdk_request_too_large"}, status_code=413)
    except (ValueError, TypeError, KeyError):
        return JSONResponse({"code": "sdk_request_invalid"}, status_code=400)
    root = Path(os.environ["SDK_AGENT_STATE_ROOT"])
    try:
        state, lock = acquire_session(root, launch.scope)
    except SessionCleanupRequired:
        return JSONResponse({"code": "sdk_session_reconciliation_required"}, status_code=409)
    except SessionBusy:
        return JSONResponse({"code": "sdk_session_busy"}, status_code=409)
    except SessionDeleted:
        return JSONResponse({"code": "sdk_session_deleted"}, status_code=410)
    try:
        if not SUPERVISOR.ready(root):
            lock.close()
            return JSONResponse({"code": "sdk_state_unavailable"}, status_code=503)
        try:
            ensure_profile(state, launch)
        except PermissionError:
            lock.close()
            return JSONResponse({"code": "sdk_session_profile_changed"}, status_code=409)
        try:
            await ensure_access(launch)
        except PermissionError:
            lock.close()
            return JSONResponse({"code": "sdk_knowledge_access_denied"}, status_code=403)
        except HTTPError as error:
            lock.close()
            status = 403 if error.code in (401, 403) else 503
            return JSONResponse({"code": "sdk_knowledge_unavailable"}, status_code=status)
        except Exception:
            lock.close()
            return JSONResponse({"code": "sdk_knowledge_unavailable"}, status_code=503)
        # Authority admission awaited external work; shutdown may have started.
        if not SUPERVISOR.ready(root):
            lock.close()
            return JSONResponse({"code": "sdk_state_unavailable"}, status_code=503)
        canonical = dict(body)
        if isinstance(body.get("read_bridge"), dict):
            canonical["read_bridge"] = {k: v for k, v in body["read_bridge"].items() if k != "grant"}
        fingerprint = hashlib.sha256(json.dumps(canonical, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        run_key = hashlib.sha256(launch.request_id.encode()).hexdigest()
        receipt = state / ("run-" + run_key + ".json")
        if isinstance(body.get("capability_bridge"), dict):
            canonical["capability_bridge"] = {k: v for k, v in body["capability_bridge"].items() if k != "grant"}
            fingerprint = hashlib.sha256(json.dumps(canonical, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()).hexdigest()
        pending = state / "pending.json"
        if pending.exists():
            lock.close()
            return JSONResponse({"code": "sdk_session_reconciliation_required"}, status_code=409)
        if receipt.exists():
            prior = json.loads(receipt.read_text())
            lock.close()
            if prior["fingerprint"] != fingerprint:
                return JSONResponse({"code": "sdk_request_id_reused"}, status_code=409)
            if launch.stream:
                async def replay():
                    yield "data: " + json.dumps(prior["result"]) + "\n\n"
                return OwnedStreamingResponse(replay(), media_type="text/event-stream")
            return JSONResponse(prior["result"])
        touch_activity(state)
        atomic_json(pending, {"request_id": launch.request_id, "fingerprint": fingerprint})
    except asyncio.CancelledError:
        lock.close()
        raise
    except Exception:
        lock.close()
        return JSONResponse({"code": "sdk_state_unavailable"}, status_code=503)

    async def cleanup(owner):
        # Watchers notify the execution owner; only this task signals the child.
        for watcher in owner.watchers:
            watcher.cancel()
        spawn = owner.spawn_task
        if owner.child is None and spawn is not None:
            try:
                owner.child = await asyncio.shield(spawn)
            except FileNotFoundError as error:
                # A missing fixed interpreter cannot have executed the SDK child.
                # Other launch failures/cancellations have no proven group outcome.
                if error.errno != errno.ENOENT or error.filename != sys.executable:
                    raise
        if owner.child is not None:
            # Signal before waiting for watcher cancellation to finish.
            await stop_child(owner.child)
        if owner.watchers:
            await asyncio.gather(*owner.watchers, return_exceptions=True)

    async def execute(owner):
        output_bytes = 0
        # Do not inherit provider keys, cloud credentials, proxies or user settings.
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(state),
               "PYTHONUNBUFFERED": "1", "LANG": "C.UTF-8", "OPENAI_AGENTS_DISABLE_TRACING": "1"}
        if os.environ.get("SDK_AGENT_GATEWAY_URL"):
            env["SDK_AGENT_GATEWAY_URL"] = os.environ["SDK_AGENT_GATEWAY_URL"]
        async with asyncio.timeout(launch.capability_bridge["timeout_seconds"] if launch.capability_bridge is not None else 300):
            owner.spawn_task = asyncio.create_task(asyncio.create_subprocess_exec(sys.executable, "-m", "sdk_agent_worker.child",
                stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.DEVNULL, env=env, start_new_session=True, limit=MAX_EVENT))
            child = owner.child = await asyncio.shield(owner.spawn_task)
            if not launch.stream:
                async def watch_disconnect():
                    while not owner.stopping and owner.cleanup_task is None:
                        if await request.is_disconnected():
                            owner.request_stop()
                            return
                        if owner.stopping or owner.cleanup_task is not None:
                            return
                        await asyncio.sleep(0.25)
                owner.watchers.append(asyncio.create_task(watch_disconnect()))
            if launch.capability_bridge is not None:
                async def watch_authority():
                    from .capabilities import ensure_access as capability_access
                    try:
                        while not owner.stopping and owner.cleanup_task is None:
                            await asyncio.sleep(10)
                            if owner.stopping or owner.cleanup_task is not None:
                                return
                            await capability_access(launch)
                    except asyncio.CancelledError:
                        raise
                    except Exception:
                        owner.request_stop()
                owner.watchers.append(asyncio.create_task(watch_authority()))
            child.stdin.write(launch_frame)
            await child.stdin.drain()
            child.stdin.close()
            while line := await child.stdout.readline():
                output_bytes += len(line)
                if len(line) > MAX_EVENT or output_bytes > MAX_OUTPUT:
                    raise ValueError("sdk_output_limit")
                value = json.loads(line)
                if value.get("event") == "completed":
                    if not isinstance(value.get("text"), str) or not isinstance(value.get("usage"), dict):
                        raise ValueError("sdk_terminal_invalid")
                    code = await child.wait()
                    if code != 0:
                        raise ValueError("sdk_child_failed")
                    # Cleanup must complete before success/receipt publication.
                    await owner.cleaned()
                    if owner.stopping:
                        raise ValueError("sdk_execution_cancelled")
                    await ensure_execution_active(launch)
                    if owner.stopping or owner.finished:
                        raise ValueError("sdk_execution_cancelled")
                    owner.commit_started = True
                    atomic_json(receipt, {"fingerprint": fingerprint, "result": value})
                    pending.unlink()
                    fsync_directory(state)
                    owner.committed_result = value
                    owner.committed = True
                    return value
                if value.get("event") == "error":
                    raise ValueError("sdk_child_failed")
                if value.get("event") in {"approval_required", "approval_result", "tool_result"}:
                    if launch.capability_bridge is None or not launch.capability_bridge["tools"] or value.get("run_id") != launch.capability_bridge["run_id"] or not isinstance(value.get("tool_call_id"), str):
                        raise ValueError("sdk_event_invalid")
                    await owner.emit(value)
                    continue
                if value.get("event") != "delta" or not isinstance(value.get("text"), str):
                    raise ValueError("sdk_event_invalid")
                await owner.emit(value)
        raise ValueError("sdk_stream_incomplete")

    try:
        owner = SUPERVISOR.own(state, lock, execute, cleanup,
            launch.capability_bridge["timeout_seconds"] if launch.capability_bridge is not None else 300)
    except Exception:
        lock.close()
        return JSONResponse({"code": "sdk_state_unavailable"}, status_code=503)
    if launch.stream:
        async def sse():
            async with aclosing(owner.iterate()) as iterator:
                async for event in iterator:
                    yield "data: " + json.dumps(event, separators=(",", ":")) + "\n\n"
        return OwnedStreamingResponse(sse(), media_type="text/event-stream", headers={"Cache-Control": "no-store"}, release=owner.abort_unstarted)
    async with aclosing(owner.iterate()) as iterator:
        async for event in iterator:
            if event["event"] == "completed":
                return JSONResponse(event, headers={"Cache-Control": "no-store"})
            if event["event"] == "error":
                return JSONResponse(event, status_code=502)
    return JSONResponse({"code": "sdk_stream_incomplete"}, status_code=502)

async def ready(request):
    if not authorize(request):
        return JSONResponse({"code": "sdk_worker_unauthorized"}, status_code=401)
    # Startup gate is a package/version/state-root check, never a paid provider call.
    import importlib.metadata
    try:
        configuration()
        if not retention_ready():
            raise ValueError("sdk_retention_unavailable")
        versions = {name: importlib.metadata.version(name) for name in ("openai-agents", "claude-agent-sdk")}
        if versions != {"openai-agents": "0.23.1", "claude-agent-sdk": "0.2.165"}:
            raise ValueError("sdk_version_mismatch")
        root = Path(os.environ["SDK_AGENT_STATE_ROOT"])
        if not root.is_absolute() or not root.is_dir() or not os.access(root, os.W_OK) or not SUPERVISOR.ready(root):
            raise ValueError("sdk_state_unavailable")
    except Exception:
        return JSONResponse({"ready": False}, status_code=503)
    return JSONResponse({"ready": True, "protocol": PROTOCOL, "versions": versions})

@asynccontextmanager
async def lifespan(app):
    os.environ["SDK_AGENT_WORKER_TOKEN"] = configuration()
    os.environ.pop("SDK_AGENT_WORKER_TOKEN_FILE", None)
    os.umask(0o077)
    if SUPERVISOR.owners or (SUPERVISOR.retention_task is not None and not SUPERVISOR.retention_task.done()):
        raise ValueError("sdk_cleanup_unavailable")
    SUPERVISOR.closing = False
    task = asyncio.create_task(sweep_forever())
    try:
        yield
    finally:
        SUPERVISOR.closing = True
        with anyio.CancelScope(shield=True):
            clean = await SUPERVISOR.shutdown(task)
        if not clean:
            raise RuntimeError("sdk_cleanup_unresolved")

app = Starlette(lifespan=lifespan, routes=[
    Route("/v1/runs", runs, methods=["POST"]),
    Route("/v1/sessions", delete_session, methods=["DELETE"]),
    Route("/health/ready", ready),
])
