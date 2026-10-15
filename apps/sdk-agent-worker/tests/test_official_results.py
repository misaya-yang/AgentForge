"""Offline result-boundary tests with the worker's locked official SDK dependencies.

Runner and ClaudeSDKClient are doubles; result objects/parsers and worker conversion
are real. These tests do not establish actual SDK-loop or provider acceptance.
Missing dependencies fail collection. Run explicitly in the optional worker environment.
"""

import asyncio
import importlib.util
import json
import os
import socket
import subprocess
import sys
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

import agents
import claude_agent_sdk as claude
import openai
from claude_agent_sdk._internal.message_parser import parse_message

# Bind to adjacent checkout source, not a potentially stale installed worker wheel.
# A private package alias keeps sibling tests and sys.path untouched.
SOURCE = Path(__file__).resolve().parents[1] / "sdk_agent_worker"
PACKAGE = "_sdk_worker_official_results_test_subject"
spec = importlib.util.spec_from_file_location(
    PACKAGE, SOURCE / "__init__.py", submodule_search_locations=[str(SOURCE)]
)
package = importlib.util.module_from_spec(spec)
sys.modules[PACKAGE] = package
spec.loader.exec_module(package)
engines = __import__(PACKAGE + ".engines", fromlist=["engines"])
Launch = __import__(PACKAGE + ".contract", fromlist=["Launch"]).Launch


class OfficialResultsTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.state = Path(directory.name)
        self.events = []
        self.launch = Launch(
            "openai_agents", "service", "tenant", "user", "request", "session",
            "fixture-model", "fixture", 2, "fixture", True, "nonsecret-fixture-key",
        )
        # Ambient proxy/provider settings must not affect an offline fixture.
        environment = patch.dict(os.environ, {}, clear=True)
        environment.start()
        self.addCleanup(environment.stop)
        for target, attribute in ((socket.socket, "connect"), (subprocess, "Popen")):
            guard = patch.object(target, attribute, side_effect=AssertionError("unexpected I/O"))
            guard.start()
            self.addCleanup(guard.stop)

    async def emit(self, event):
        self.events.append(event)

    def result(self, text):
        return agents.RunResultStreaming(
            input="fixture", new_items=[], raw_responses=[], final_output=text,
            input_guardrail_results=[], output_guardrail_results=[],
            tool_input_guardrail_results=[], tool_output_guardrail_results=[],
            context_wrapper=agents.RunContextWrapper(None), current_agent=agents.Agent(name="fixture"),
            current_turn=1, max_turns=2, _current_agent_output_schema=None, trace=None, is_complete=True,
        )

    async def run_openai(self, result):
        with (
            patch.object(agents.Runner, "run_streamed", return_value=result),
            patch.object(openai._base_client, "get_platform", return_value="Linux"),
        ):
            await engines.openai_run(self.launch, self.state, self.emit)

    async def test_openai_json_text_is_preserved(self):
        text = '{"ok":true,"value":42}'
        await self.run_openai(self.result(text))
        self.assertEqual(self.events[-1]["event"], "completed")
        self.assertEqual(self.events[-1]["text"], text)
        self.assertEqual(self.events[-1]["streamed_text"], text)

    async def test_openai_result_propagates_run_loop_error(self):
        result = self.result(None)

        async def fail():
            raise agents.MaxTurnsExceeded("fixture limit")

        result.run_loop_task = asyncio.create_task(fail())
        await asyncio.sleep(0)
        try:
            with self.assertRaises(agents.MaxTurnsExceeded):
                await self.run_openai(result)
        finally:
            await asyncio.gather(result.run_loop_task, return_exceptions=True)
        self.assertFalse(any(event["event"] == "completed" for event in self.events))

    async def run_claude(self, **fields):
        result = parse_message({
            "type": "result", "subtype": "success", "duration_ms": 1,
            "duration_api_ms": 1, "is_error": False, "num_turns": 1,
            "session_id": "upstream", "terminal_reason": "completed",
        } | fields)
        self.assertIsInstance(result, claude.ResultMessage)

        class Client:
            async def __aenter__(self):
                return self

            async def __aexit__(self, *_args):
                return False

            async def query(self, _value):
                return None

            async def receive_response(self):
                yield result

        with patch.object(claude, "ClaudeSDKClient", return_value=Client()):
            await engines.claude_run(
                replace(self.launch, framework="claude_agent_sdk"), self.state, self.emit,
            )

    async def test_claude_json_text_is_preserved(self):
        text = '{"ok":true,"value":42}'
        await self.run_claude(result=text)
        self.assertEqual(self.events[-1]["event"], "completed")
        self.assertEqual(self.events[-1]["text"], text)
        self.assertEqual(self.events[-1]["streamed_text"], text)
        self.assertEqual(
            json.loads((self.state / "claude-session.json").read_text()),
            {"session_id": "upstream"},
        )

    async def test_claude_empty_success_stays_empty(self):
        await self.run_claude(result=None)
        self.assertEqual(self.events[-1]["event"], "completed")
        self.assertEqual(self.events[-1]["text"], "")
        self.assertEqual(self.events[-1]["streamed_text"], "")

    async def test_claude_error_results_never_commit_or_complete(self):
        for fields in (
            {"is_error": True}, {"subtype": "error_during_execution"},
            {"terminal_reason": "max_turns"},
        ):
            with (
                self.subTest(fields=fields),
                self.assertRaisesRegex(RuntimeError, "sdk_provider_execution_failed"),
            ):
                await self.run_claude(**fields)
            self.assertFalse((self.state / "claude-session.json").exists())
            self.assertFalse(any(event["event"] == "completed" for event in self.events))


if __name__ == "__main__":
    unittest.main()
