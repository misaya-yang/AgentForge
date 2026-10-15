"""Offline retry and authentication-boundary regression checks."""
from __future__ import annotations

import os
import unittest
from unittest.mock import AsyncMock, patch

import httpx

from ai_assistant.auth import AuthConfig, AuthManager
from ai_assistant.exceptions import AssistantError
from ai_assistant.transport.http import HTTPTransport

ORIGIN = "https://gateway.example"
PATH = "/api/v1/assistant/runs/00000000-0000-4000-8000-000000000001"


class TransportSafetyTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        # Construct only in-memory clients; ambient proxy configuration is irrelevant.
        environment = patch.dict(os.environ, {key: "" for key in (
            "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy",
        )})
        environment.start()
        self.addCleanup(environment.stop)
        self.transport = HTTPTransport(AuthManager(AuthConfig(
            api_key="synthetic-key", base_url=ORIGIN, tenant_id="tenant", user_id="owner",
            max_retries=2,
        )))
        await self.transport._client.aclose()
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

        # Even a supplied underlying client with redirects enabled must not follow one.
        self.transport._client = httpx.AsyncClient(
            base_url=ORIGIN, transport=httpx.MockTransport(handle), follow_redirects=True,
            headers={"Authorization": "Bearer synthetic-bearer"},
        )
        self.sleep_patch = patch("ai_assistant.transport.http.asyncio.sleep", new_callable=AsyncMock)
        self.sleep = self.sleep_patch.start()

    async def asyncTearDown(self):
        self.sleep_patch.stop()
        await self.transport.close()

    async def test_get_and_head_keep_bounded_transient_retries(self):
        for method in ("GET", "HEAD"):
            self.requests.clear()
            self.responses = [httpx.Response(503), httpx.ReadTimeout("timeout"),
                              httpx.Response(200, json={})]
            await self.transport.request(method, PATH)
            self.assertEqual([r.method for r in self.requests], [method] * 3)
            self.assertTrue(all(r.headers["X-API-Key"] == "synthetic-key" for r in self.requests))
        self.assertEqual(self.sleep.await_count, 4)

    async def test_read_failure_stops_after_configured_attempts(self):
        self.responses = [httpx.Response(429)] * 3
        with self.assertRaises(AssistantError) as error:
            await self.transport.request("GET", PATH)
        self.assertEqual(error.exception.status_code, 429)
        self.assertEqual(len(self.requests), 3)

    async def test_all_mutations_are_one_attempt_even_with_idempotency_header(self):
        for method in ("POST", "PUT", "PATCH", "DELETE"):
            for status in (429, 502, 503, 504):
                with self.subTest(method=method, status=status):
                    self.requests.clear()
                    self.responses = [httpx.Response(status)]
                    with self.assertRaises(AssistantError):
                        await self.transport.request(method, PATH, json={},
                                                     headers={"Idempotency-Key": "caller-key"})
                    self.assertEqual(len(self.requests), 1)
        self.sleep.assert_not_awaited()

    async def test_mutation_timeout_and_network_failure_do_not_replay(self):
        for failure in (httpx.ReadTimeout("timeout"), httpx.ConnectError("unavailable")):
            self.requests.clear()
            self.responses = [failure]
            with self.assertRaises(AssistantError):
                await self.transport.request("POST", PATH, json={})
            self.assertEqual(len(self.requests), 1)
        self.sleep.assert_not_awaited()

    async def test_redirects_never_forward_tokens_or_rewrite_methods(self):
        for status in (301, 302, 303, 307, 308):
            for location in (ORIGIN + "/other", "https://untrusted.example/collect"):
                for mode in ("get", "post", "stream_get", "stream_post"):
                    with self.subTest(status=status, location=location, mode=mode):
                        self.requests.clear()
                        self.responses = [httpx.Response(status, headers={"Location": location})]
                        with self.assertRaises(AssistantError):
                            if mode == "get":
                                await self.transport.request("GET", PATH)
                            elif mode == "post":
                                await self.transport.request("POST", PATH, json={})
                            elif mode == "stream_get":
                                _ = [event async for event in self.transport.stream_sse_get(PATH)]
                            else:
                                _ = [event async for event in self.transport.stream_sse(PATH, {})]
                        self.assertEqual(len(self.requests), 1)
                        request = self.requests[0]
                        self.assertEqual(request.method, "GET" if mode.endswith("get") else "POST")
                        self.assertEqual(request.url.host, "gateway.example")
                        self.assertEqual(request.headers["Authorization"], "Bearer synthetic-bearer")
                        self.assertEqual(request.headers["X-API-Key"], "synthetic-key")
        self.sleep.assert_not_awaited()

    async def test_off_origin_and_malformed_urls_never_send(self):
        for path in ("https://untrusted.example/collect", "//untrusted.example/collect",
                     "http://gateway.example/collect", "https://gateway.example:444/collect",
                     "https://name:password@gateway.example/collect", "https://gateway.example/x#y",
                     "\\\\untrusted.example/x", "/x\nHost:untrusted.example"):
            for mode in ("json", "stream_get", "stream_post"):
                with self.subTest(path=path, mode=mode), self.assertRaises(ValueError):
                    if mode == "json":
                        await self.transport.request("GET", path)
                    elif mode == "stream_get":
                        _ = [event async for event in self.transport.stream_sse_get(path)]
                    else:
                        _ = [event async for event in self.transport.stream_sse(path, {})]
        self.assertEqual(self.requests, [])

    async def test_same_origin_absolute_cursor_is_get_only(self):
        self.responses = [httpx.Response(200, text='data: {"event_type":"run_finished","data":{}}\n\n')]
        events = [event async for event in self.transport.stream_sse_get(ORIGIN + PATH)]
        self.assertEqual(len(events), 1)
        self.assertEqual(self.requests[0].method, "GET")

    async def test_request_headers_cannot_replace_auth_or_owner(self):
        for key in ("Authorization", "X-API-Key", "x-tenant-id", "X-User-Id", "hOsT", "Cookie",
                    "Proxy-Authorization"):
            with self.assertRaises(ValueError):
                await self.transport.request("GET", PATH, headers={key: "override"})
        self.assertEqual(self.requests, [])

    async def test_retry_after_is_finite_and_bounded(self):
        for raw, expected in (("inf", None), ("nan", None), ("-1", None), ("6000", 30), ("2", 2)):
            response = httpx.Response(503, headers={"Retry-After": raw})
            self.assertEqual(HTTPTransport._parse_retry_after(response), expected)

    async def test_disconnect_does_not_reissue_stream_submission(self):
        self.responses = [httpx.Response(200, text='data: {"event_type":"text_delta","data":{"content":"partial"}}\n\n')]
        events = [event async for event in self.transport.stream_sse(PATH, {})]
        self.assertEqual(len(events), 1)
        self.assertFalse(any(event.is_terminal() for event in events))
        self.assertEqual(len(self.requests), 1)
        self.assertEqual(self.requests[0].method, "POST")
