"""Session prerequisite through the real adapter/HTTPConnector; no remote service."""
from __future__ import annotations

import asyncio
import json
from contextlib import asynccontextmanager
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_gateway_core.enums import ContentType
from ai_gateway_core.exceptions import ValidationFailedError

from src.adapters.langgraph import LangGraphAdapter
from src.models.request import ContentItem, UnifiedRequest
from src.models.service import ServiceDefinition

SESSION = "00000000-0000-4000-8000-000000000001"
OTHER = "00000000-0000-4000-8000-000000000002"
SECRET = "SYNTHETIC_PRIVATE_THREAD_DETAIL"


def request(session=SESSION):
    return UnifiedRequest("request", "service", [ContentItem(ContentType.TEXT, "question")],
                          session_id=session, user_id="fixture-user", tenant_id="fixture-tenant")


class Remote:
    def __init__(self, create="success", readback="success"):
        self.create, self.readback = create, readback
        self.calls = []
        self.entered = asyncio.Event()
        self.release = asyncio.Event()
        self.thread_id = None

    async def handle(self, req):
        self.calls.append(req)
        if req.url.path == "/threads":
            payload = json.loads(req.content)
            self.thread_id = payload["thread_id"]
            assert payload["if_exists"] == "do_nothing"
            assert payload["metadata"]["user_id"] == "fixture-user"
            assert req.headers["x-user-id"] == "fixture-user"
            kind = self.create
        elif req.method == "GET":
            assert req.url.path == f"/threads/{self.thread_id}"
            kind = self.readback
        else:
            assert req.url.path in (f"/threads/{self.thread_id}/runs/wait",
                                    f"/threads/{self.thread_id}/runs/stream")
            if req.url.path.endswith("/stream"):
                return httpx.Response(200, headers={"content-type": "text/event-stream"},
                                      text='event: messages\ndata: [{"type":"ai","content":"answer"},{}]\n\nevent: end\ndata: {}\n\n')
            return httpx.Response(200, json={"messages": [{"type": "ai", "content": "answer"}]})
        if kind == "block":
            self.entered.set()
            await self.release.wait()
        if isinstance(kind, int):
            return httpx.Response(kind, json={"detail": SECRET})
        if kind == "timeout":
            raise httpx.ReadTimeout(SECRET, request=req)
        if kind == "malformed":
            return httpx.Response(200, headers={"content-type": "application/json"}, content=b'{invalid')
        if kind == "empty":
            return httpx.Response(204)
        if kind == "non_json":
            return httpx.Response(200, text=SECRET)
        if kind == "missing":
            return httpx.Response(200, json={})
        if kind == "embedded":
            return httpx.Response(200, json={"thread_id": self.thread_id, "__error__": SECRET})
        if kind == "list":
            return httpx.Response(200, json=[])
        return httpx.Response(200, json={"thread_id": OTHER if kind == "mismatch" else self.thread_id})


@asynccontextmanager
async def adapter(remote, *, cached=None, redis_failure=False):
    service = ServiceDefinition("service", "Fixture", session_enabled=True,
                                connector_config={"base_url": "https://fixture.invalid"})
    subject = LangGraphAdapter(service)
    await subject.connector.close()
    subject.connector._client = httpx.AsyncClient(base_url="https://fixture.invalid",
                                                transport=httpx.MockTransport(remote.handle))
    subject._session_to_thread_map = {}
    redis = AsyncMock()
    redis.get_thread_mapping.return_value = cached
    if redis_failure:
        redis.get_thread_mapping.side_effect = OSError("fixture unavailable")
        redis.set_thread_mapping.side_effect = OSError("fixture unavailable")
    subject._get_redis_storage = AsyncMock(return_value=redis)
    try:
        yield subject, redis
    finally:
        await subject.connector.close()


