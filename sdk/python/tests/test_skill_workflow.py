"""Offline public client → transport → skill wire workflow contracts."""
from __future__ import annotations

import json
import unittest

import httpx
from ai_assistant import AssistantClient, AssistantError


class SkillWorkflowTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.client = AssistantClient("synthetic-key", base_url="https://gateway.example",
                                      max_retries=0)
        await self.client._transport._client.aclose()
        self.requests = []
        self.responses = []

        def handle(request):
            self.requests.append(request)
            if not self.responses:
                raise AssertionError("Unexpected additional request")
            response = self.responses.pop(0)
            if isinstance(response, BaseException):
                raise response
            return response

        self.client._transport._client = httpx.AsyncClient(
            base_url="https://gateway.example", transport=httpx.MockTransport(handle),
        )

    async def asyncTearDown(self):
        await self.client.close()

    def respond(self, value, status=200):
        self.responses.append(httpx.Response(status, json=value))

    async def test_explicit_skill_surface(self):
        self.assertTrue(hasattr(self.client, "skills"), "explicit skill surface missing")

    async def test_legacy_dictionary_becomes_text(self):
        self.respond({"success": True, "type": "skill_instructions", "result": "instructions"})
        await self.client.tools.test("skill-create", input_data={"purpose": "例"})
        body = json.loads(self.requests[0].content)
        self.assertIsInstance(body["input"], str, "skill input must be string")
        self.assertEqual(json.loads(body["input"]), {"purpose": "例"})

    async def test_legacy_none_becomes_empty_text(self):
        self.respond({"success": True})
        await self.client.tools.test("skill-create")
        self.assertEqual(json.loads(self.requests[0].content), {"input": ""})

    async def test_runtime_catalog_unchanged(self):
        self.respond({"tools": [{"name": "browser", "description": "runtime", "category": "web"}]})
        result = await self.client.tools.list_tools()
        self.assertEqual(result[0].name, "browser")
        self.assertEqual(self.requests[0].url.path, "/api/v1/assistant/tools")
        self.assertEqual(self.requests[0].method, "GET")

    async def test_discover_read_test_journey(self):
        row = {"name": "refund-guide", "enabled": False, "status": "proposed", "revision": 7}
        detail = {**row, "instructions": "Only return instructions", "version_id": "version-one"}
        result = {"success": True, "type": "skill_instructions", "result": detail["instructions"]}
        catalog = {"skills": [row], "total": 1, "future_metadata": "preserved"}
        for value in (catalog, detail, result):
            self.respond(value)
        self.assertEqual(await self.client.skills.list(enabled_only=False), catalog)
        self.assertEqual(await self.client.skills.get(row["name"]), detail)
        self.assertEqual(await self.client.skills.test(row["name"], input="A question"), result)
        self.assertEqual([(r.method, r.url.path) for r in self.requests], [
            ("GET", "/api/v1/skills"), ("GET", "/api/v1/skills/refund-guide"),
            ("POST", "/api/v1/skills/refund-guide/test"),
        ])
        self.assertEqual(dict(self.requests[0].url.params), {"enabled_only": "false"})
        self.assertEqual(json.loads(self.requests[2].content), {"input": "A question"})
        self.assertTrue(all(r.headers["X-API-Key"] == "synthetic-key" for r in self.requests))

    async def test_default_filter_and_platform_row_preserved(self):
        value = {"skills": [{"name": "skill-create", "enabled": False}], "total": 1}
        self.respond(value)
        self.assertEqual(await self.client.skills.list(), value)
        self.assertEqual(self.requests[0].url.params["enabled_only"], "true")

    async def test_false_result_is_not_success(self):
        value = {"success": False, "error": "instruction unavailable"}
        self.respond(value)
        self.assertEqual(await self.client.skills.test("refund-guide", input=""), value)
        self.assertEqual(len(self.requests), 1)

    async def test_legacy_string_passthrough(self):
        self.respond({"success": True})
        await self.client.tools.test("refund-guide", input_data="plain text")
        self.assertEqual(json.loads(self.requests[0].content), {"input": "plain text"})

    async def test_legacy_get_enable_disable_unchanged(self):
        self.respond({"name": "refund-guide", "enabled": False})
        self.respond({"enabled": True})
        self.respond({"enabled": False})
        self.assertEqual((await self.client.tools.get("refund-guide")).name, "refund-guide")
        self.assertEqual(await self.client.tools.enable("refund-guide"), {"enabled": True})
        self.assertEqual(await self.client.tools.disable("refund-guide"), {"enabled": False})
        self.assertEqual([(r.method, r.url.path) for r in self.requests], [
            ("GET", "/api/v1/skills/refund-guide"),
            ("POST", "/api/v1/skills/refund-guide/enable"),
            ("POST", "/api/v1/skills/refund-guide/disable"),
        ])

    async def test_catalog_failure_then_explicit_read(self):
        self.respond({"detail": "denied"}, 403)
        with self.assertRaises(AssistantError) as caught:
            await self.client.skills.list()
        self.assertEqual(caught.exception.status_code, 403)
        value = {"skills": [], "total": 0}
        self.respond(value)
        self.assertEqual(await self.client.skills.list(), value)
        self.assertEqual([r.method for r in self.requests], ["GET", "GET"])

    async def test_mismatched_detail_rejected(self):
        self.respond({"name": "another-skill"})
        with self.assertRaisesRegex(AssistantError, "Invalid skill detail"):
            await self.client.skills.get("refund-guide")

    async def test_invalid_filter_local(self):
        with self.assertRaises(ValueError):
            await self.client.skills.list(enabled_only=1)
        self.assertEqual(self.requests, [])


