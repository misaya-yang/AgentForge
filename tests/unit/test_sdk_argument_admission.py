"""Production governed bridge admission, with only its HTTP I/O edge doubled.

No SDK provider, Runtime, Worker process, database or listener is started. Actual
bridge methods, JSON construction, thread dispatch, approval polling and poison
logic execute. A fake Gateway returns known approval/terminal receipts.
"""
import asyncio
import importlib.util
import json
import os
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[2] / "apps/sdk-agent-worker/sdk_agent_worker"
PACKAGE = "_sdk_argument_admission_test_subject"
spec = importlib.util.spec_from_file_location(PACKAGE, SOURCE / "__init__.py", submodule_search_locations=[str(SOURCE)])
package = importlib.util.module_from_spec(spec)
sys.modules[PACKAGE] = package
spec.loader.exec_module(package)
capabilities = __import__(PACKAGE + ".capabilities", fromlist=["capabilities"])
RUN_ID = "00000000-0000-4000-8000-000000000001"
TOOL = "platform_capability_0"


def compact_bytes(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode()


class ArgumentAdmissionTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.events, self.calls = [], []
        self.terminal = "succeeded"
        self.launch = SimpleNamespace(capability_bridge={"grant": "a" * 64, "run_id": RUN_ID,
            "tools": [{"name": TOOL, "tool": "fixture.write", "namespace": "fixture"}]})

        async def emit(value):
            self.events.append(value)

        self.governed = capabilities.GovernedTools(self.launch, emit)
        owner = self

        class Gateway:
            def open(self, request, *, timeout):
                owner.assertEqual(timeout, 50)
                body = json.loads(request.data)
                owner.calls.append(body)
                # Match the existing Rust object/compact-byte admission boundary.
                if len(compact_bytes(body["arguments"])) > 65536:
                    raise AssertionError("server argument limit exceeded")
                result = {"run_id": RUN_ID, "tool_call_id": "official-call-1", "tool": "fixture.write",
                    "approval_id": "00000000-0000-4000-8000-000000000002"}
                if len(owner.calls) == 1:
                    result.update(status="approval_required", expires_at="2099-01-01T00:00:00Z", preview={"can_approve": True})
                else:
                    result.update(status=owner.terminal, approval_status="consumed" if owner.terminal == "succeeded" else "rejected",
                        execution_id="fixture-execution" if owner.terminal == "succeeded" else None,
                        response={"success": owner.terminal == "succeeded"})

                class Response:
                    def __enter__(self): return self
                    def __exit__(self, *_args): return False
                    def read(self, bound): return json.dumps(result).encode()[:bound]

                return Response()

        for guard in (patch.dict(os.environ, {"SDK_AGENT_GATEWAY_URL": "https://gateway.invalid"}),
                      patch.object(capabilities, "build_opener", return_value=Gateway())):
            guard.start()
            self.addCleanup(guard.stop)

    async def assert_allowed(self, arguments):
        value = await asyncio.wait_for(self.governed.invoke(TOOL, arguments, "official-call-1"), 3)
        self.assertEqual(value["status"], self.terminal)
        self.assertEqual(len(self.calls), 2)
        self.assertTrue(all(call["arguments"] == arguments for call in self.calls))
        self.assertEqual([event["event"] for event in self.events], ["approval_required", "approval_result", "tool_result"])
        self.assertEqual(self.events[1]["approval_status"], "consumed" if self.terminal == "succeeded" else "rejected")
        self.assertFalse(self.governed.poisoned)

    async def test_multilingual_action_reaches_approval_and_completion(self):
        await self.assert_allowed({"text": "文" * 12000})

    async def test_multilingual_action_rejection_preserves_receipt(self):
        self.terminal = "failed"
        await self.assert_allowed({"text": "文" * 12000})

    async def test_compact_array_avoids_optional_whitespace_overcount(self):
        await self.assert_allowed({"values": [0] * 30000})

    async def test_ascii_exact_65536_byte_limit(self):
        arguments = {"text": "a" * 65525}
        self.assertEqual(len(compact_bytes(arguments)), 65536)
        await self.assert_allowed(arguments)

    async def test_utf8_exact_65536_byte_limit(self):
        arguments = {"text": "文" * 21841 + "ab"}
        self.assertEqual(len(compact_bytes(arguments)), 65536)
        await self.assert_allowed(arguments)

    async def test_ascii_oversize_stays_fenced_before_dispatch(self):
        with self.assertRaisesRegex(ValueError, "sdk_capability_arguments_invalid"):
            await self.governed.invoke(TOOL, {"text": "a" * 65526}, "official-call-1")
        self.assertEqual(self.calls, [])
        self.assertTrue(self.governed.poisoned)

    async def test_utf8_oversize_stays_fenced_before_dispatch(self):
        with self.assertRaisesRegex(ValueError, "sdk_capability_arguments_invalid"):
            await self.governed.invoke(TOOL, {"text": "文" * 21842}, "official-call-1")
        self.assertEqual(self.calls, [])
        self.assertTrue(self.governed.poisoned)

    async def test_nonserializable_value_stays_fenced_before_dispatch(self):
        with self.assertRaises(TypeError):
            await self.governed.invoke(TOOL, {"value": object()}, "official-call-1")
        self.assertEqual(self.calls, [])
        self.assertTrue(self.governed.poisoned)

    async def test_lone_surrogate_stays_fenced_before_dispatch(self):
        with self.assertRaises(UnicodeEncodeError):
            await self.governed.invoke(TOOL, {"value": "\ud800"}, "official-call-1")
        self.assertEqual(self.calls, [])
        self.assertTrue(self.governed.poisoned)

    async def test_nonfinite_json_stays_fenced_before_dispatch(self):
        with self.assertRaises(ValueError):
            await self.governed.invoke(TOOL, {"value": float("nan")}, "official-call-1")
        self.assertEqual(self.calls, [])
        self.assertTrue(self.governed.poisoned)


if __name__ == "__main__":
    unittest.main()
