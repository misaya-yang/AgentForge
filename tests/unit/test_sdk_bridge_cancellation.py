"""Production bridge cancellation fences with bounded in-process HTTP doubles.

No HTTP listener, provider, subprocess or real credential is used. Actual bridge
request construction, JSON/size checks, asyncio.to_thread, locks and poison logic
run unchanged. Every blocked fake response is explicitly released and all executor
threads are joined before the test ends. These are local semantics, not evidence
that a remote action stops when its awaiting task is cancelled.
"""
import asyncio
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
import importlib.util
import json
import os
from pathlib import Path
import sys
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch


SOURCE = Path(__file__).resolve().parents[2] / "apps/sdk-agent-worker/sdk_agent_worker"
PACKAGE = "_sdk_bridge_cancellation_test_subject"
spec = importlib.util.spec_from_file_location(
    PACKAGE, SOURCE / "__init__.py", submodule_search_locations=[str(SOURCE)]
)
package = importlib.util.module_from_spec(spec)
sys.modules[PACKAGE] = package
spec.loader.exec_module(package)
capabilities = __import__(PACKAGE + ".capabilities", fromlist=["capabilities"])
knowledge = __import__(PACKAGE + ".knowledge", fromlist=["knowledge"])
RUN_ID = "00000000-0000-4000-8000-000000000001"
TOOL = "platform_capability_0"


def launch():
    return SimpleNamespace(capability_bridge={
        "grant": "a" * 64, "run_id": RUN_ID, "tools": [{
            "name": TOOL, "tool": "fixture.write", "namespace": "fixture",
        }],
    }, read_bridge={"grant": "b" * 64, "dataset_ids": ["fixture-dataset"]})


class TrackedExecutor(ThreadPoolExecutor):
    def __init__(self):
        super().__init__(max_workers=3, thread_name_prefix="sdk-bridge-test")
        self.submitted = []

    def submit(self, *args, **kwargs):
        future = super().submit(*args, **kwargs)
        self.submitted.append(future)
        return future


class GatewayDouble:
    """Only the HTTP I/O edge is replaced; actual _request reads this response."""

    def __init__(self, loop):
        self.loop = loop
        self.calls = []
        self.read_sizes = []
        self.closed = []
        self.started = asyncio.Event()
        self.finished = asyncio.Event()
        self.release = threading.Event()
        self.block_call = "active"
        self.raw = None
        self.failure = None
        self.status = "succeeded"

    def open(self, request, *, timeout):
        value = json.loads(request.data)
        call_id = value.get("tool_call_id")
        self.calls.append((call_id, request.full_url, timeout))
        if request.full_url.endswith("/retrieve"):
            raw = json.dumps({"passages": []}).encode()
        else:
            raw = json.dumps({
                "run_id": RUN_ID, "tool_call_id": call_id, "tool": "fixture.write",
                "status": self.status, "execution_id": "fixture-execution",
                "response": {"value": 42},
            }).encode()
        raw = self.raw if self.raw is not None else raw
        gateway = self

        class Response:
            def __enter__(self):
                return self

            def read(self, size):
                gateway.read_sizes.append(size)
                if call_id == gateway.block_call:
                    gateway.loop.call_soon_threadsafe(gateway.started.set)
                    if not gateway.release.wait(5):
                        raise TimeoutError("test_response_was_not_released")
                if gateway.failure is not None:
                    raise gateway.failure
                return raw[:size]

            def __exit__(self, *args):
                gateway.closed.append(call_id)
                if call_id == gateway.block_call:
                    gateway.loop.call_soon_threadsafe(gateway.finished.set)

        return Response()


class BridgeCancellationTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.executor = TrackedExecutor()
        asyncio.get_running_loop().set_default_executor(self.executor)
        self.gateway = GatewayDouble(asyncio.get_running_loop())
        self.stack.enter_context(patch.dict(os.environ, {
            "SDK_AGENT_GATEWAY_URL": "https://gateway.invalid",
        }))
        self.stack.enter_context(patch.object(capabilities, "build_opener", return_value=self.gateway))
        self.stack.enter_context(patch.object(knowledge, "build_opener", return_value=self.gateway))
        self.events = []
        self.tasks = []

        async def emit(value):
            self.events.append(value)

        self.governed = capabilities.GovernedTools(launch(), emit)

    async def asyncTearDown(self):
        self.gateway.release.set()
        for task in self.tasks:
            if not task.done():
                task.cancel()
        await asyncio.gather(*self.tasks, return_exceptions=True)
        if self.gateway.started.is_set():
            await asyncio.wait_for(self.gateway.finished.wait(), 2)
        self.executor.shutdown(wait=True)
        self.assertTrue(all(future.done() for future in self.executor.submitted))
        self.assertEqual(len(self.gateway.calls), len(self.gateway.closed))

    def invoke(self, call_id):
        task = asyncio.create_task(self.governed.invoke(TOOL, {"value": 42}, call_id))
        self.tasks.append(task)
        return task

    async def start_active(self):
        task = self.invoke("active")
        await asyncio.wait_for(self.gateway.started.wait(), 2)
        return task

    async def assert_fenced(self, call_id):
        with self.assertRaisesRegex(RuntimeError, "sdk_capability_reconciliation_required"):
            await asyncio.wait_for(self.invoke(call_id), 2)

    async def test_cancel_active_request_fences_queued_new_and_late_success(self):
        active = await self.start_active()
        queued = self.invoke("queued")
        await asyncio.sleep(0)  # Put the sibling at the production lock.
        self.assertFalse(queued.done())
        self.assertEqual(len(self.gateway.calls), 1)
        active.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await active
        self.assertFalse(self.gateway.release.is_set())
        self.assertFalse(self.gateway.finished.is_set())
        with self.assertRaisesRegex(RuntimeError, "sdk_capability_reconciliation_required"):
            await asyncio.wait_for(queued, 2)
        self.assertTrue(self.governed.poisoned)
        await self.assert_fenced("new")
        await self.assert_fenced("active")  # No automatic replay of original ID.
        self.gateway.release.set()
        await asyncio.wait_for(self.gateway.finished.wait(), 2)
        await self.assert_fenced("after_late_success")
        self.assertEqual([call[0] for call in self.gateway.calls], ["active"])
        self.assertEqual(self.events, [])
        self.assertTrue(self.governed.poisoned)

    async def test_deadline_cancellation_preserves_timeout_and_fences_dispatch(self):
        deadlines = []

        async def run_with_deadline():
            async with asyncio.timeout(None) as deadline:
                deadlines.append(deadline)
                return await self.governed.invoke(TOOL, {}, "active")

        active = asyncio.create_task(run_with_deadline())
        self.tasks.append(active)
        await asyncio.wait_for(self.gateway.started.wait(), 2)
        deadlines[0].reschedule(asyncio.get_running_loop().time())
        with self.assertRaises(TimeoutError):
            await asyncio.wait_for(active, 2)
        self.assertFalse(self.gateway.finished.is_set())
        await self.assert_fenced("after_timeout")
        self.assertTrue(self.governed.poisoned)
        self.assertEqual(len(self.gateway.calls), 1)
        self.assertEqual(self.events, [])

    async def test_cancel_queued_waiter_does_not_poison_active_or_later_call(self):
        active = await self.start_active()
        queued = self.invoke("queued")
        await asyncio.sleep(0)
        queued.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await queued
        self.assertFalse(self.governed.poisoned)
        self.assertEqual([call[0] for call in self.gateway.calls], ["active"])
        self.gateway.release.set()
        self.assertEqual((await asyncio.wait_for(active, 2))["status"], "succeeded")
        self.assertEqual((await asyncio.wait_for(self.invoke("later"), 2))["status"], "succeeded")
        self.assertFalse(self.governed.poisoned)
        self.assertEqual([call[0] for call in self.gateway.calls], ["active", "later"])
        self.assertEqual([event["tool_call_id"] for event in self.events], ["active", "later"])

    async def test_normal_receipt_serializes_and_allows_next_request(self):
        active = await self.start_active()
        queued = self.invoke("queued")
        await asyncio.sleep(0)
        self.assertEqual(len(self.gateway.calls), 1)
        self.gateway.release.set()
        values = await asyncio.wait_for(asyncio.gather(active, queued), 2)
        self.assertEqual([value["status"] for value in values], ["succeeded", "succeeded"])
        self.assertFalse(self.governed.poisoned)
        self.assertEqual([call[0] for call in self.gateway.calls], ["active", "queued"])
        self.assertEqual(self.gateway.read_sizes, [1048577, 1048577])
        self.assertTrue(all(call[2] == 50 for call in self.gateway.calls))

    async def test_cancelled_receipt_is_not_task_cancellation(self):
        self.gateway.block_call = None
        self.gateway.status = "cancelled"
        value = await asyncio.wait_for(self.invoke("cancelled_receipt"), 2)
        self.assertEqual(value["status"], "cancelled")
        self.assertFalse(self.governed.poisoned)
        self.gateway.status = "succeeded"
        self.assertEqual((await asyncio.wait_for(self.invoke("later"), 2))["status"], "succeeded")

    async def test_transport_error_still_fences_without_retry(self):
        self.gateway.failure = OSError("synthetic_transport_error")
        self.gateway.block_call = None
        with self.assertRaisesRegex(OSError, "synthetic_transport_error"):
            await asyncio.wait_for(self.invoke("failed"), 2)
        self.assertTrue(self.governed.poisoned)
        await self.assert_fenced("later")
        self.assertEqual(len(self.gateway.calls), 1)
        self.assertEqual(self.events, [])

    async def test_oversized_capability_response_still_fences_and_closes(self):
        self.gateway.raw = b" " * 1048577
        self.gateway.block_call = None
        with self.assertRaisesRegex(ValueError, "sdk_capability_output_limit"):
            await asyncio.wait_for(self.invoke("oversized"), 2)
        self.assertTrue(self.governed.poisoned)
        await self.assert_fenced("later")
        self.assertEqual(self.gateway.read_sizes, [1048577])
        self.assertEqual(self.gateway.closed, ["oversized"])

    async def test_knowledge_read_keeps_its_existing_response_bound(self):
        self.gateway.block_call = "never"
        self.gateway.raw = b" " * 1048577
        retrieve = knowledge.tools_for(launch())["knowledge_retrieve"][2]
        with self.assertRaisesRegex(ValueError, "sdk_knowledge_output_limit"):
            await asyncio.wait_for(retrieve({"dataset_id": "fixture-dataset", "query": "fixture", "top_k": 1}), 2)
        self.assertEqual(self.gateway.read_sizes, [1048577])
        self.assertEqual(self.gateway.calls, [(None, "https://gateway.invalid/internal/v1/sdk-tools/knowledge/retrieve", 35)])
        self.assertEqual(self.gateway.closed, [None])


if __name__ == "__main__":
    unittest.main()