_CASES = {
    "bad_name": ["", "..", "abc/def", "abc%2fdef", "abc?x", "abc#x", "A-b", "ab", "a" * 51],
    "bad_text": [None, {}, 1],
    "bad_legacy": [1, {"n": float("nan")}, {"n": float("inf")}, {"n": object()}],
    "bad_catalog": [[], {}, {"skills": {}, "total": 0}, {"skills": [], "total": True},
                    {"skills": [], "total": -1}, {"skills": [None], "total": 1}],
    "bad_detail": [[], {}, {"name": "../other"}],
    "bad_result": [[], {}, {"success": "true"}],
    "rejected_test": [401, 403, 404, 409, 422, 429, 500, 503],
    "lost_test": [httpx.ReadTimeout("lost"), httpx.ConnectError("lost")],
}


def _case(kind, value):
    async def check(self):
        if kind == "bad_name":
            for operation in (self.client.skills.get,
                              lambda name: self.client.skills.test(name, input="hello"),
                              lambda name: self.client.tools.test(name, input_data="hello")):
                with self.assertRaises(ValueError):
                    await operation(value)
            self.assertEqual(self.requests, [])
        elif kind in {"bad_text", "bad_legacy"}:
            with self.assertRaises((ValueError, TypeError)):
                if kind == "bad_text":
                    await self.client.skills.test("refund-guide", input=value)
                else:
                    await self.client.tools.test("refund-guide", input_data=value)
            self.assertEqual(self.requests, [])
        elif kind in {"bad_catalog", "bad_detail", "bad_result"}:
            self.respond(value)
            with self.assertRaises(AssistantError):
                if kind == "bad_catalog":
                    await self.client.skills.list()
                elif kind == "bad_detail":
                    await self.client.skills.get("refund-guide")
                else:
                    await self.client.skills.test("refund-guide", input="hello")
            self.assertEqual(len(self.requests), 1)
        else:
            self.client._transport._auth.config.max_retries = 3
            if kind == "rejected_test":
                self.respond({"detail": "rejected"}, value)
            else:
                self.responses.append(value)
            with self.assertRaises(AssistantError):
                await self.client.skills.test("refund-guide", input="hello")
            self.assertEqual(len(self.requests), 1)
            self.assertEqual(self.requests[0].method, "POST")
    return check


for _kind, _values in _CASES.items():
    for _index, _value in enumerate(_values):
        setattr(SkillWorkflowTests, f"test_{_kind}_{_index}", _case(_kind, _value))
