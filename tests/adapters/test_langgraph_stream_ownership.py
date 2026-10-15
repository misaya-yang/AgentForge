from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_gateway_core.enums import ContentType, StreamEventType

from src.adapters.base import BulkheadAdapter
from src.adapters.langgraph import LangGraphAdapter
from src.models.request import ContentItem, UnifiedRequest
from src.services.registry.service_registry import MemoryRegistryStorage, ServiceRegistry


class TrackedBody(httpx.AsyncByteStream):
    def __init__(self, *, block_close=False, fail_read=False):
        self.close_started = asyncio.Event()
        self.close_allowed = asyncio.Event()
        if not block_close:
            self.close_allowed.set()
        self.closed = 0
        self.fail_read = fail_read

    async def __aiter__(self):
        yield b'event: messages\ndata: [{"type":"ai","content":"hello"},{}]\n\n'
        if self.fail_read:
            raise httpx.ReadError('fixture read failure')
        yield b'event: end\ndata: {}\n\n'

    async def aclose(self):
        self.close_started.set()
        await self.close_allowed.wait()
        self.closed += 1


def request():
    return UnifiedRequest('request', 'service', [ContentItem(ContentType.TEXT, 'question')],
                          user_id='fixture-user', tenant_id='fixture-tenant')


def remote_adapter(client):
    adapter = object.__new__(LangGraphAdapter)
    adapter.remote = True
    adapter.config = {}
    adapter.service = SimpleNamespace(session_enabled=False, metadata={})
    adapter.assistant_id = 'fixture-agent'
    adapter.stream_endpoint = '/runs/stream'
    adapter.connector = SimpleNamespace(_client=client)
    adapter.forward_auth = True
    adapter.auth_user_id = None
    adapter.auth_user_type = 'user'
    return adapter


def configured_bulkhead(adapter):
    # Production registry selection of the configured wrapper, not a fake wrapper.
    adapter.service.metadata = {'bulkhead': {'concurrency_limit': 1}}
    registry = ServiceRegistry(MemoryRegistryStorage())
    return registry._wrap_adapter_bulkhead(adapter.service, adapter, 'langgraph')


def client_for(body):
    def handle(req):
        assert req.method == 'POST' and req.url.path == '/runs/stream'
        assert json.loads(req.content)['metadata']['tenant_id'] == 'fixture-tenant'
        return httpx.Response(200, headers={'content-type': 'text/event-stream'}, stream=body)
    return httpx.AsyncClient(base_url='https://fixture.invalid', transport=httpx.MockTransport(handle))


@pytest.mark.asyncio
@pytest.mark.parametrize('wrapped', [False, True])
async def test_early_close_joins_http_response_before_return(wrapped):
    body = TrackedBody()
    async with client_for(body) as client:
        adapter = remote_adapter(client)
        owner = configured_bulkhead(adapter) if wrapped else adapter
        stream = owner.stream(request())
        first = await anext(stream)
        assert not first.is_final and body.closed == 0
        await stream.aclose()
        assert body.closed == 1
        await stream.aclose()
        assert body.closed == 1
        if wrapped:
            assert owner._semaphore._value == 1


@pytest.mark.asyncio
async def test_repeated_cancel_during_owned_close_holds_bulkhead_until_http_closed():
    body = TrackedBody(block_close=True)
    async with client_for(body) as client:
        owner = configured_bulkhead(remote_adapter(client))
        stream = owner.stream(request())
        await anext(stream)
        closing = asyncio.create_task(stream.aclose())
        try:
            await asyncio.wait_for(body.close_started.wait(), 1)
            closing.cancel()
            await asyncio.sleep(0)
            closing.cancel()
            await asyncio.sleep(0)
            assert not closing.done()
            assert owner._semaphore._value == 0 and body.closed == 0
        finally:
            body.close_allowed.set()
            # GeneratorExit remains the primary termination of aclose().
            await asyncio.wait_for(closing, 1)
        assert body.closed == 1 and owner._semaphore._value == 1


@pytest.mark.asyncio
@pytest.mark.parametrize('wrapped', [False, True])
async def test_normal_completion_preserves_one_final_and_closes_body(wrapped):
    body = TrackedBody()
    async with client_for(body) as client:
        owner = remote_adapter(client)
        if wrapped:
            owner = configured_bulkhead(owner)
        chunks = [chunk async for chunk in owner.stream(request())]
        assert body.closed == 1
        assert sum(chunk.is_final for chunk in chunks) == 1
        assert ''.join(c.content.data for c in chunks if c.event_type == StreamEventType.TEXT_DELTA) == 'hello'


@pytest.mark.asyncio
async def test_remote_read_error_emits_no_success_final_and_closes_body():
    body = TrackedBody(fail_read=True)
    chunks = []
    async with client_for(body) as client:
        owner = configured_bulkhead(remote_adapter(client))
        with pytest.raises(Exception, match='fixture read failure'):
            async for chunk in owner.stream(request()):
                chunks.append(chunk)
        assert body.closed == 1 and owner._semaphore._value == 1
        assert not any(c.is_final for c in chunks)


@pytest.mark.asyncio
async def test_in_process_graph_generator_is_closed_before_wrapper_returns():
    closed = []
    async def events(*args, **kwargs):
        try:
            yield {'event': 'on_chat_model_stream', 'data': {'chunk': {'content': 'hello'}}}
        finally:
            closed.append(True)
    adapter = object.__new__(LangGraphAdapter)
    adapter.remote = False
    adapter._build_run_config = AsyncMock(return_value={})
    adapter.graph = SimpleNamespace(astream_events=events)
    stream = adapter.stream(request())
    assert (await anext(stream)).content.data == 'hello'
    await stream.aclose()
    assert closed == [True]


class ControlledIterator:
    def __init__(self, error=None, close_error=None):
        self.error = error
        self.close_error = close_error
        self.closed = 0

    def __aiter__(self):
        return self

    async def __anext__(self):
        if self.error is not None:
            raise self.error
        raise StopAsyncIteration

    async def aclose(self):
        self.closed += 1
        if self.close_error is not None:
            raise self.close_error


@pytest.mark.asyncio
@pytest.mark.parametrize('primary', [RuntimeError('producer failed'), asyncio.CancelledError('cancelled')])
async def test_bulkhead_preserves_primary_failure_when_owned_close_fails(primary):
    iterator = ControlledIterator(primary, ValueError('close failed'))
    owner = BulkheadAdapter(SimpleNamespace(stream=lambda _req: iterator), concurrency_limit=1)
    with pytest.raises(type(primary)) as caught:
        await anext(owner.stream(request()))
    assert caught.value is primary
    assert iterator.closed == 1 and owner._semaphore._value == 1


@pytest.mark.asyncio
async def test_bulkhead_surfaces_close_failure_after_normal_exhaustion():
    failure = ValueError('close failed')
    iterator = ControlledIterator(close_error=failure)
    owner = BulkheadAdapter(SimpleNamespace(stream=lambda _req: iterator), concurrency_limit=1)
    with pytest.raises(ValueError) as caught:
        await anext(owner.stream(request()))
    assert caught.value is failure
    assert iterator.closed == 1 and owner._semaphore._value == 1


@pytest.mark.asyncio
async def test_bulkhead_supports_iterator_without_aclose():
    class NoClose:
        def __aiter__(self):
            return self
        async def __anext__(self):
            raise StopAsyncIteration
    owner = BulkheadAdapter(SimpleNamespace(stream=lambda _req: NoClose()), concurrency_limit=1)
    assert [chunk async for chunk in owner.stream(request())] == []
    assert owner._semaphore._value == 1
