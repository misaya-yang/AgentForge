"""Offline client contract tests using httpx's in-memory transport only."""
from __future__ import annotations

import json
import os
import unittest
from pathlib import Path
from unittest.mock import AsyncMock, patch

import httpx

from ai_assistant import AssistantClient, AssistantError, AuthError, UnknownOutcomeError
from ai_assistant.runs import run_view

RUN_ID = "00000000-0000-4000-8000-000000000001"
FIXTURE = Path(__file__).resolve().parents[2] / "fixtures" / "run_recovery_v1.json"
ORIGIN = "https://gateway.example"


class RunProjectionTests(unittest.TestCase):
    def test_shared_native_python_run_recovery_fixture(self):
        for case in json.loads(FIXTURE.read_text())["cases"]:
            with self.subTest(case=case["name"]):
                if case["expected"] is None:
                    with self.assertRaises(AssistantError):
                        run_view(RUN_ID, case["response"])
                else:
                    self.assertEqual(run_view(RUN_ID, case["response"]), case["expected"])

    def test_nested_usage_is_bounded_and_nonfinite_cost_is_absent(self):
        details = {"cached_tokens": 2}
        for _ in range(100):
            details = {"input_tokens_details": details}
        for cost in (float("nan"), float("inf"), -1, True, 10**400):
            result = run_view(RUN_ID, {"run": {
                "run_id": RUN_ID, "engine": "openai_agents", "status": "completed",
                "usage": {"input_tokens_details": details, "sdk_usage": {
                    "provider_cost_usd": cost, "state": "unavailable",
                }},
            }})["run"]["usage"]
            self.assertEqual(result["input_tokens_details"], {})
            self.assertNotIn("provider_cost_usd", result["sdk_usage"])
            self.assertNotIn("input_tokens", result)


class RunRecoveryTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        # Construct only in-memory clients; ambient proxy configuration is irrelevant.
        environment = patch.dict(os.environ, {key: "" for key in (
            "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy",
        )})
        environment.start()
        self.addCleanup(environment.stop)
        self.client = AssistantClient(
            "synthetic-owner-key", base_url=ORIGIN, tenant_id="tenant-one", user_id="owner-one",
            max_retries=2,
        )
        await self.client._transport._client.aclose()
        self.requests = []
        self.responses = []

        def handle(request):
            self.requests.append(request)
            if not self.responses:
                raise AssertionError("Unexpected HTTP attempt")
            response = self.responses.pop(0)
            if isinstance(response, Exception):
                raise response
            return response

        self.client._transport._client = httpx.AsyncClient(
            base_url=ORIGIN, transport=httpx.MockTransport(handle), follow_redirects=True,
        )
        self.sleep = patch("ai_assistant.transport.http.asyncio.sleep", new_callable=AsyncMock)
        self.sleep.start()

    async def asyncTearDown(self):
        self.sleep.stop()
        await self.client.close()

    @staticmethod
    def response(status="running", usage=None):
        run = {"run_id": RUN_ID, "engine": "openai_agents", "status": status}
        if usage is not None:
            run["usage"] = usage
        return httpx.Response(200, json={"run": run})

    async def test_refresh_is_get_only_and_late_usage_is_not_zero(self):
        self.responses = [self.response("completed"), self.response("completed", {
            "output_tokens": 2, "sdk_usage": {"state": "unavailable", "cost_state": "reported",
                                              "provider_cost_usd": 0.25},
        })]
        first = await self.client.runs.get(RUN_ID)
        self.assertNotIn("usage", first["run"])
        second = await self.client.runs.get(RUN_ID)
        self.assertNotIn("input_tokens", second["run"]["usage"])
        self.assertEqual(second["run"]["usage"]["output_tokens"], 2)
        for request in self.requests:
            self.assertEqual(request.method, "GET")
            self.assertEqual(request.url.path, f"/api/v1/assistant/runs/{RUN_ID}")
            self.assertEqual(request.headers["X-API-Key"], "synthetic-owner-key")
            self.assertEqual(request.headers["X-Tenant-Id"], "tenant-one")
            self.assertEqual(request.headers["X-User-Id"], "owner-one")
            self.assertEqual(request.content, b"")

    async def test_invalid_run_id_never_sends(self):
        for run_id in ("", "../other", RUN_ID.upper().replace("0001", "000A"),
                       RUN_ID + "?owner=other", "worker-session", None):
            with self.subTest(run_id=run_id), self.assertRaises(ValueError):
                await self.client.runs.get(run_id)
        self.assertEqual(self.requests, [])

    async def test_revoked_or_other_owner_read_never_returns_cached_status(self):
        self.responses = [self.response(), httpx.Response(403, json={"detail": "denied"}),
                          httpx.Response(404, json={"detail": "not found"})]
        await self.client.runs.get(RUN_ID)
        with self.assertRaises(AuthError):
            await self.client.runs.get(RUN_ID)
        with self.assertRaises(AssistantError) as error:
            await self.client.runs.get(RUN_ID)
        self.assertEqual(error.exception.status_code, 404)
        self.assertEqual([r.method for r in self.requests], ["GET", "GET", "GET"])

    async def test_cancel_acknowledgement_preserves_true_and_false(self):
        for cancelled in (True, False):
            self.responses = [httpx.Response(200, json={
                "task_id": RUN_ID, "session_id": "session-one", "cancelled": cancelled,
                "message": "Cancellation requested",
            })]
            result = await self.client.chat.cancel(RUN_ID, reason="client_interrupt")
            self.assertEqual(result, {"task_id": RUN_ID, "session_id": "session-one",
                                      "cancelled": cancelled, "message": "Cancellation requested"})
            request = self.requests[-1]
            self.assertEqual(request.method, "POST")
            self.assertEqual(request.url.path, f"/api/v1/assistant/tasks/{RUN_ID}/cancel")
            self.assertEqual(json.loads(request.content), {"reason": "client_interrupt"})
        self.assertEqual(len(self.requests), 2)

    async def test_uncertain_cancel_is_one_post_then_explicit_get(self):
        failures = [httpx.Response(503, json={"detail": "unavailable"}),
                    httpx.Response(408), httpx.ReadTimeout("connection lost"),
                    httpx.RemoteProtocolError("connection lost"),
                    httpx.Response(200, content=b"not-json"),
                    httpx.Response(200, json={"task_id": "wrong", "cancelled": True}),
                    httpx.Response(200, json={"task_id": RUN_ID, "cancelled": "true"})]
        for failure in failures:
            with self.subTest(failure=type(failure).__name__):
                self.requests.clear()
                self.responses = [failure, self.response("running")]
                with self.assertRaises(UnknownOutcomeError):
                    await self.client.chat.cancel(RUN_ID)
                result = await self.client.runs.get(RUN_ID)
                self.assertEqual(result["run"]["status"], "running")
                self.assertEqual([r.method for r in self.requests], ["POST", "GET"])

    async def test_rejected_cancel_preserves_auth_error_without_retry(self):
        self.responses = [httpx.Response(403, json={"detail": "denied"})]
        with self.assertRaises(AuthError):
            await self.client.chat.cancel(RUN_ID)
        self.assertEqual(len(self.requests), 1)

    async def test_invalid_cancel_id_never_sends(self):
        for task in ("", "..", "../other", RUN_ID + "?run=other", "%2e%2e", "a/b", "a\\b"):
            with self.assertRaises(ValueError):
                await self.client.chat.cancel(task)
        self.assertEqual(self.requests, [])
