"""Offline real proxy, heartbeat, lease/renewal, and ASGI response methods."""
import asyncio
import unittest
from unittest.mock import AsyncMock, patch

from ai_gateway_core.proxy.sse_heartbeat import with_sse_heartbeat
from starlette.requests import ClientDisconnect

from src.api.v1.proxy import _OwnedProxyStreamResponse as OwnedResponse
from src.core.gateway.admission import CapacityAdmissionController, CapacityLease, CapacityRejected
from src.proxy import transparent_proxy as module
from tests.proxy import test_transparent_proxy_response_lifecycle as lifecycle

Body = lifecycle.Body

class OwnershipTests(lifecycle.ResponseLifecycleTests):
    async def asyncSetUp(self):
        await super().asyncSetUp()
        self.release = AsyncMock()
        self.lease = CapacityLease(budget_keys=['test'], queue_wait_ms=0, release=self.release)
        self.proxy._acquire_capacity_lease = AsyncMock(return_value=self.lease)

    def assert_released(self, body):
        self.assertEqual(body.close_calls, 1)
        self.assertEqual(self.proxy._get_service_semaphore(self.config, 'stream')._value, self.proxy._resolve_concurrency_limit(self.config, 'stream'))
        self.assertFalse(self.proxy._lb_connections[self.config.service_id])
        self.assertEqual(self.release.await_count, 1)

    async def lose(self):
        controller = CapacityAdmissionController(lease_ttl_ms=1000)
        with patch('src.core.gateway.admission.eval_script', AsyncMock(return_value=0)):
            await controller._renew_loop(self.lease, [('shared', 'tenant', 'member')])
        await asyncio.sleep(0)

    async def test_never_started_close(self):
        body = Body()
        response = await self.forward(body, content_type='text/event-stream')
        await response.stream.aclose()
        await response.stream.aclose()
        self.assert_released(body)

    async def test_lost_before_bind(self):
        self.lease.lost = True
        body = Body()
        with self.assertRaises(CapacityRejected):
            await self.forward(body, content_type='text/event-stream')
        self.assert_released(body)

    async def test_cancel_start_before_owner_runs(self):
        body = Body()
        original = module._OwnedProxyStream.start
        async def cancel_start(stream, lease):
            asyncio.current_task().cancel()
            await original(stream, lease)
        with patch.object(module._OwnedProxyStream, 'start', cancel_start):
            task = asyncio.create_task(self.forward(body, content_type='text/event-stream'))
            with self.assertRaises(asyncio.CancelledError):
                await task
        self.assert_released(body)

    async def test_heartbeat_lease_loss_second_read(self):
        reading = asyncio.Event()
        class Stall(Body):
            async def __aiter__(self):
                yield b'data: first\n\n'
                reading.set()
                await asyncio.Event().wait()
        body = Stall()
        response = await self.forward(body, content_type='text/event-stream')
        heartbeat = with_sse_heartbeat(response.stream, interval_seconds=60)
        self.assertEqual(await anext(heartbeat), b'data: first\n\n')
        owner = self.lease._owner
        self.assertFalse(owner.done())
        read = asyncio.create_task(anext(heartbeat))
        await asyncio.wait_for(reading.wait(), 1)
        await self.lose()
        with self.assertRaises(asyncio.CancelledError):
            await asyncio.wait_for(read, 1)
        self.assertIs(self.lease._owner, owner)
        self.assert_released(body)

    async def test_backpressure_and_loss_between_reads(self):
        next_read = asyncio.Event()
        class Count(Body):
            async def __aiter__(self):
                yield b'data: first\n\n'
                next_read.set()
                yield b'data: second\n\n'
        body = Count()
        response = await self.forward(body, content_type='text/event-stream')
        await anext(response.stream)
        await asyncio.sleep(0)
        self.assertFalse(next_read.is_set())
        await self.lose()
        with self.assertRaises(asyncio.CancelledError):
            await asyncio.wait_for(response.stream.wait_closed(), 1)
        self.assertFalse(next_read.is_set())
        self.assert_released(body)

    async def run_asgi(self, send, *, body=None):
        body = body or Body()
        response = await self.forward(body, content_type='text/event-stream')
        async def receive():
            await asyncio.Event().wait()
        task = asyncio.create_task(OwnedResponse(response.stream)(
            {'type': 'http', 'asgi': {'version': '3.0', 'spec_version': '2.4'}}, receive, send))
        return body, task

    async def test_asgi_start_failure(self):
        async def send(message):
            raise OSError('start failed')
        body, task = await self.run_asgi(send)
        with self.assertRaises(ClientDisconnect):
            await asyncio.wait_for(task, 1)
        self.assert_released(body)

    async def test_asgi_lease_loss_blocked_send(self):
        sending = asyncio.Event()
        async def send(message):
            if message['type'] == 'http.response.body':
                sending.set()
                await asyncio.Event().wait()
        body, task = await self.run_asgi(send)
        await asyncio.wait_for(sending.wait(), 1)
        await self.lose()
        with self.assertRaises(asyncio.CancelledError):
            await asyncio.wait_for(task, 1)
        self.assert_released(body)

    async def test_asgi_disconnect_before_body(self):
        response = await self.forward(Body(), content_type='text/event-stream')
        async def receive():
            return {'type': 'http.disconnect'}
        async def send(message):
            await asyncio.Event().wait()
        await asyncio.wait_for(OwnedResponse(response.stream)(
            {'type': 'http', 'asgi': {'version': '3.0', 'spec_version': '2.3'}}, receive, send), 1)
        self.assertEqual(self.release.await_count, 1)

    async def test_loss_before_first_read(self):
        body = Body()
        response = await self.forward(body, content_type='text/event-stream')
        await self.lose()
        with self.assertRaises(asyncio.CancelledError):
            await asyncio.wait_for(response.stream.wait_closed(), 1)
        self.assert_released(body)

    async def test_repeated_cancellation_during_transport_close(self):
        closing = asyncio.Event()
        finish = asyncio.Event()
        class SlowClose(Body):
            async def aclose(self):
                self.close_calls += 1
                closing.set()
                await finish.wait()
        body = SlowClose()
        response = await self.forward(body, content_type='text/event-stream')
        close = asyncio.create_task(response.stream.aclose())
        await asyncio.wait_for(closing.wait(), 1)
        close.cancel()
        await asyncio.sleep(0)
        close.cancel()
        await asyncio.sleep(0)
        self.assertFalse(close.done())
        finish.set()
        with self.assertRaises(asyncio.CancelledError):
            await asyncio.wait_for(close, 1)
        self.assert_released(body)

    async def test_asgi_happy_path_bytes(self):
        messages = []
        async def send(message):
            messages.append(message)
        body, task = await self.run_asgi(send)
        await asyncio.wait_for(task, 1)
        self.assertEqual([m.get('body') for m in messages if m['type'] == 'http.response.body'], [b'data: {"hello": "world"}\n\n', b''])
        self.assert_released(body)

    async def test_no_capacity_lease(self):
        self.proxy._acquire_capacity_lease = AsyncMock(return_value=None)
        body = Body()
        response = await self.forward(body, content_type='text/event-stream')
        chunks = [chunk async for chunk in response.stream]
        self.assertEqual(chunks, [b'data: {"hello": "world"}\n\n'])
        self.assertEqual(body.close_calls, 1)
        self.assertFalse(self.proxy._lb_connections[self.config.service_id])

    async def test_lease_loss_during_accounting(self):
        from types import SimpleNamespace
        from unittest.mock import Mock
        entered = asyncio.Event()
        async def finalize():
            entered.set()
            await asyncio.Event().wait()
        self.proxy.billing_interceptor = SimpleNamespace(create_stream_processor=Mock(return_value=SimpleNamespace(process_chunk=AsyncMock(side_effect=lambda x: x), finalize=finalize)))
        body = Body()
        response = await self.forward(body, content_type='text/event-stream')
        await anext(response.stream)
        eof = asyncio.create_task(anext(response.stream))
        await asyncio.wait_for(entered.wait(), 1)
        await self.lose()
        with self.assertRaises(asyncio.CancelledError):
            await asyncio.wait_for(eof, 1)
        self.assert_released(body)

if __name__ == '__main__':
    unittest.main(verbosity=2)
