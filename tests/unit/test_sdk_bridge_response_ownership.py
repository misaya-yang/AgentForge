"""Actual urllib processing with in-memory responses; no socket or SDK calls."""
import asyncio
import importlib.util
import io
import json
import os
import sys
import unittest
from contextlib import ExitStack
from email.message import Message
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import BaseHandler, build_opener
from urllib.response import addinfourl

SOURCE = Path(__file__).resolve().parents[2] / "apps/sdk-agent-worker/sdk_agent_worker"
PACKAGE = "_sdk_bridge_response_ownership_subject"
spec = importlib.util.spec_from_file_location(
    PACKAGE, SOURCE / "__init__.py", submodule_search_locations=[str(SOURCE)]
)
package = importlib.util.module_from_spec(spec)
sys.modules[PACKAGE] = package
spec.loader.exec_module(package)
knowledge = __import__(PACKAGE + ".knowledge", fromlist=["knowledge"])
capabilities = __import__(PACKAGE + ".capabilities", fromlist=["capabilities"])
RUN_ID = "00000000-0000-4000-8000-000000000001"


def launch():
    return SimpleNamespace(
        read_bridge={"grant": "b" * 64, "dataset_ids": ["fixture-dataset"]},
        capability_bridge={"grant": "a" * 64, "run_id": RUN_ID, "tools": [{
            "name": "platform_capability_0", "tool": "fixture.write", "namespace": "fixture",
        }]},
    )


class Body(io.BytesIO):
    def __init__(self, value):
        super().__init__(value)
        self.closes = 0
        self.read_sizes = []

    def read(self, size=-1):
        self.read_sizes.append(size)
        return super().read(size)

    def close(self):
        self.closes += 1
        super().close()


class MemoryHTTP(BaseHandler):
    handler_order = 100

    def __init__(self, status=200, raw=b'{"authorized":true}', location=None):
        self.status, self.raw, self.location = status, raw, location
        self.calls, self.bodies, self.responses = [], [], []
        self.unrelated = Body(b"unrelated")

    def https_open(self, request):
        self.calls.append(request)
        headers = Message()
        if self.location is not None:
            headers["Location"] = self.location
        body = Body(self.raw)
        response = addinfourl(body, headers, request.full_url, self.status)
        response.msg = "synthetic status"
        self.bodies.append(body)
        self.responses.append(response)
        return response

    http_open = https_open

    def opener(self, *handlers):
        return build_opener(*handlers, self)


