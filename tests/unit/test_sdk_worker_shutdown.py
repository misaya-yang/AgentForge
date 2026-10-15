"""Real worker ownership and durable refusal with bounded process-boundary doubles.

No listener, process, provider or observer runs. Test-only fixture release never
represents a production reconciliation operation.
"""
import asyncio
import errno
import hashlib
import sys
import threading
import tempfile
import importlib.util
import json
import os
from pathlib import Path
import signal
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("sdk_shutdown_base", Path(__file__).with_name("test_sdk_worker_lifecycle.py"))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
server, lifecycle = base.server, base.lifecycle
supervision = __import__(base.PACKAGE + ".supervision", fromlist=["supervision"])


class WorkerShutdownTests(unittest.IsolatedAsyncioTestCase):
    request = base.WorkerLifecycleTests.request
    state = base.WorkerLifecycleTests.state
    timeout = base.WorkerLifecycleTests.timeout
    assert_unlocked_pending = base.WorkerLifecycleTests.assert_unlocked_pending

    async def asyncSetUp(self):
        await base.WorkerLifecycleTests.asyncSetUp(self)
        self.assertEqual(server.SUPERVISOR.owners, {})
        server.SUPERVISOR.closing = False
        self.assertEqual(supervision.CLEANUP_OBSERVATION_SECONDS, 5)
        self.assertEqual(supervision.SHUTDOWN_OBSERVATION_SECONDS, 5)
        self.stack.enter_context(patch.object(supervision, "CLEANUP_OBSERVATION_SECONDS", 0.08))
        self.stack.enter_context(patch.object(supervision, "SHUTDOWN_OBSERVATION_SECONDS", 0.08))
        self.hold_reap = self.hold_grace = self.deny_signal = False
        self.reap_release, self.grace_release = asyncio.Event(), asyncio.Event()
        self.wait_started, self.grace_started = asyncio.Event(), asyncio.Event()

    async def asyncTearDown(self):
        # Tests must explicitly drain every deliberately blocked task first.
        self.assertEqual({t for t in asyncio.all_tasks() if t is not asyncio.current_task() and not t.done()}, set())
        self.assertEqual(server.SUPERVISOR.owners, {})
        server.SUPERVISOR.closing = False

    async def sleep(self, delay):
        if delay == 3:
            self.grace_started.set()
            if self.hold_grace:
                await self.grace_release.wait()
            else:
                await base.REAL_SLEEP(0)
        else:
            await base.REAL_SLEEP(delay)

    async def spawn(self, *args, **kwargs):
        child = await base.WorkerLifecycleTests.spawn(self, *args, **kwargs)
        original = child.wait
        async def wait():
            self.wait_started.set()
            # A completed leader's first wait confirms its result. Its cleanup
            # wait is separately obstructed to test post-result ownership.
            if self.hold_reap and (child.mode != "completed" or child.waits > 0):
                await self.reap_release.wait()
            return await original()
        child.wait = wait
        return child

    def killpg(self, pid, sig):
        if self.deny_signal:
            raise PermissionError("synthetic signal denial")
        if self.hold_reap:
            state = Path(self.spawn_calls[pid - 12345][1]["env"]["HOME"])
            with self.assertRaises(lifecycle.SessionBusy):
                lifecycle.acquire_session(self.root, state.name)
            self.signals.append((pid, sig))
        else:
            base.WorkerLifecycleTests.killpg(self, pid, sig)

    async def started(self, child_count=0):
        async with base.REAL_TIMEOUT(5):
            while len(self.children) <= child_count or not self.children[-1].read_started.is_set():
                await base.REAL_SLEEP(0)

    def owner(self, value):
        return server.SUPERVISOR.owners[self.state(value)]

    def assert_fenced(self, value):
        state = self.state(value)
        self.assertTrue(lifecycle.cleanup_marker(self.root, state.name).exists())
        self.assertTrue((state / "pending.json").exists())
        self.assertFalse(list(state.glob("run-*.json")))
        with self.assertRaises(lifecycle.SessionBusy):
            lifecycle.acquire_session(self.root, state.name)
        with self.assertRaises(lifecycle.SessionBusy):
            lifecycle.remove_session(self.root, state.name)

    async def release_fixture(self, owner, request=None, *, abandon=False, marker=True):
        self.reap_release.set()
        self.grace_release.set()
        for child in self.children:
            child.kill()
        async with base.REAL_TIMEOUT(5):
            if owner.task is not None:
                await asyncio.gather(owner.task, return_exceptions=True)
            tasks = [task for task in [owner.execution, owner.cleanup_task, owner.spawn_task, *owner.watchers] if task is not None]
            await asyncio.gather(*tasks, return_exceptions=True)
            if request is not None:
                await asyncio.gather(request, return_exceptions=True)
        if abandon:
            # Only an intentionally failed process double is discarded here.
            # Production provides no such reconciliation/deletion method.
            self.assertEqual(lifecycle.cleanup_marker(self.root, owner.state.name).exists(), marker)
            owner.lock.close()
            server.SUPERVISOR.owners.pop(owner.state)

    async def test_repeated_raw_cancel_retains_owner_until_observed_cleanup(self):
        self.mode, self.hold_reap = "stdout", True
        value = base.body(True)
        request = asyncio.create_task(self.request(value))
        await self.started()
        owner = self.owner(value)
        try:
            request.cancel()
            async with base.REAL_TIMEOUT(5):
                await self.wait_started.wait()
                request.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await request
            self.assert_fenced(value)
            self.assertFalse(owner.task.done())
            self.assertIsNone(self.children[-1].returncode)
        finally:
            await self.release_fixture(owner, request)
        self.assert_unlocked_pending(value)
        self.assertFalse(lifecycle.cleanup_marker(self.root, self.state(value).name).exists())
        self.assertEqual((await self.request(value))[0], 409)

    async def test_signal_error_fails_closed_without_unlock_or_automatic_retry(self):
        self.mode, self.deny_signal = "stdout", True
        value = base.body()
        request = asyncio.create_task(self.request(value))
        await self.started()
        owner = self.owner(value)
        owner.request_stop()
        try:
            async with base.REAL_TIMEOUT(5):
                status, result = await request
            self.assertEqual((status, result), (502, supervision.FAILURE))
            self.assert_fenced(value)
            self.assertTrue(owner.task.done())
            self.assertTrue(owner.uncertain)
            self.assertFalse(server.SUPERVISOR.ready(self.root))
            count = len(self.spawn_calls)
            status, result = await self.request(base.body(session_id="another"))
            self.assertEqual((status, result["code"]), (503, "sdk_state_unavailable"))
            self.assertEqual(len(self.spawn_calls), count)
        finally:
            await self.release_fixture(owner, request, abandon=True)

    async def test_uvicorn_cancellation_during_grace_never_commits_or_unlocks(self):
        from uvicorn import Config, Server
        self.stack.enter_context(patch.object(supervision, "CLEANUP_OBSERVATION_SECONDS", 0.5))
        self.hold_grace = True
        value = base.body(True)
        request = asyncio.create_task(self.request(value))
        async with base.REAL_TIMEOUT(5):
            await self.grace_started.wait()
        owner = self.owner(value)
        class Lifespan:
            async def shutdown(self):
                return None
        runtime = Server(Config(server.app, timeout_graceful_shutdown=0, log_config=None))
        runtime.servers, runtime.lifespan = [], Lifespan()
        runtime.server_state.tasks.add(request)
        request.add_done_callback(runtime.server_state.tasks.discard)
        try:
            self.assert_fenced(value)
            async with base.REAL_TIMEOUT(5):
                await runtime.shutdown()
                with self.assertRaises(asyncio.CancelledError):
                    await request
            self.assert_fenced(value)
            self.assertEqual([sig for _, sig in self.signals], [signal.SIGTERM])
            self.assertFalse(owner.cleanup_task.done())
        finally:
            await self.release_fixture(owner, request)
        self.assertEqual([sig for _, sig in self.signals], [signal.SIGTERM, signal.SIGKILL])
        self.assert_unlocked_pending(value)

    async def test_cleanup_budget_returns_one_error_and_never_late_success(self):
        for stream in [False, True]:
            with self.subTest(stream=stream):
                self.hold_reap = True
                self.reap_release.clear()
                self.wait_started.clear()
                value = base.body(stream, session_id=str(stream))
                request = asyncio.create_task(self.request(value))
                count = len(self.children)
                await self.started(count)
                owner = self.owner(value)
                try:
                    async with base.REAL_TIMEOUT(5):
                        status, result = await request
                    self.assertEqual(status, 200 if stream else 502)
                    self.assertEqual(result, [supervision.FAILURE] if stream else supervision.FAILURE)
                    self.assert_fenced(value)
                    self.assertFalse(server.SUPERVISOR.ready(self.root))
                    self.assertFalse(owner.task.done())
                finally:
                    await self.release_fixture(owner, request)
                self.assert_unlocked_pending(value)
                self.assertEqual(owner.result, supervision.FAILURE)
                self.assertTrue(server.SUPERVISOR.ready(self.root))

    async def test_hostile_watcher_cannot_delay_signals_or_response_budget(self):
        self.mode, self.test_deadline = "stdout", 0.04
        started, cancelled, release = asyncio.Event(), asyncio.Event(), asyncio.Event()
        async def disconnected(request):
            started.set()
            try:
                await release.wait()
            except asyncio.CancelledError:
                cancelled.set()
                await release.wait()
                raise
            return False
        value = base.body()
        with patch.object(server.Request, "is_disconnected", disconnected):
            request = asyncio.create_task(self.request(value))
            async with base.REAL_TIMEOUT(5):
                await started.wait()
            owner = self.owner(value)
            try:
                async with base.REAL_TIMEOUT(5):
                    await cancelled.wait()
                    status, result = await request
                self.assertEqual((status, result), (502, supervision.FAILURE))
                self.assertEqual([sig for _, sig in self.signals], [signal.SIGTERM, signal.SIGKILL])
                self.assert_fenced(value)
            finally:
                release.set()
                await self.release_fixture(owner, request)
        self.assert_unlocked_pending(value)

    async def test_actual_lifespan_drains_normal_owner_and_reports_uncertain_expiry(self):
        for stuck in [False, True]:
            with self.subTest(stuck=stuck):
                server.SUPERVISOR.closing = False
                self.mode, self.hold_reap = "stdout", stuck
                self.reap_release.clear()
                self.wait_started.clear()
                async def sweep():
                    await asyncio.Event().wait()
                with patch.object(server, "configuration", return_value=base.TOKEN), patch.object(server.os, "umask", return_value=0o077), patch.object(server, "sweep_forever", sweep):
                    lifetime = server.lifespan(server.app)
                    await lifetime.__aenter__()
                    count = len(self.children)
                    value = base.body(session_id=str(stuck))
                    request = asyncio.create_task(self.request(value))
                    await self.started(count)
                    owner = self.owner(value)
                    try:
                        async with base.REAL_TIMEOUT(5):
                            if stuck:
                                with self.assertRaisesRegex(RuntimeError, "sdk_cleanup_unresolved"):
                                    await lifetime.__aexit__(None, None, None)
                                self.assert_fenced(value)
                            else:
                                await lifetime.__aexit__(None, None, None)
                                self.assertTrue(owner.released)
                            status, result = await request
                            self.assertEqual((status, result), (502, supervision.FAILURE))
                            self.assertFalse(server.SUPERVISOR.ready(self.root))
                    finally:
                        await self.release_fixture(owner, request)
                        await lifetime.__aexit__(None, None, None)
                self.assert_unlocked_pending(value)

    async def test_restart_marker_precedes_receipt_replay_and_blocks_deletion_sweep(self):
        value = base.body()
        state, lock = lifecycle.acquire_session(self.root, self.state(value).name)
        server.atomic_json(state / "pending.json", {"request_id": value["request_id"]})
        lifecycle.begin_cleanup(state)
        run_key = hashlib.sha256(value["request_id"].encode()).hexdigest()
        fingerprint = hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        server.atomic_json(state / ("run-" + run_key + ".json"), {"fingerprint": fingerprint, "result": base.COMPLETED})
        lock.close()  # A process exit drops flock; the durable refusal must survive.
        self.assertFalse(supervision.RunSupervisor().ready(self.root))
        status, result = await self.request(value)
        self.assertEqual((status, result["code"]), (409, "sdk_session_reconciliation_required"))
        with self.assertRaises(lifecycle.SessionCleanupRequired):
            lifecycle.remove_session(self.root, state.name)
        os.utime(state / ".activity", (0, 0))
        sweeper = lifecycle.Sweeper(self.root, 3600)
        try:
            self.assertEqual(sweeper.tick(), 0)
        finally:
            sweeper.close()
        self.assertTrue(state.exists())
        self.assertFalse(lifecycle.marker(self.root, state.name).exists())
        (state / "pending.json").unlink()
        status, result = await self.request(value)
        self.assertEqual((status, result["code"]), (409, "sdk_session_reconciliation_required"))
        self.assertEqual(self.spawn_calls, [])

    async def test_legacy_unknown_deletion_refused_but_observed_cleanup_allows_it(self):
        for observed in [False, True]:
            with self.subTest(observed=observed):
                value = base.body(session_id=str(observed))
                state, lock = lifecycle.acquire_session(self.root, self.state(value).name)
                server.atomic_json(state / "pending.json", {"request_id": value["request_id"]})
                if observed:
                    lifecycle.begin_cleanup(state)
                    lifecycle.complete_cleanup(state)
                lock.close()
                if observed:
                    self.assertTrue(lifecycle.remove_session(self.root, state.name))
                    self.assertEqual(json.loads(lifecycle.marker(self.root, state.name).read_text())["reason"], "deleted_unknown")
                else:
                    with self.assertRaises(lifecycle.SessionCleanupRequired):
                        lifecycle.remove_session(self.root, state.name)
                    self.assertTrue(state.exists())

    async def test_authority_admission_cannot_register_after_shutdown(self):
        admitted, release = asyncio.Event(), asyncio.Event()
        async def authority(launch):
            admitted.set()
            await release.wait()
        value = base.body()
        with patch.object(server, "ensure_access", authority):
            request = asyncio.create_task(self.request(value))
            try:
                async with base.REAL_TIMEOUT(5):
                    await admitted.wait()
                    self.assertTrue(await server.SUPERVISOR.shutdown())
                    release.set()
                    status, result = await request
                self.assertEqual((status, result["code"]), (503, "sdk_state_unavailable"))
                self.assertFalse((self.state(value) / "pending.json").exists())
                self.assertEqual(self.spawn_calls, [])
                _, lock = lifecycle.acquire_session(self.root, self.state(value).name)
                lock.close()
            finally:
                release.set()
                await asyncio.gather(request, return_exceptions=True)

    async def test_late_spawn_handle_and_definitive_missing_interpreter_cleanup(self):
        for missing in [False, True]:
            with self.subTest(missing=missing):
                self.test_deadline = 0.04
                entered, release = asyncio.Event(), asyncio.Event()
                async def spawn(*args, **kwargs):
                    entered.set()
                    await release.wait()
                    if missing:
                        raise FileNotFoundError(errno.ENOENT, "synthetic missing interpreter", sys.executable)
                    return await self.spawn(*args, **kwargs)
                value = base.body(session_id=str(missing))
                with patch.object(server.asyncio, "create_subprocess_exec", spawn):
                    request = asyncio.create_task(self.request(value))
                    async with base.REAL_TIMEOUT(5):
                        await entered.wait()
                    owner = self.owner(value)
                    try:
                        async with base.REAL_TIMEOUT(5):
                            status, result = await request
                        self.assertEqual((status, result), (502, supervision.FAILURE))
                        self.assert_fenced(value)
                        self.assertFalse(owner.spawn_task.done())
                    finally:
                        release.set()
                        await self.release_fixture(owner, request)
                self.assert_unlocked_pending(value)
                self.assertTrue(owner.released)
                self.assertTrue(server.SUPERVISOR.ready(self.root))
                self.assertEqual(owner.result, supervision.FAILURE)

    async def test_actual_retention_thread_is_observed_with_same_shutdown_budget(self):
        entered, release, exited = threading.Event(), threading.Event(), threading.Event()
        def tick(sweeper):
            entered.set()
            try:
                release.wait()
                return 0
            finally:
                exited.set()
        with patch.object(server, "configuration", return_value=base.TOKEN), patch.object(server.os, "umask", return_value=0o077), patch.object(lifecycle.Sweeper, "tick", tick), patch.dict(os.environ, {"SDK_AGENT_RETENTION_SECONDS": "3600"}):
            lifetime = server.lifespan(server.app)
            await lifetime.__aenter__()
            try:
                async with base.REAL_TIMEOUT(5):
                    while not entered.is_set():
                        await base.REAL_SLEEP(0)
                    with self.assertRaisesRegex(RuntimeError, "sdk_cleanup_unresolved"):
                        await lifetime.__aexit__(None, None, None)
                    self.assertFalse(server.SUPERVISOR.retention_task.done())
                    self.assertFalse(exited.is_set())
                    self.assertFalse(server.SUPERVISOR.ready(self.root))
            finally:
                release.set()
                async with base.REAL_TIMEOUT(5):
                    await asyncio.gather(server.SUPERVISOR.retention_task, return_exceptions=True)
                await lifetime.__aexit__(None, None, None)
            self.assertTrue(exited.is_set())

    async def test_commit_fsync_failure_retains_refusal_even_after_pending_unlink(self):
        value = base.body()
        original = server.fsync_directory
        def fsync(path):
            if path == self.state(value) and list(path.glob("run-*.json")) and not (path / "pending.json").exists():
                raise OSError("synthetic commit fsync failure")
            original(path)
        with patch.object(server, "fsync_directory", fsync):
            status, result = await self.request(value)
        owner = self.owner(value)
        try:
            self.assertEqual((status, result), (502, supervision.FAILURE))
            self.assertFalse(owner.committed)
            self.assertTrue(owner.commit_started)
            self.assertTrue(lifecycle.cleanup_marker(self.root, owner.state.name).exists())
            self.assertFalse((owner.state / "pending.json").exists())
            self.assertEqual(len(list(owner.state.glob("run-*.json"))), 1)
            self.assertFalse(server.SUPERVISOR.ready(self.root))
        finally:
            await self.release_fixture(owner, abandon=True)
        status, result = await self.request(value)
        self.assertEqual((status, result["code"]), (409, "sdk_session_reconciliation_required"))
        with self.assertRaises(lifecycle.SessionCleanupRequired):
            lifecycle.remove_session(self.root, self.state(value).name)

    async def test_postcommit_release_fsync_failure_preserves_known_result(self):
        value = base.body()
        original = lifecycle.sync_directory
        def fsync(path):
            if path == self.root / ".cleanup" and not lifecycle.cleanup_marker(self.root, self.state(value).name).exists():
                raise OSError("synthetic cleanup directory fsync failure")
            original(path)
        with patch.object(lifecycle, "sync_directory", fsync):
            status, result = await self.request(value)
        owner = self.owner(value)
        try:
            self.assertEqual((status, result), (200, base.COMPLETED))
            self.assertTrue(owner.committed)
            self.assertTrue(owner.uncertain)
            self.assertFalse(server.SUPERVISOR.ready(self.root))
            with self.assertRaises(lifecycle.SessionBusy):
                lifecycle.acquire_session(self.root, owner.state.name)
        finally:
            await self.release_fixture(owner, abandon=True, marker=False)
        # The process/commit outcome is known. Replay is safe if the completed
        # cleanup marker removal is what survives a process exit.
        status, result = await self.request(value)
        self.assertEqual((status, result), (200, base.COMPLETED))
        self.assertEqual(len(self.spawn_calls), 1)

    async def test_begin_fence_failure_never_spawns_and_releases_unstarted_owner(self):
        for stream in [False, True]:
            with self.subTest(stream=stream):
                value = base.body(stream, session_id=str(stream))
                with patch.object(supervision, "begin_cleanup", side_effect=OSError("synthetic fence failure")):
                    status, result = await self.request(value)
                self.assertEqual(status, 200 if stream else 502)
                self.assertEqual(result, [supervision.FAILURE] if stream else supervision.FAILURE)
                self.assertEqual(self.spawn_calls, [])
                self.assert_unlocked_pending(value)
                self.assertEqual(server.SUPERVISOR.owners, {})

    async def test_internal_stop_bounds_resistant_execution_and_detaches_late_events(self):
        value = base.body()
        state, lock = lifecycle.acquire_session(self.root, self.state(value).name)
        server.atomic_json(state / "pending.json", {"request_id": value["request_id"]})
        entered, release = asyncio.Event(), asyncio.Event()
        async def operation(owner):
            entered.set()
            while not release.is_set():
                try:
                    await release.wait()
                except asyncio.CancelledError:
                    pass
            for _ in range(64):
                await owner.emit({"event": "delta", "text": "late"})
            return supervision.FAILURE
        async def cleanup(owner):
            return None
        owner = server.SUPERVISOR.own(state, lock, operation, cleanup)
        async def consume():
            return [value async for value in owner.iterate()]
        request = asyncio.create_task(consume())
        try:
            async with base.REAL_TIMEOUT(5):
                await entered.wait()
                owner.request_stop()  # Same path used by disconnect/authority watchers.
                result = await request
            self.assertEqual(result, [supervision.FAILURE])
            self.assert_fenced(value)
            self.assertTrue(owner.detached)
            self.assertFalse(server.SUPERVISOR.ready(self.root))
        finally:
            release.set()
            await self.release_fixture(owner, request)
        self.assert_unlocked_pending(value)
        self.assertEqual(list(owner.events), [])
        self.assertEqual(owner.result, supervision.FAILURE)

    async def test_shutdown_observes_cleanup_after_owner_task_is_cancelled(self):
        self.mode, self.hold_grace = "stdout", True
        value = base.body()
        request = asyncio.create_task(self.request(value))
        await self.started()
        owner = self.owner(value)
        owner.task.cancel()
        try:
            async with base.REAL_TIMEOUT(5):
                await self.grace_started.wait()
                await owner.task
                status, result = await request
                self.assertEqual((status, result), (502, supervision.FAILURE))
                shutdown = asyncio.create_task(server.SUPERVISOR.shutdown())
                await base.REAL_SLEEP(0)
                self.assertFalse(shutdown.done())
                self.grace_release.set()
                self.assertFalse(await shutdown)
            self.assertEqual([sig for _, sig in self.signals], [signal.SIGTERM, signal.SIGKILL])
            self.assertTrue(owner.cleanup_task.done())
            self.assert_fenced(value)
        finally:
            await self.release_fixture(owner, request, abandon=True)

    async def test_hard_execution_observation_bounds_cancellation_resistance(self):
        value = base.body()
        state, lock = lifecycle.acquire_session(self.root, self.state(value).name)
        server.atomic_json(state / "pending.json", {"request_id": value["request_id"]})
        release = asyncio.Event()
        async def operation(owner):
            while not release.is_set():
                try:
                    await release.wait()
                except asyncio.CancelledError:
                    pass
            return supervision.FAILURE
        async def cleanup(owner):
            return None
        owner = server.SUPERVISOR.own(state, lock, operation, cleanup, execution_seconds=0.02)
        async def consume():
            return [value async for value in owner.iterate()]
        request = asyncio.create_task(consume())
        try:
            async with base.REAL_TIMEOUT(5):
                result = await request
            self.assertEqual(result, [supervision.FAILURE])
            self.assertTrue(owner.stopping)
            self.assertTrue(owner.uncertain)
            self.assert_fenced(value)
        finally:
            release.set()
            await self.release_fixture(owner, request)
        self.assert_unlocked_pending(value)

    async def test_owner_cancel_at_committed_execution_preserves_known_result(self):
        value = base.body()
        original = server.atomic_json
        def atomic(path, value):
            original(path, value)
            if path.name.startswith("run-"):
                owner = server.SUPERVISOR.owners[path.parent]
                asyncio.current_task().add_done_callback(lambda task: owner.task.cancel())
        with patch.object(server, "atomic_json", atomic):
            status, result = await self.request(value)
        owner = self.owner(value)
        try:
            self.assertEqual((status, result), (200, base.COMPLETED))
            self.assertTrue(owner.committed)
            self.assertTrue(owner.uncertain)
            self.assertTrue(lifecycle.cleanup_marker(self.root, owner.state.name).exists())
            self.assertFalse(server.SUPERVISOR.ready(self.root))
            with self.assertRaises(lifecycle.SessionBusy):
                lifecycle.acquire_session(self.root, owner.state.name)
        finally:
            await self.release_fixture(owner, abandon=True)

    def fresh_fixture_root(self):
        self.root = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        os.environ["SDK_AGENT_STATE_ROOT"] = str(self.root)

    async def test_begin_marker_actual_fsync_failures_never_open_replay_or_deletion(self):
        for failure in range(1, 5):
            with self.subTest(fsync=failure):
                self.fresh_fixture_root()
                value = base.body()
                state, lock = lifecycle.acquire_session(self.root, self.state(value).name)
                server.atomic_json(state / "pending.json", {"request_id": value["request_id"]})
                (state / ".cleanup-complete").write_text("sdk-cleanup/v1\n")
                original, calls = os.fsync, 0
                def fsync(fd):
                    nonlocal calls
                    calls += 1
                    if calls == failure:
                        raise OSError("synthetic begin fsync failure")
                    original(fd)
                try:
                    with patch.object(lifecycle.os, "fsync", fsync):
                        with self.assertRaises(OSError):
                            lifecycle.begin_cleanup(state)
                finally:
                    lock.close()
                self.assertEqual(calls, failure)
                self.assertFalse((state / ".cleanup-complete").exists())
                self.assertTrue((state / "pending.json").exists())
                self.assertEqual(lifecycle.cleanup_marker(self.root, state.name).exists(), failure >= 3)
                status, _ = await self.request(value)
                self.assertIn(status, (409, 503))
                with self.assertRaises(lifecycle.SessionCleanupRequired):
                    lifecycle.remove_session(self.root, state.name)
                self.assertEqual(self.spawn_calls, [])

    async def test_complete_marker_fsync_and_unlink_failure_state_matrix(self):
        for failure in (1, 2, 3, "unlink"):
            with self.subTest(failure=failure):
                self.fresh_fixture_root()
                value = base.body()
                state, lock = lifecycle.acquire_session(self.root, self.state(value).name)
                server.atomic_json(state / "pending.json", {"request_id": value["request_id"]})
                lifecycle.begin_cleanup(state)
                active = lifecycle.cleanup_marker(self.root, state.name)
                original_fsync, original_unlink, calls = os.fsync, Path.unlink, 0
                def fsync(fd):
                    nonlocal calls
                    calls += 1
                    if calls == failure:
                        raise OSError("synthetic complete fsync failure")
                    original_fsync(fd)
                def unlink(path, *args, **kwargs):
                    if failure == "unlink" and path == active:
                        raise OSError("synthetic active-marker unlink failure")
                    return original_unlink(path, *args, **kwargs)
                try:
                    with patch.object(lifecycle.os, "fsync", fsync), patch.object(Path, "unlink", unlink):
                        with self.assertRaises(OSError):
                            lifecycle.complete_cleanup(state)
                finally:
                    lock.close()
                marker_remains = failure != 3
                self.assertEqual(active.exists(), marker_remains)
                self.assertTrue((state / "pending.json").exists())
                status, result = await self.request(value)
                self.assertEqual((status, result["code"]), (409, "sdk_session_reconciliation_required"))
                if marker_remains:
                    with self.assertRaises(lifecycle.SessionCleanupRequired):
                        lifecycle.remove_session(self.root, state.name)
                else:
                    # The caller observed local cleanup; durable execution stays
                    # unknown/pending even when final marker-directory sync fails.
                    self.assertFalse(lifecycle.cleanup_unknown(self.root, state.name, state))
                self.assertEqual(self.spawn_calls, [])

    async def test_actual_terminal_fsync_failures_keep_commit_uncertainty_fenced(self):
        for failure in range(1, 4):
            with self.subTest(fsync=failure):
                self.fresh_fixture_root()
                value = base.body()
                original, calls = os.fsync, 0
                def fsync(fd):
                    nonlocal calls
                    owner = server.SUPERVISOR.owners.get(self.state(value))
                    if owner is not None and owner.commit_started and not owner.committed:
                        calls += 1
                        if calls == failure:
                            raise OSError("synthetic terminal fsync failure")
                    original(fd)
                with patch.object(server.os, "fsync", fsync):
                    status, result = await self.request(value)
                owner = self.owner(value)
                try:
                    self.assertEqual(calls, failure)
                    self.assertEqual((status, result), (502, supervision.FAILURE))
                    self.assertFalse(owner.committed)
                    self.assertTrue(lifecycle.cleanup_marker(self.root, owner.state.name).exists())
                    self.assertEqual((owner.state / "pending.json").exists(), failure < 3)
                    self.assertEqual(len(list(owner.state.glob("run-*.json"))), 0 if failure == 1 else 1)
                    self.assertFalse(server.SUPERVISOR.ready(self.root))
                finally:
                    await self.release_fixture(owner, abandon=True)
                status, result = await self.request(value)
                self.assertEqual((status, result["code"]), (409, "sdk_session_reconciliation_required"))
                with self.assertRaises(lifecycle.SessionCleanupRequired):
                    lifecycle.remove_session(self.root, self.state(value).name)

    async def test_pending_unlink_failure_keeps_receipt_and_unknown_fenced(self):
        value = base.body()
        original = Path.unlink
        def unlink(path, *args, **kwargs):
            if path == self.state(value) / "pending.json":
                raise OSError("synthetic pending unlink failure")
            return original(path, *args, **kwargs)
        with patch.object(Path, "unlink", unlink):
            status, result = await self.request(value)
        owner = self.owner(value)
        try:
            self.assertEqual((status, result), (502, supervision.FAILURE))
            self.assertTrue(owner.commit_started)
            self.assertFalse(owner.committed)
            self.assertTrue((owner.state / "pending.json").exists())
            self.assertEqual(len(list(owner.state.glob("run-*.json"))), 1)
            self.assertTrue(lifecycle.cleanup_marker(self.root, owner.state.name).exists())
        finally:
            await self.release_fixture(owner, abandon=True)
        self.assertEqual((await self.request(value))[0], 409)

    async def test_crash_window_completed_proof_does_not_override_active_marker(self):
        value = base.body()
        state, lock = lifecycle.acquire_session(self.root, self.state(value).name)
        lifecycle.begin_cleanup(state)
        (state / ".cleanup-complete").write_text("sdk-cleanup/v1\n")
        lock.close()
        with self.assertRaises(lifecycle.SessionCleanupRequired):
            lifecycle.acquire_session(self.root, state.name)
        with self.assertRaises(lifecycle.SessionCleanupRequired):
            lifecycle.remove_session(self.root, state.name)
        self.assertFalse(server.SUPERVISOR.ready(self.root))


if __name__ == "__main__":
    unittest.main()
