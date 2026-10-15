"""Real ASGI/admission/filesystem lifecycle with bounded child-process doubles.

No listener, child process, provider or Gateway is used. Only OS subprocess/signal
boundaries and the Gateway response are doubled; production admission, protocol,
lock/receipt state, streaming response, and stop_child execute. Hard deadlines and
TERM grace are shortened for tests, without changing their cancellation semantics.
"""
import asyncio
from contextlib import ExitStack
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import unittest
from unittest.mock import patch

from starlette.requests import ClientDisconnect


SOURCE = Path(__file__).resolve().parents[2] / "apps/sdk-agent-worker/sdk_agent_worker"
PACKAGE = "_sdk_worker_lifecycle_test_subject"
spec = importlib.util.spec_from_file_location(
    PACKAGE, SOURCE / "__init__.py", submodule_search_locations=[str(SOURCE)]
)
package = importlib.util.module_from_spec(spec)
sys.modules[PACKAGE] = package
spec.loader.exec_module(package)
server = __import__(PACKAGE + ".server", fromlist=["server"])
lifecycle = __import__(PACKAGE + ".lifecycle", fromlist=["lifecycle"])
capabilities = __import__(PACKAGE + ".capabilities", fromlist=["capabilities"])
frame_codec = __import__(PACKAGE + ".launch_frame", fromlist=["launch_frame"])
REAL_TIMEOUT = asyncio.timeout
REAL_SLEEP = asyncio.sleep
TOKEN = "synthetic-worker-token-not-a-secret"
PROVIDER_KEY = "synthetic-provider-key-not-a-secret"
COMPLETED = {"event": "completed", "text": "done", "usage": {}}


def body(stream=False, governed=False, **overrides):
    value = {
        "protocol": server.PROTOCOL, "framework": "openai_agents",
        "service_id": "fixture-service", "tenant_id": "fixture-tenant",
        "user_id": "fixture-user", "request_id": "fixture-request",
        "session_id": "fixture-session", "agent": {
            "model": "fixture-model", "instructions": "Reply briefly.", "max_turns": 2,
        }, "input": "Hello", "stream": stream,
    }
    if governed:
        value["capability_bridge"] = {
            "grant": "a" * 64, "run_id": "00000000-0000-4000-8000-000000000001",
            "timeout_seconds": 1800, "tools": [{
                "name": "platform_capability_0", "tool": "fixture.read",
                "namespace": "fixture", "description": "Read a fixture.",
                "input_schema": {"type": "object"}, "id": "fixture.read.v1",
                "schema_hash": "sha256:" + "b" * 64, "effect": "read",
            }],
        }
    return dict(value, **overrides)


class Child:
    """Protocol-only process double. Real stop_child drives fake group shutdown."""

    def __init__(self, pid=12345, mode="completed", code=0):
        self.pid, self.mode, self.returncode = pid, mode, code
        if mode in {"drain", "stdout", "wait"}:
            self.returncode = None
        self.stdin = self
        self.stdout = self
        self.payload = bytearray()
        self.closed = False
        self.drain_started = asyncio.Event()
        self.read_started = asyncio.Event()
        self.reaped = asyncio.Event()
        self.lines = [json.dumps(COMPLETED).encode() + b"\n"]
        self.waits = 0

    def write(self, value):
        self.payload.extend(value)

    async def drain(self):
        self.drain_started.set()
        if self.mode == "drain":
            await asyncio.Event().wait()

    def close(self):
        self.closed = True

    async def readline(self):
        self.read_started.set()
        if self.mode == "stdout":
            await self.reaped.wait()
            return b""
        return self.lines.pop(0) if self.lines else b""

    async def wait(self):
        self.waits += 1
        if self.returncode is None:
            await self.reaped.wait()
        return self.returncode

    def kill(self):
        self.returncode = -signal.SIGKILL
        self.reaped.set()


class WorkerLifecycleTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.root = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.children = []
        self.signals = []
        self.spawn_calls = []
        self.spawn_cancelled = False
        self.spawn_blocked = False
        self.spawn_release = asyncio.Event()
        self.mode = "completed"
        self.child_code = 0
        self.deadlines = []
        self.test_deadline = 5
        self.stack.enter_context(patch.dict(os.environ, {
            "SDK_AGENT_WORKER_TOKEN": TOKEN, "SDK_AGENT_STATE_ROOT": str(self.root),
            "SDK_AGENT_GATEWAY_URL": "http://127.0.0.1:8080",
        }))
        self.stack.enter_context(patch.object(server.asyncio, "create_subprocess_exec", self.spawn))
        self.stack.enter_context(patch.object(server.os, "killpg", self.killpg))
        self.stack.enter_context(patch.object(server.asyncio, "sleep", self.sleep))
        self.stack.enter_context(patch.object(server.asyncio, "timeout", self.timeout))
        self.stack.enter_context(patch.object(capabilities, "_request", return_value={"authorized": True}))

    async def asyncTearDown(self):
        pending = {task for task in asyncio.all_tasks() if task is not asyncio.current_task() and not task.done()}
        self.assertEqual(pending, set())

    def timeout(self, delay):
        self.deadlines.append(delay)
        return REAL_TIMEOUT(self.test_deadline)

    async def sleep(self, delay):
        await REAL_SLEEP(0 if delay == 3 else delay)

    async def spawn(self, *args, **kwargs):
        self.spawn_calls.append((args, kwargs))
        if self.spawn_blocked:
            try:
                await self.spawn_release.wait()
            except asyncio.CancelledError:
                self.spawn_cancelled = True
                raise
        child = Child(12345 + len(self.children), self.mode, self.child_code)
        self.children.append(child)
        return child

    def killpg(self, pid, sig):
        state = Path(self.spawn_calls[pid - 12345][1]["env"]["HOME"])
        with self.assertRaises(lifecycle.SessionBusy):
            lifecycle.acquire_session(self.root, state.name)
        self.signals.append((pid, sig))
        if sig == signal.SIGKILL:
            next(child for child in self.children if child.pid == pid).kill()

    async def request(self, value=None, *, payloads=None, send_hook=None, token=TOKEN,
                      asgi_version="2.4", disconnect=None):
        messages = []
        pieces = list(payloads if payloads is not None else [json.dumps(value).encode()])
        scope = {
            "type": "http", "asgi": {"version": "3.0", "spec_version": asgi_version},
            "http_version": "1.1", "method": "POST", "scheme": "http",
            "path": "/v1/runs", "raw_path": b"/v1/runs", "query_string": b"",
            "headers": [(b"authorization", ("Bearer " + token).encode()),
                        (b"x-sdk-provider-key", PROVIDER_KEY.encode())],
            "server": ("fixture", 80), "client": ("fixture", 1234),
        }

        async def receive():
            if pieces:
                return {"type": "http.request", "body": pieces.pop(0), "more_body": bool(pieces)}
            await (disconnect or asyncio.Event()).wait()
            return {"type": "http.disconnect"}

        async def send(message):
            messages.append(message)
            if send_hook:
                await send_hook(message)

        await server.app(scope, receive, send)
        status = next(m["status"] for m in messages if m["type"] == "http.response.start")
        raw = b"".join(m.get("body", b"") for m in messages)
        if status == 200 and value and value["stream"]:
            result = [json.loads(line[6:]) for line in raw.splitlines() if line.startswith(b"data: ")]
        else:
            result = json.loads(raw)
        return status, result

    def state(self, value):
        launch = server.Launch.parse(value, PROVIDER_KEY)
        return self.root / launch.scope

    def assert_unlocked_pending(self, value):
        state = self.state(value)
        self.assertEqual(json.loads((state / "pending.json").read_text())["request_id"], value["request_id"])
        self.assertFalse(list(state.glob("run-*.json")))
        _, lock = lifecycle.acquire_session(self.root, state.name)
        lock.close()

    def assert_stopped(self):
        child = self.children[-1]
        self.assertEqual(self.signals[-2:], [(child.pid, signal.SIGTERM), (child.pid, signal.SIGKILL)])
        self.assertTrue(child.reaped.is_set())
        self.assertGreaterEqual(child.waits, 1)

    async def test_deadline_covers_launch_payload_backpressure(self):
        self.test_deadline = 0.04
        for stream, governed in [(False, False), (True, True)]:
            with self.subTest(stream=stream, governed=governed):
                value = body(stream, governed, session_id=str(stream))
                self.mode = "drain"
                async with REAL_TIMEOUT(5):
                    status, result = await self.request(value)
                self.assertEqual(status, 200 if stream else 502)
                self.assertEqual(result, [{"event": "error", "code": "sdk_execution_failed"}] if stream else {"event": "error", "code": "sdk_execution_failed"})
                self.assertTrue(self.children[-1].drain_started.is_set())
                self.assert_unlocked_pending(value)
                self.assert_stopped()
        self.assertEqual(self.deadlines, [300, 1800])

    async def test_deadline_covers_spawn_handshake(self):
        self.test_deadline = 0.04
        self.spawn_blocked = True
        value = body()
        supervision = __import__(PACKAGE + ".supervision", fromlist=["supervision"])
        with patch.object(supervision, "CLEANUP_OBSERVATION_SECONDS", 0.04):
            async with REAL_TIMEOUT(5):
                status, result = await self.request(value)
            self.assertEqual(status, 502)
            self.assertEqual(result["code"], "sdk_execution_failed")
            self.assertFalse(self.spawn_cancelled)
            self.assertEqual(self.children, [])
            self.assertEqual(self.signals, [])
            owner = server.SUPERVISOR.owners[self.state(value)]
            try:
                with self.assertRaises(lifecycle.SessionBusy):
                    lifecycle.acquire_session(self.root, self.state(value).name)
                self.assertTrue(lifecycle.cleanup_marker(self.root, self.state(value).name).exists())
            finally:
                # An eventual handle is observed and stopped by its retained owner.
                self.spawn_release.set()
                async with REAL_TIMEOUT(5):
                    await owner.task
            self.assert_unlocked_pending(value)
            self.assert_stopped()

    async def test_deadline_covers_child_output_and_exit(self):
        self.test_deadline = 0.04
        for mode in ["stdout", "wait"]:
            with self.subTest(mode=mode):
                self.mode = mode
                value = body(session_id=mode)
                async with REAL_TIMEOUT(5):
                    status, result = await self.request(value)
                self.assertEqual(status, 502)
                self.assertEqual(result["event"], "error")
                self.assert_unlocked_pending(value)
                self.assert_stopped()

    async def test_stream_cancellation_reaps_before_unlock_and_blocks_retry(self):
        self.mode = "stdout"
        value = body(True)
        task = asyncio.create_task(self.request(value))
        async with REAL_TIMEOUT(5):
            while not self.children or not self.children[-1].read_started.is_set():
                await REAL_SLEEP(0)
            with self.assertRaises(lifecycle.SessionBusy):
                lifecycle.acquire_session(self.root, self.state(value).name)
            task.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await task
        self.assert_stopped()
        self.assert_unlocked_pending(value)
        count = len(self.spawn_calls)
        status, result = await self.request(value)
        self.assertEqual((status, result["code"]), (409, "sdk_session_reconciliation_required"))
        self.assertEqual(len(self.spawn_calls), count)

    async def test_asgi_disconnect_cancels_stream_and_reaps_under_shield(self):
        self.test_deadline = 5
        self.mode = "stdout"
        value = body(True)
        disconnected = asyncio.Event()
        task = asyncio.create_task(self.request(value, asgi_version="2.3", disconnect=disconnected))
        async with REAL_TIMEOUT(5):
            while not self.children or not self.children[-1].read_started.is_set():
                await REAL_SLEEP(0)
            disconnected.set()
            status, result = await task
        self.assertEqual((status, result), (200, []))
        self.assert_stopped()
        self.assert_unlocked_pending(value)

    async def test_nonstream_disconnect_reaps_without_success(self):
        self.test_deadline = 5
        self.mode = "stdout"
        value = body()
        disconnected = asyncio.Event()
        task = asyncio.create_task(self.request(value, disconnect=disconnected))
        async with REAL_TIMEOUT(5):
            while not self.children or not self.children[-1].read_started.is_set():
                await REAL_SLEEP(0)
            disconnected.set()
            status, result = await task
        self.assertEqual((status, result["event"]), (502, "error"))
        self.assert_stopped()
        self.assert_unlocked_pending(value)

    async def test_stream_header_send_failure_releases_unstarted_iterator(self):
        value = body(True)

        async def fail_headers(message):
            if message["type"] == "http.response.start":
                raise OSError("synthetic client disconnect")

        with self.assertRaises(ClientDisconnect):
            await self.request(value, send_hook=fail_headers)
        self.assertEqual(self.spawn_calls, [])
        self.assert_unlocked_pending(value)

    async def test_failed_child_does_not_commit_completed_event(self):
        self.child_code = 1
        value = body()
        status, result = await self.request(value)
        self.assertEqual((status, result["event"]), (502, "error"))
        self.assert_unlocked_pending(value)
        self.assert_stopped()

    async def test_receipt_replay_is_bound_to_request_and_tenant(self):
        first = body()
        status, result = await self.request(first)
        self.assertEqual((status, result), (200, COMPLETED))
        self.assert_stopped()
        state = self.state(first)
        self.assertFalse((state / "pending.json").exists())
        self.assertEqual(len(list(state.glob("run-*.json"))), 1)
        status, result = await self.request(first)
        self.assertEqual((status, result), (200, COMPLETED))
        self.assertEqual(len(self.spawn_calls), 1)
        status, result = await self.request(dict(first, input="Different"))
        self.assertEqual((status, result["code"]), (409, "sdk_request_id_reused"))
        self.assertEqual(len(self.spawn_calls), 1)
        other = dict(first, tenant_id="other-tenant")
        status, result = await self.request(other)
        self.assertEqual((status, result), (200, COMPLETED))
        self.assertNotEqual(self.state(other), state)
        self.assertEqual(len(self.spawn_calls), 2)
        args, options = self.spawn_calls[0]
        self.assertEqual(args, (sys.executable, "-m", "sdk_agent_worker.child"))
        self.assertTrue(options["start_new_session"])
        self.assertEqual(set(options["env"]), {"PATH", "HOME", "PYTHONUNBUFFERED", "LANG", "OPENAI_AGENTS_DISABLE_TRACING", "SDK_AGENT_GATEWAY_URL"})
        self.assertNotIn(PROVIDER_KEY, json.dumps(options))
        self.assertEqual(json.loads(self.children[0].payload)["provider_key"], PROVIDER_KEY)

    async def test_completed_event_is_not_committed_after_authority_revocation(self):
        value = body(True, True)
        with patch.object(capabilities, "_request", side_effect=[{"authorized": True}, {"authorized": False}]):
            status, result = await self.request(value)
        self.assertEqual((status, result), (200, [{"event": "error", "code": "sdk_execution_failed"}]))
        self.assert_unlocked_pending(value)
        self.assert_stopped()

    async def test_denied_admission_never_spawns_or_creates_state(self):
        status, result = await self.request(body(), token="wrong")
        self.assertEqual((status, result["code"]), (401, "sdk_worker_unauthorized"))
        status, result = await self.request(dict(body(), stream="true"))
        self.assertEqual((status, result["code"]), (400, "sdk_request_invalid"))
        self.assertEqual(self.spawn_calls, [])
        self.assertEqual(list(self.root.iterdir()), [])

    async def test_active_session_blocks_same_scope_but_not_other_tenant(self):
        first = body()
        state, lock = lifecycle.acquire_session(self.root, self.state(first).name)
        try:
            status, result = await self.request(first)
            self.assertEqual((status, result["code"]), (409, "sdk_session_busy"))
            status, result = await self.request(dict(first, tenant_id="other-tenant"))
            self.assertEqual((status, result), (200, COMPLETED))
            self.assertEqual(len(self.spawn_calls), 1)
            with self.assertRaises(lifecycle.SessionBusy):
                lifecycle.remove_session(self.root, state.name)
        finally:
            lock.close()
        self.assertTrue(lifecycle.remove_session(self.root, state.name))
        status, result = await self.request(first)
        self.assertEqual((status, result["code"]), (410, "sdk_session_deleted"))
        self.assertEqual(len(self.spawn_calls), 1)

    async def test_body_staging_never_copies_over_limit_chunk(self):
        lengths = []

        class TrackedBytearray(bytearray):
            def extend(self, value):
                super().extend(value)
                lengths.append(len(self))

        for pieces in [[b"x" * (server.MAX_BODY + 1)], [b"x" * server.MAX_BODY, b"x"]]:
            lengths.clear()
            with patch.object(server, "bytearray", TrackedBytearray, create=True):
                status, result = await self.request(payloads=pieces)
            self.assertEqual((status, result["code"]), (413, "sdk_request_too_large"))
            self.assertLessEqual(max(lengths, default=0), server.MAX_BODY)
        self.assertEqual(self.spawn_calls, [])
        self.assertEqual(list(self.root.iterdir()), [])


    async def test_unicode_request_reaches_child_as_complete_utf8_frame(self):
        for index, text in enumerate(["中" * 80000, "😀" * 60000]):
            value = body(input=text, session_id=f"unicode-{index}")
            raw = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()
            self.assertLess(len(raw), server.MAX_BODY)
            status, result = await self.request(value, payloads=[raw])
            self.assertEqual(status, 200)
            self.assertEqual(result["event"], "completed")
            child = self.children[-1]
            self.assertTrue(child.closed)
            decoded = frame_codec.read_launch_frame(io.BytesIO(child.payload))
            self.assertEqual(decoded, {"request": value, "provider_key": PROVIDER_KEY})
            self.assertEqual(server.Launch.parse(decoded["request"], decoded["provider_key"]).input, text)

    async def test_expanded_metadata_budget_rejected_before_session_or_spawn(self):
        value = body(stream=True, governed=True)
        template = value["capability_bridge"]["tools"][0]
        value["capability_bridge"]["tools"] = [dict(template, name=f"platform_capability_{index}",
            tool=f"fixture.read{index}", id=f"fixture.read{index}.v1",
            input_schema={"enum": [1e10] * 4000}) for index in range(16)]
        # Valid JSON exponent spelling is shorter than its canonical float form.
        raw = json.dumps(value, ensure_ascii=False, separators=(",", ":")).replace("10000000000.0", "1e10").encode()
        self.assertLess(len(raw), server.MAX_BODY)
        server.Launch.parse(json.loads(raw), PROVIDER_KEY)
        with patch.object(server, "acquire_session", side_effect=AssertionError("must not touch session")):
            status, result = await self.request(value, payloads=[raw])
        self.assertEqual((status, result["code"]), (413, "sdk_request_too_large"))
        self.assertEqual(self.spawn_calls, [])
        self.assertEqual(list(self.root.iterdir()), [])
        self.assertNotIn(PROVIDER_KEY, json.dumps(result))


if __name__ == "__main__":
    unittest.main()
