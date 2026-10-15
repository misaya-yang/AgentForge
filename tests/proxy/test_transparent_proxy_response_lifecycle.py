"""Offline production-method regressions; no sockets, services or credentials."""
from __future__ import annotations

import asyncio
import unittest
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import httpx

from src.proxy import transparent_proxy as production
from src.proxy.config_loader import ProxyServiceConfig

module = production


class Body(httpx.AsyncByteStream):
    def __init__(self, failure=None, close_failure=None):
        self.failure = failure
        self.close_failure = close_failure
        self.close_calls = 0

    async def __aiter__(self):
        yield b'data: {"hello": "world"}\n\n'
        if self.failure:
            raise self.failure

    async def aclose(self):
        self.close_calls += 1
        if self.close_failure:
            raise self.close_failure


class ResponseLifecycleTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.config = ProxyServiceConfig(service_id="lifecycle", service_name="lifecycle", upstream_url="https://upstream.invalid")
        self.proxy = module.TransparentProxy(SimpleNamespace(database=None, redis=None))
        self.proxy.get_service_availability = AsyncMock(return_value={"available_upstreams": [self.config.upstream_url]})
        self.lease = SimpleNamespace(headers={}, bind_owner=Mock(), release=AsyncMock())
        self.proxy._acquire_capacity_lease = AsyncMock(return_value=self.lease)
        self.proxy._record_non_stream_usage = AsyncMock()
        self.client = None

    async def asyncTearDown(self):
        if self.client:
            await self.client.aclose()

    async def forward(self, body, *, status=200, content_type="application/json"):
        async def handle(request):
            return httpx.Response(status, headers={"content-type": content_type}, stream=body)
        self.client = httpx.AsyncClient(transport=httpx.MockTransport(handle))
        self.proxy._get_client = AsyncMock(return_value=self.client)
        request = module.ProxyRequest(service_name="lifecycle", path="/runs/stream", method="POST", body=b"{}", stream=True, preloaded_config=self.config)
        return await self.proxy.proxy(request)

    def assert_released(self, body):
        self.assertEqual(body.close_calls, 1, "accepted upstream body must be closed")
        semaphore = self.proxy._get_service_semaphore(self.config, "stream")
        self.assertEqual(semaphore._value, self.proxy._resolve_concurrency_limit(self.config, "stream"))
        self.assertFalse(self.proxy._lb_connections[self.config.service_id])
        self.assertGreaterEqual(self.lease.release.await_count, 1)

    async def test_non_sse_read_timeout_closes_response(self):
        body = Body(httpx.ReadTimeout("fixture timeout"))
        response = await self.forward(body)
        self.assertEqual(response.status_code, 504)
        self.assert_released(body)

    async def test_error_read_timeout_closes_response(self):
        body = Body(httpx.ReadTimeout("fixture timeout"))
        response = await self.forward(body, status=503)
        self.assertEqual(response.status_code, 504)
        self.assert_released(body)

    async def test_redirect_read_error_closes_response(self):
        body = Body(httpx.RemoteProtocolError("fixture truncated body"))
        response = await self.forward(body, status=302)
        self.assertEqual(response.status_code, 502)
        self.assert_released(body)

    async def test_content_decode_error_closes_response(self):
        body = Body(httpx.DecodingError("fixture malformed encoding"))
        response = await self.forward(body)
        self.assertEqual(response.status_code, 502)
        self.assert_released(body)

    async def test_non_sse_body_cancellation_closes_response(self):
        body = Body(asyncio.CancelledError())
        with self.assertRaises(asyncio.CancelledError):
            await self.forward(body)
        self.assert_released(body)

    async def test_task_cancel_during_non_sse_body_read(self):
        entered = asyncio.Event()
        class BlockedBody(Body):
            async def __aiter__(self):
                entered.set()
                await asyncio.Event().wait()
                yield b"unreachable"
        body = BlockedBody()
        task = asyncio.create_task(self.forward(body))
        await asyncio.wait_for(entered.wait(), timeout=1)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assert_released(body)

    async def test_task_cancel_during_sse_finalization(self):
        entered = asyncio.Event()
        async def finalize():
            entered.set()
            await asyncio.Event().wait()
        body = Body()
        processor = SimpleNamespace(process_chunk=AsyncMock(side_effect=lambda chunk: chunk), finalize=finalize)
        self.proxy.billing_interceptor = SimpleNamespace(create_stream_processor=Mock(return_value=processor))
        response = await self.forward(body, content_type="text/event-stream")
        await anext(response.stream)
        task = asyncio.create_task(response.stream.aclose())
        await asyncio.wait_for(entered.wait(), timeout=1)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assert_released(body)

    async def test_success_non_sse_closes_once(self):
        body = Body()
        response = await self.forward(body)
        self.assertEqual(response.status_code, 200)
        self.assert_released(body)

    async def test_sse_handoff_stays_open_until_consumed(self):
        body = Body()
        response = await self.forward(body, content_type="text/event-stream")
        self.assertEqual(body.close_calls, 0)
        chunks = [chunk async for chunk in response.stream]
        self.assertEqual(chunks, [b'data: {"hello": "world"}\n\n'])
        self.assert_released(body)

    async def test_started_sse_close_releases_response(self):
        body = Body()
        response = await self.forward(body, content_type="text/event-stream")
        await anext(response.stream)
        await response.stream.aclose()
        self.assert_released(body)

    async def test_sse_finalize_cancellation_still_closes_and_releases(self):
        body = Body()
        processor = SimpleNamespace(process_chunk=AsyncMock(side_effect=lambda chunk: chunk), finalize=AsyncMock(side_effect=asyncio.CancelledError()))
        self.proxy.billing_interceptor = SimpleNamespace(create_stream_processor=Mock(return_value=processor))
        response = await self.forward(body, content_type="text/event-stream")
        await anext(response.stream)
        with self.assertRaises(asyncio.CancelledError):
            await response.stream.aclose()
        self.assert_released(body)

    async def test_sse_close_error_still_releases_slot(self):
        body = Body(close_failure=RuntimeError("fixture close error"))
        response = await self.forward(body, content_type="text/event-stream")
        await anext(response.stream)
        with self.assertRaisesRegex(RuntimeError, "fixture close error"):
            await response.stream.aclose()
        self.assert_released(body)


if __name__ == "__main__":
    unittest.main(verbosity=2)
