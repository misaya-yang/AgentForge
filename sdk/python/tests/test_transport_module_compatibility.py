"""Current SDK module calls through the hardened transport, entirely offline."""
from __future__ import annotations

import os
import unittest
from unittest.mock import patch

import httpx

from ai_assistant import AssistantClient, AssistantError

ORIGIN = "https://gateway.example"


class ModuleCompatibilityTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        environment = patch.dict(os.environ, {key: "" for key in (
            "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy",
        )})
        environment.start()
        self.addCleanup(environment.stop)
        self.client = AssistantClient("synthetic-key", base_url=ORIGIN, max_retries=0)
        await self.client._transport._client.aclose()
        self.requests = []
        self.responses = []

        def handle(request):
            self.requests.append(request)
            if not self.responses:
                raise AssertionError("Unexpected network attempt")
            return self.responses.pop(0)

        self.client._transport._client = httpx.AsyncClient(
            base_url=ORIGIN, transport=httpx.MockTransport(handle), follow_redirects=True,
        )

    async def asyncTearDown(self):
        await self.client.close()

    async def test_artifact_download_returns_current_gateway_bytes(self):
        content = b"%PDF-1.7 synthetic artifact\x00\xff"
        self.responses = [httpx.Response(200, content=content)]
        self.assertEqual(await self.client.artifacts.download("artifact-one"), content)
        self.assertEqual(len(self.requests), 1)
        request = self.requests[0]
        self.assertEqual(request.method, "GET")
        self.assertEqual(request.url.path, "/api/v1/assistant/artifacts/artifact-one/download")
        self.assertEqual(request.headers["X-API-Key"], "synthetic-key")
        self.assertEqual(self.client.artifacts.download_url("artifact-one"),
                         ORIGIN + "/api/v1/assistant/artifacts/artifact-one/download")

    async def test_legacy_download_redirect_fails_without_forwarding_credentials(self):
        self.responses = [httpx.Response(307, headers={
            "Location": "https://storage.example/object?signature=synthetic",
        })]
        with self.assertRaises(AssistantError) as error:
            await self.client.artifacts.download("artifact-one")
        self.assertEqual(error.exception.status_code, 307)
        self.assertEqual(len(self.requests), 1)
        self.assertEqual(self.requests[0].url.host, "gateway.example")

    async def test_session_list_and_delete_keep_route_query_and_empty_response(self):
        self.responses = [httpx.Response(200, json={"sessions": [{"session_id": "session-one"}]}),
                          httpx.Response(204)]
        sessions = await self.client.sessions.list(limit=5, offset=10)
        self.assertEqual(sessions[0].session_id, "session-one")
        self.assertEqual(dict(self.requests[0].url.params), {"limit": "5", "offset": "10"})
        self.assertIsNone(await self.client.sessions.delete("session-one"))
        self.assertEqual([request.method for request in self.requests], ["GET", "DELETE"])

    async def test_managed_cursor_recovery_keeps_query_and_never_submits(self):
        body = 'data: {"event_type":"run_finished","data":{"run_id":"turn-one"}}\n\n'
        path = "/api/v2/agent/threads/thread-one/events?after_sequence=6&turn_id=turn-one"
        self.responses = [httpx.Response(200, text=body), httpx.Response(200, text=body)]
        for url in (path, ORIGIN + path):
            events = [event async for event in self.client.threads.events(
                "thread-one", turn={"turn": {"events_url": url}},
            )]
            self.assertTrue(events[0].is_done())
        for request in self.requests:
            self.assertEqual(request.method, "GET")
            self.assertEqual(str(request.url), ORIGIN + path)

    async def test_managed_server_cursor_cannot_change_origin(self):
        with self.assertRaises(ValueError):
            _ = [event async for event in self.client.threads.events(
                "thread-one", turn={"turn": {"events_url": "https://elsewhere.example/events"}},
            )]
        self.assertEqual(self.requests, [])

    async def test_configured_base_path_is_preserved(self):
        self.client._transport._client.base_url = ORIGIN + "/gateway-prefix"
        self.responses = [httpx.Response(200, json={"sessions": []})]
        await self.client.sessions.list()
        self.assertEqual(self.requests[0].url.path, "/gateway-prefix/api/v1/assistant/sessions")