class BridgeResponseOwnershipTests(unittest.TestCase):
    def patches(self, gateway):
        stack = ExitStack()
        stack.enter_context(patch.dict(os.environ, {
            "SDK_AGENT_GATEWAY_URL": "https://gateway.invalid",
            "HTTPS_PROXY": "https://ambient-proxy.invalid",
        }))
        for module in (knowledge, capabilities):
            stack.enter_context(patch.object(module, "build_opener", gateway.opener))
        return stack

    def request(self, bridge):
        if bridge == "knowledge":
            return knowledge._request(launch(), "admit")
        return capabilities._request(launch(), "admit", {})

    def assert_owned_closed(self, gateway):
        self.assertEqual(len(gateway.calls), 1)
        self.assertEqual(gateway.bodies[0].closes, 1)
        self.assertTrue(gateway.bodies[0].closed)
        self.assertEqual(gateway.unrelated.closes, 0)
        self.assertFalse(gateway.unrelated.closed)
        self.assertEqual(gateway.calls[0].host, "gateway.invalid")

    def test_http_errors_close_before_caller_retains_exception(self):
        retained = []
        for bridge in ("knowledge", "capability"):
            for status in (400, 401, 403, 409, 429, 500, 503):
                with self.subTest(bridge=bridge, status=status):
                    gateway = MemoryHTTP(status, b"secret-shaped-error-body")
                    with self.patches(gateway):
                        try:
                            self.request(bridge)
                        except HTTPError as error:
                            retained.append(error)
                            self.assertEqual(error.code, status)
                            self.assertEqual(error.reason, "synthetic status")
                            self.assertNotIn("secret-shaped", str(error))
                        else:
                            self.fail("HTTP failure was accepted")
                    self.assert_owned_closed(gateway)
                    self.assertEqual(gateway.bodies[0].read_sizes, [])
        self.assertEqual(len(retained), 14)

    def test_redirects_close_without_read_or_second_dispatch(self):
        for bridge in ("knowledge", "capability"):
            for status in (301, 302, 303, 307, 308):
                for location in ("https://elsewhere.invalid/secret-shaped-target", "file:///private", None):
                    with self.subTest(bridge=bridge, status=status, location=location):
                        gateway = MemoryHTTP(status, b"secret-shaped-redirect-body", location)
                        with self.patches(gateway):
                            if location is not None and location.startswith("https:"):
                                with self.assertRaisesRegex(PermissionError, "^sdk_gateway_redirect_forbidden$"):
                                    self.request(bridge)
                            else:
                                # urllib rejects absent/unsupported Location before
                                # redirect_request; preserve that HTTP status path.
                                with self.assertRaises(HTTPError) as raised:
                                    self.request(bridge)
                                self.assertEqual(raised.exception.code, status)
                        self.assert_owned_closed(gateway)
                        self.assertEqual(gateway.bodies[0].read_sizes, [])

    def test_malformed_location_closes_before_redirect_callback(self):
        for bridge in ("knowledge", "capability"):
            for status in (301, 302, 303, 307, 308):
                with self.subTest(bridge=bridge, status=status):
                    gateway = MemoryHTTP(status, b"unread-body", "https://[invalid")
                    with self.patches(gateway), self.assertRaises(ValueError):
                        self.request(bridge)
                    self.assert_owned_closed(gateway)
                    self.assertEqual(gateway.bodies[0].read_sizes, [])

    def test_success_keeps_existing_bounded_read_and_timeout(self):
        for bridge, timeout in (("knowledge", 35), ("capability", 50)):
            with self.subTest(bridge=bridge):
                gateway = MemoryHTTP()
                with self.patches(gateway):
                    self.assertEqual(self.request(bridge), {"authorized": True})
                self.assert_owned_closed(gateway)
                self.assertEqual(gateway.bodies[0].read_sizes, [1048577])
                self.assertEqual(gateway.calls[0].timeout, timeout)

    def test_malformed_and_oversized_success_bodies_close(self):
        for bridge in ("knowledge", "capability"):
            for raw in (b"invalid-json", b" " * 1048577):
                with self.subTest(bridge=bridge, size=len(raw)):
                    gateway = MemoryHTTP(raw=raw)
                    with self.patches(gateway), self.assertRaises(ValueError):
                        self.request(bridge)
                    self.assert_owned_closed(gateway)
                    self.assertEqual(gateway.bodies[0].read_sizes, [1048577])

    def test_authority_rejection_keeps_original_status(self):
        async def scenario():
            for status in (401, 403, 503):
                gateway = MemoryHTTP(status)
                with self.patches(gateway):
                    with self.assertRaises(HTTPError) as raised:
                        await knowledge.ensure_access(launch())
                    self.assertEqual(raised.exception.code, status)
                self.assert_owned_closed(gateway)
        asyncio.run(scenario())

    def test_uncertain_execution_closes_and_fences_later_dispatch(self):
        async def scenario():
            for status, location in ((503, None), (307, "https://elsewhere.invalid")):
                gateway = MemoryHTTP(status, location=location)
                events = []

                async def emit(value, events=events):
                    events.append(value)

                governed = capabilities.GovernedTools(launch(), emit)
                with self.patches(gateway):
                    with self.assertRaises((HTTPError, PermissionError)):
                        await governed.invoke("platform_capability_0", {}, "first")
                    self.assertTrue(governed.poisoned)
                    with self.assertRaisesRegex(RuntimeError, "sdk_capability_reconciliation_required"):
                        await governed.invoke("platform_capability_0", {}, "later")
                self.assert_owned_closed(gateway)
                self.assertEqual(events, [])
                self.assertEqual(json.loads(gateway.calls[0].data)["tool_call_id"], "first")
        asyncio.run(scenario())


if __name__ == "__main__":
    unittest.main()