async def run(subject, mode, req=None):
    req = req or request()
    if mode == "invoke":
        return await subject.invoke(req)
    return [chunk async for chunk in subject.stream(req)]


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["invoke", "stream"])
@pytest.mark.parametrize("failure", [401, 403, 429, 500, "timeout", "malformed", "empty", "non_json", "missing", "embedded", "list", "mismatch"])
async def test_create_failure_never_dispatches_run_or_publishes_cache(mode, failure, caplog):
    remote = Remote(failure)
    async with adapter(remote) as (subject, redis):
        with pytest.raises(ValidationFailedError) as caught:
            await run(subject, mode)
        assert subject._session_to_thread_map == {}
        redis.set_thread_mapping.assert_not_awaited()
    assert [r.url.path for r in remote.calls] == ["/threads"]
    assert SECRET not in str(caught.value)
    assert SECRET not in caplog.text


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["invoke", "stream"])
async def test_confirmed_thread_is_reused_without_duplicate_creation(mode):
    remote = Remote()
    async with adapter(remote) as (subject, redis):
        await run(subject, mode)
        await run(subject, mode)
        assert subject._session_to_thread_map == {SESSION: SESSION}
        redis.set_thread_mapping.assert_awaited_once_with(SESSION, SESSION)
    assert [r.url.path for r in remote.calls] == ["/threads", f"/threads/{SESSION}/runs/{'wait' if mode == 'invoke' else 'stream'}"] + [remote.calls[-1].url.path]


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["invoke", "stream"])
async def test_conflict_requires_exact_readback_before_run(mode):
    remote = Remote(409)
    async with adapter(remote) as (subject, redis):
        await run(subject, mode)
        redis.set_thread_mapping.assert_awaited_once_with(SESSION, SESSION)
    assert [(r.method, r.url.path) for r in remote.calls[:2]] == [
        ("POST", "/threads"), ("GET", f"/threads/{SESSION}")]
    assert remote.calls[0].headers["x-user-id"] == remote.calls[1].headers["x-user-id"]
    assert len(remote.calls) == 3


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", [403, 404, "timeout", "empty", "missing", "mismatch", "embedded"])
async def test_unconfirmed_conflict_never_dispatches_or_caches(failure):
    remote = Remote(409, failure)
    async with adapter(remote) as (subject, redis):
        with pytest.raises(ValidationFailedError):
            await run(subject, "invoke")
        assert subject._session_to_thread_map == {}
        redis.set_thread_mapping.assert_not_awaited()
    assert len(remote.calls) == 2


@pytest.mark.asyncio
@pytest.mark.parametrize("conflict", [False, True])
@pytest.mark.parametrize("mode", ["invoke", "stream"])
async def test_cancelled_create_or_readback_never_caches_or_dispatches(conflict, mode):
    remote = Remote(409 if conflict else "block", "block")
    async with adapter(remote) as (subject, redis):
        task = asyncio.create_task(run(subject, mode))
        await asyncio.wait_for(remote.entered.wait(), 1)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        assert subject._session_to_thread_map == {}
        redis.set_thread_mapping.assert_not_awaited()
    assert len(remote.calls) == (2 if conflict else 1)


@pytest.mark.asyncio
async def test_explicit_retry_after_unknown_create_is_not_automatic():
    remote = Remote("timeout")
    async with adapter(remote) as (subject, redis):
        with pytest.raises(ValidationFailedError):
            await run(subject, "invoke")
        assert len(remote.calls) == 1
        remote.create = "success"
        await run(subject, "invoke")
        redis.set_thread_mapping.assert_awaited_once_with(SESSION, SESSION)
    assert [r.url.path for r in remote.calls] == ["/threads", "/threads", f"/threads/{SESSION}/runs/wait"]


@pytest.mark.asyncio
async def test_existing_redis_mapping_and_redis_unavailability_remain_supported():
    remote = Remote()
    remote.thread_id = SESSION
    async with adapter(remote, cached=SESSION) as (subject, redis):
        await run(subject, "invoke")
        redis.set_thread_mapping.assert_not_awaited()
    assert [r.url.path for r in remote.calls] == [f"/threads/{SESSION}/runs/wait"]
    remote = Remote()
    async with adapter(remote, redis_failure=True) as (subject, _):
        await run(subject, "invoke")
        assert subject._session_to_thread_map == {SESSION: SESSION}
    assert len(remote.calls) == 2


@pytest.mark.asyncio
async def test_non_uuid_session_maps_only_after_confirmed_generated_identity():
    remote = Remote()
    async with adapter(remote) as (subject, redis):
        await run(subject, "invoke", request("human-session"))
        assert subject._session_to_thread_map == {"human-session": remote.thread_id}
        redis.set_thread_mapping.assert_awaited_once_with("human-session", remote.thread_id)
    assert remote.thread_id != "human-session"
