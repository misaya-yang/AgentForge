"""Shared wire cases plus the exact production async method, without service imports/I/O.

The method is loaded from its AST unchanged; only its HTTP client/config/logger are
substituted. This is deliberately not application assembly or provider acceptance.
"""

import ast
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import unittest

ROOT = Path(__file__).resolve().parents[3]
SOURCE = ROOT / "apps/knowledge-service/src/knowledge_service/services/knowledge"
SPEC = importlib.util.spec_from_file_location("qa_provider_stream", SOURCE / "qa_provider_stream.py")
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
QaProviderStream = MODULE.QaProviderStream
CASES = json.loads((ROOT / "tests/fixtures/knowledge/qa-provider-stream.json").read_text())


class ParserTests(unittest.TestCase):
    def test_shared_contract(self):
        for case in CASES:
            with self.subTest(case=case["name"]):
                parser = QaProviderStream()
                text, tokens, error = "", None, None
                try:
                    closed = False
                    for line in case["lines"]:
                        event = parser.line(line)
                        if event:
                            text += event.content or ""
                            if event.tokens is not None:
                                tokens = event.tokens
                            if event.closed:
                                closed = True
                                break
                    if not closed:
                        event = parser.finish()
                        if event:
                            text += event.content or ""
                            if event.tokens is not None:
                                tokens = event.tokens
                except ValueError as exc:
                    error = str(exc)
                self.assertEqual((text, tokens, error), (case["text"], case["tokens"], case["error"]))

    def test_event_budget(self):
        parser = QaProviderStream()
        parser.line("data: " + "x" * (3 * 1024 * 1024))
        with self.assertRaisesRegex(ValueError, "^qa_stream_event_too_large$"):
            parser.line("data: " + "x" * (2 * 1024 * 1024))
        with self.assertRaisesRegex(ValueError, "^qa_stream_event_too_large$"):
            QaProviderStream().line("中" * (2 * 1024 * 1024))


def production_method():
    tree = ast.parse((SOURCE / "qa_service.py").read_text())
    owner = next(node for node in tree.body if isinstance(node, ast.ClassDef) and node.name == "LLMClient")
    method = next(node for node in owner.body if isinstance(node, ast.AsyncFunctionDef) and node.name == "chat_completion_stream")
    namespace = {
        "QaProviderStream": QaProviderStream,
        "httpx": SimpleNamespace(HTTPStatusError=type("HTTPStatusError", (Exception,), {})),
        "logger": SimpleNamespace(error=lambda *_: None),
    }
    exec(compile(ast.Module(body=[method], type_ignores=[]), str(SOURCE / "qa_service.py"), "exec"), namespace)
    return namespace["chat_completion_stream"]


class ProductionMethodTests(unittest.IsolatedAsyncioTestCase):
    async def test_exact_method_wire_cases_no_replay(self):
        method = production_method()
        for case in CASES:
            with self.subTest(case=case["name"]):
                requests = []
                disposed = []

                class Response:
                    def raise_for_status(self):
                        pass

                    async def __aenter__(self):
                        return self

                    async def __aexit__(self, *_):
                        disposed.append(True)

                    async def aiter_lines(self):
                        for line in case["lines"]:
                            yield line

                class Client:
                    def stream(self, *args, **kwargs):
                        requests.append((args, kwargs))
                        return Response()

                async def get_client():
                    return Client()

                owner = SimpleNamespace(_get_client=get_client, config=SimpleNamespace(model="fixture", temperature=0, max_tokens=64))
                text, tokens, error = "", None, None
                try:
                    async for event in method(owner, []):
                        if event["type"] == "delta":
                            text += event["content"]
                        if event["type"] == "usage":
                            tokens = event["tokens"]
                except ValueError as exc:
                    error = str(exc)
                self.assertEqual((text, tokens, error), (case["text"], case["tokens"], case["error"]))
                self.assertEqual(len(requests), 1)
                self.assertEqual(disposed, [True])


if __name__ == "__main__":
    unittest.main()
