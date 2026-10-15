"""Explicit public stream closure owns HTTP cleanup without remote mutation."""
from __future__ import annotations

import asyncio
import unittest
from contextlib import aclosing
from unittest.mock import patch

import httpx
from ai_assistant import AssistantClient, StreamError
from ai_assistant.models.events import EventType, StreamEvent

SURFACES = ("client", "chat", "knowledge", "threads")


class ObservedBody(httpx.AsyncByteStream):
    def __init__(self):
        self.closed = 0
        self.cleanup_error = None
        self.body_error = None
        self.reading = asyncio.Event()
        self.release = asyncio.Event()

    async def __aiter__(self):
        yield b'data: {"event_type":"text_delta","data":{"content":"first"}}\n\n'
        self.reading.set()
        await self.release.wait()
        if self.body_error is not None:
            raise self.body_error

    async def aclose(self):
        self.closed += 1
        if self.cleanup_error is not None:
            raise self.cleanup_error


class CustomEvents:
    def __init__(self, primary=None, cleanup=None):
        self.primary = primary
        self.cleanup = cleanup
        self.calls = 0
        self.closed = 0

    def __aiter__(self):
        return self

    async def __anext__(self):
        self.calls += 1
        if self.calls == 1:
            return StreamEvent(event_type=EventType.TEXT_DELTA, data={"content": "first"})
        if self.primary is not None:
            raise self.primary
        raise StopAsyncIteration

    async def aclose(self):
        self.closed += 1
        if self.cleanup is not None:
            raise self.cleanup


class NoCloseEvents:
    def __init__(self):
        self.calls = 0

    def __aiter__(self):
        return self

    async def __anext__(self):
        self.calls += 1
        if self.calls == 1:
            return StreamEvent(event_type=EventType.TEXT_DELTA, data={"content": "first"})
        raise StopAsyncIteration


class StreamLifecycleTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.client = AssistantClient("synthetic-key", base_url="https://gateway.example")
        await self.client._transport._client.aclose()
        self.requests = []
        self.body = ObservedBody()

        def handle(request):
            self.requests.append(request)
            return httpx.Response(200, stream=self.body)

        self.client._transport._client = httpx.AsyncClient(
            base_url="https://gateway.example", transport=httpx.MockTransport(handle),
        )

    async def asyncTearDown(self):
        await self.client.close()

    def stream(self, surface):
        if surface == "client":
            return self.client.stream("hello")
        if surface == "chat":
            return self.client.chat.stream("hello")
        if surface == "knowledge":
            return self.client.knowledge.ask_stream("hello", ["dataset-one"])
        return self.client.threads.events("thread-one")

    def substitute(self, surface, events):
        name = "stream_sse_get" if surface == "threads" else "stream_sse"
        return patch.object(self.client._transport, name, return_value=events)

    async def check_close(self, surface):
        events = self.stream(surface)
        await anext(events)
        self.assertEqual(self.body.closed, 0)
        await events.aclose()
        self.assertEqual(self.body.closed, 1, "public close must await HTTP response cleanup")
        await events.aclose()
        self.assertEqual(self.body.closed, 1)
        self.assertEqual(len(self.requests), 1)
        self.assertEqual(self.requests[0].method, "GET" if surface == "threads" else "POST")

    async def check_unstarted(self, surface):
        await self.stream(surface).aclose()
        self.assertEqual(self.requests, [])
        self.assertEqual(self.body.closed, 0)

    async def check_break(self, surface):
        async with aclosing(self.stream(surface)) as events:
            async for _ in events:
                break
        self.assertEqual(self.body.closed, 1)
        self.assertEqual(len(self.requests), 1)

    async def check_cancel(self, surface):
        events = self.stream(surface)
        await anext(events)
        pending = asyncio.create_task(anext(events))
        await asyncio.wait_for(self.body.reading.wait(), 1)
        pending.cancel("caller stopped observation")
        with self.assertRaises(asyncio.CancelledError):
            await pending
        self.assertEqual(self.body.closed, 1)
        self.assertEqual(len(self.requests), 1)

    async def check_transport_cancel_cleanup(self, surface):
        self.body.cleanup_error = ValueError("response cleanup failure")
        events = self.stream(surface)
        await anext(events)
        pending = asyncio.create_task(anext(events))
        await asyncio.wait_for(self.body.reading.wait(), 1)
        pending.cancel("original read cancellation")
        observed = None
        try:
            await pending
        except BaseException as exc:
            observed = exc
        self.assertIsInstance(observed, asyncio.CancelledError)
        self.assertEqual(observed.args, ("original read cancellation",))
        self.assertEqual(self.body.closed, 1)
        self.assertEqual(len(self.requests), 1)

    async def check_transport_error_cleanup(self, surface):
        primary = RuntimeError("original response read failure")
        self.body.body_error = primary
        self.body.cleanup_error = ValueError("response cleanup failure")
        self.body.release.set()
        events = self.stream(surface)
        await anext(events)
        with self.assertRaises(StreamError) as caught:
            await anext(events)
        self.assertIs(caught.exception.__cause__, primary)
        self.assertEqual(self.body.closed, 1)
        self.assertEqual(len(self.requests), 1)

    async def check_exhaustion(self, surface):
        self.body.release.set()
        result = [event async for event in self.stream(surface)]
        self.assertEqual(len(result), 1)
        self.assertEqual(self.body.closed, 1)
        self.assertEqual(len(self.requests), 1)

    async def check_no_close(self, surface):
        source = NoCloseEvents()
        with self.substitute(surface, source):
            self.assertEqual(len([event async for event in self.stream(surface)]), 1)

    async def check_primary_error(self, surface):
        primary = RuntimeError("original stream failure")
        source = CustomEvents(primary=primary, cleanup=ValueError("cleanup failure"))
        with self.substitute(surface, source):
            events = self.stream(surface)
            await anext(events)
            with self.assertRaises(RuntimeError) as caught:
                await anext(events)
            self.assertIs(caught.exception, primary)
        self.assertEqual(source.closed, 1)

    async def check_primary_cancel(self, surface):
        primary = asyncio.CancelledError("original cancellation")
        source = CustomEvents(primary=primary, cleanup=ValueError("cleanup failure"))
        with self.substitute(surface, source):
            events = self.stream(surface)
            await anext(events)
            with self.assertRaises(asyncio.CancelledError) as caught:
                await anext(events)
            self.assertIs(caught.exception, primary)
        self.assertEqual(source.closed, 1)

    async def check_close_error(self, surface):
        cleanup = ValueError("cleanup failure")
        source = CustomEvents(cleanup=cleanup)
        with self.substitute(surface, source):
            events = self.stream(surface)
            await anext(events)
            with self.assertRaises(ValueError) as caught:
                await events.aclose()
            self.assertIs(caught.exception, cleanup)
        self.assertEqual(source.closed, 1)


# Keep each API/case separately visible to unittest and pytest.
def case(check, surface):
    async def test(self):
        await getattr(self, "check_" + check)(surface)
    return test


for _surface in SURFACES:
    for _check in ("close", "unstarted", "break", "cancel", "exhaustion", "no_close",
                   "primary_error", "primary_cancel", "close_error",
                   "transport_cancel_cleanup", "transport_error_cleanup"):
        setattr(StreamLifecycleTests, f"test_{_surface}_{_check}", case(_check, _surface))
