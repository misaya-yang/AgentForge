"""Private launch framing and actual child entrypoint; no process/model/network."""
import importlib.util
import io
import json
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

SOURCE = Path(__file__).resolve().parents[2] / "apps/sdk-agent-worker/sdk_agent_worker"
PACKAGE = "_sdk_launch_frame_test_subject"
spec = importlib.util.spec_from_file_location(PACKAGE, SOURCE / "__init__.py", submodule_search_locations=[str(SOURCE)])
package = importlib.util.module_from_spec(spec)
sys.modules[PACKAGE] = package
spec.loader.exec_module(package)
codec = __import__(PACKAGE + ".launch_frame", fromlist=["launch_frame"])
child = __import__(PACKAGE + ".child", fromlist=["child"])
contract = __import__(PACKAGE + ".contract", fromlist=["contract"])
KEY = "synthetic-provider-key"


def request(text="hello"):
    return {"protocol": contract.PROTOCOL, "framework": "openai_agents", "service_id": "service",
            "tenant_id": "tenant", "user_id": "user", "request_id": "request", "session_id": "session",
            "agent": {"model": "model", "instructions": "Reply briefly.", "max_turns": 2},
            "input": text, "stream": True}


class LaunchFrameTests(unittest.TestCase):
    def test_chinese_and_emoji_preserve_admitted_input(self):
        for text in ["中" * 80000, "😀" * 60000]:
            with self.subTest(character=text[0]):
                body = request(text)
                contract.Launch.parse(body, KEY)
                self.assertLess(len(json.dumps(body, ensure_ascii=False).encode()), 393216)
                self.assertGreater(len(json.dumps({"request": body, "provider_key": KEY}).encode()), 400000)
                wire = codec.encode_launch_frame(body, KEY)
                self.assertEqual(codec.read_launch_frame(io.BytesIO(wire)), {"request": body, "provider_key": KEY})
                self.assertLess(len(wire), codec.MAX_LAUNCH_FRAME)

    def test_exact_whole_envelope_boundary_and_max_plus_one(self):
        overhead = len(codec.encode_launch_frame({"padding": ""}, KEY))
        body = {"padding": "x" * (codec.MAX_LAUNCH_FRAME - overhead)}
        wire = codec.encode_launch_frame(body, KEY)
        self.assertEqual(len(wire), codec.MAX_LAUNCH_FRAME)
        self.assertEqual(codec.read_launch_frame(io.BytesIO(wire))["request"], body)
        with self.assertRaises(codec.LaunchFrameTooLarge):
            codec.encode_launch_frame({"padding": body["padding"] + "x"}, KEY)
        with self.assertRaises(codec.LaunchFrameTooLarge):
            codec.read_launch_frame(io.BytesIO(wire + b"x"))

    def test_key_and_metadata_count_toward_budget(self):
        body = request("中文😀")
        body["read_bridge"] = {"grant": "a" * 64, "dataset_ids": ["00000000-0000-4000-8000-000000000001"]}
        key = '\\"' * 2048
        contract.Launch.parse(body, key)
        wire = codec.encode_launch_frame(body, key)
        value = codec.read_launch_frame(io.BytesIO(wire))
        self.assertEqual(value, {"request": body, "provider_key": key})
        self.assertNotIn(key, str(codec.LaunchFrameTooLarge()))

    def test_missing_delimiter_truncation_extra_frames_and_invalid_utf8(self):
        wire = codec.encode_launch_frame(request(), KEY)
        for raw in [b"", wire[:-1], wire[:25], wire[:-3] + b"\n", wire + wire, b"\xff\n", b"[]\n", b'{"request":{},"provider_key":"x","extra":1}\n']:
            with self.subTest(raw_length=len(raw)):
                with self.assertRaises(ValueError) as error:
                    codec.read_launch_frame(io.BytesIO(raw))
                self.assertNotIn(KEY, str(error.exception))

    def test_nonfinite_and_malformed_unicode_fail_without_contents(self):
        for value in [float("nan"), float("inf"), "\ud800"]:
            with self.assertRaisesRegex(ValueError, "^sdk_launch_frame_invalid$"):
                codec.encode_launch_frame({"value": value}, KEY)
        for numeric in [b"NaN", b"1e999"]:
            with self.assertRaisesRegex(ValueError, "^sdk_launch_frame_invalid$"):
                codec.read_launch_frame(io.BytesIO(b'{"request":{"value":' + numeric + b'},"provider_key":"' + KEY.encode() + b'"}\n'))

    def test_reader_uses_bounded_eof_read(self):
        calls = []
        class Stream(io.BytesIO):
            def read(self, count=-1):
                calls.append(count)
                return super().read(count)
        codec.read_launch_frame(Stream(codec.encode_launch_frame(request(), KEY)))
        self.assertEqual(calls, [codec.MAX_LAUNCH_FRAME + 1])


class ChildEntrypointTests(unittest.IsolatedAsyncioTestCase):
    async def test_actual_entrypoint_receives_unicode_launch(self):
        body = request("中" * 80000)
        wire = codec.encode_launch_frame(body, KEY)
        engine, access, profile = AsyncMock(), AsyncMock(), Mock()
        with patch.object(child.sys, "stdin", SimpleNamespace(buffer=io.BytesIO(wire))), patch.object(child.sys, "stdout", io.StringIO()), patch.object(child, "ensure_profile", profile), patch.object(child, "ensure_access", access), patch.object(child, "openai_run", engine):
            result = await child.main()
        self.assertEqual(result, 0)
        self.assertEqual(engine.await_args.args[0].input, body["input"])
        self.assertEqual(engine.await_args.args[0].api_key, KEY)
        profile.assert_called_once()
        access.assert_awaited_once()

    async def test_invalid_frame_fails_before_state_access_or_engine_and_redacts(self):
        wire = codec.encode_launch_frame(request(), KEY)[:-10]
        engine, access, profile, output = AsyncMock(), AsyncMock(), Mock(), io.StringIO()
        with patch.object(child.sys, "stdin", SimpleNamespace(buffer=io.BytesIO(wire))), patch.object(child.sys, "stdout", output), patch.object(child, "ensure_profile", profile), patch.object(child, "ensure_access", access), patch.object(child, "openai_run", engine):
            result = await child.main()
        self.assertEqual(result, 1)
        self.assertEqual(json.loads(output.getvalue()), {"event": "error", "code": "sdk_execution_failed"})
        self.assertNotIn(KEY, output.getvalue())
        engine.assert_not_called()
        access.assert_not_called()
        profile.assert_not_called()


if __name__ == "__main__":
    unittest.main()
