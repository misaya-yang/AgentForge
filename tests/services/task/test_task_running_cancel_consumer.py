"""Actual submit/dispatcher/queue/worker/result consumers; no external I/O."""

import asyncio
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import httpx
import pytest
from ai_gateway_core.enums import InvocationMode, ServiceType, TransportType
from fastapi import FastAPI

from src.api.v1 import submit, tasks
from src.core.auth.rbac import RBAC
from src.core.auth.user_resolver import UserContext
from src.core.gateway.dispatcher import GatewayDispatcher
from src.core.gateway.validator import RequestValidator
from src.models.request import ContentItem, ContentType
from src.models.response import UnifiedResponse
from src.models.service import ServiceDefinition
from src.services.task.task_manager import MemoryTaskStorage, TaskManager
from src.services.task.task_queue import MemoryTaskQueue, RedisTaskQueue
from src.services.task.task_worker import TaskWorker


class RecordingRedis:
    """Record actual RedisTaskQueue serialization; no Redis server/claim guarantee."""

    def __init__(self):
        self.rows = {}
        self.popped = []

    async def zadd(self, key, rows):
        assert key == "gateway:tasks:priority_queue"
        self.rows.update(rows)

    async def zpopmin(self, key, count):
        assert key == "gateway:tasks:priority_queue" and count == 1
        member = min(self.rows, key=self.rows.get)
        result = (member, self.rows.pop(member))
        self.popped.append(result)
        return [result]


@pytest.mark.parametrize("queue_kind", ["memory", "redis_envelope"])
async def test_public_running_cancel_stops_local_continuation(queue_kind):
    queue = MemoryTaskQueue() if queue_kind == "memory" else RedisTaskQueue(RecordingRedis())
    storage = MemoryTaskStorage()
    manager = TaskManager(storage, queue)
    answer = UnifiedResponse(
        "consumer-request", "success", [ContentItem(ContentType.IMAGE, b"\x00\xff")]
    )
    adapter = MagicMock()
    entered, stopped, release = asyncio.Event(), asyncio.Event(), asyncio.Event()
    effects = []
    async def controlled_invoke(*args, **kwargs):
        effects.append("upstream accepted before cancellation")
        entered.set()
        try:
            await release.wait()
            effects.append("avoidable continuation after cancellation")
            return answer
        finally:
            stopped.set()
    adapter.invoke = AsyncMock(side_effect=controlled_invoke)
    service = ServiceDefinition(
        service_id="agent_service",
        name="Agent Service",
        service_type=ServiceType.LANGGRAPH,
        connector_type=TransportType.HTTP,
        supported_modes=[InvocationMode.ASYNC],
        accepted_content_types=[ContentType.TEXT],
        output_content_types=[ContentType.IMAGE],
        session_enabled=False,
    )
    # External service lookup, rate-limit authority, session persistence and the
    # provider adapter are controlled seams. Generic task behavior stays real.
    registry = SimpleNamespace(
        get=AsyncMock(return_value=service), get_adapter=MagicMock(return_value=adapter)
    )
    sessions = SimpleNamespace(
        get_or_create=AsyncMock(return_value=SimpleNamespace(session_id="owned-session")),
        add_message=AsyncMock(),
    )
    rbac = RBAC({"user": ["conversation:playground:access"]})
    dispatcher = GatewayDispatcher(
        registry=registry,
        validator=RequestValidator(rbac),
        rbac=rbac,
        rate_limiter=SimpleNamespace(enforce=AsyncMock()),
        task_manager=manager,
        session_manager=sessions,
    )
    worker = TaskWorker(queue, storage, dispatcher, manager)
    user = UserContext(
        user_id="consumer-user", tenant_id="consumer-tenant", roles=["user"], is_authenticated=True
    )
    app = FastAPI()
    app.include_router(submit.router)
    app.include_router(tasks.router)
    app.dependency_overrides[submit.get_dispatcher] = lambda: dispatcher
    app.dependency_overrides[submit.get_user_context] = lambda: user
    app.dependency_overrides[tasks.get_task_manager] = lambda: manager
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://fixture"
    ) as client:
        receipt = await client.post(
            "/submit",
            json={
                "request_id": "consumer-request",
                "service_id": "agent_service",
                "inputs": [{"type": "text", "data": "hello"}],
                "user_id": "forged-user",
                "tenant_id": "forged-tenant",
                "parameters": {"nested": [1]},
            },
        )
        assert receipt.status_code == 200, receipt.text
        task_id = receipt.json()["task_id"]
        pending = await storage.get(task_id)
        assert pending.user_id == user.user_id and pending.tenant_id == user.tenant_id
        if queue_kind == "redis_envelope":
            # Fail if RedisTaskQueue silently took its memory fallback. Inspect
            # the actual serialized member before exercising deserialization.
            assert queue.fallback._queue.empty()
            assert len(queue.redis.rows) == 1
            serialized, score = next(iter(queue.redis.rows.items()))
            recorded = json.loads(serialized)
            assert recorded["task_id"] == task_id and score == 0
            assert recorded["roles"] == user.roles
            assert recorded["request"]["user_id"] == user.user_id
            assert recorded["request"]["tenant_id"] == user.tenant_id
            assert recorded["request"]["request_id"] == "consumer-request"
            assert recorded["request"]["service_id"] == "agent_service"
            assert recorded["request"]["parameters"] == {"nested": [1]}
            assert recorded["request"]["session_id"] == receipt.json()["session_id"]
        envelope = await asyncio.wait_for(queue.dequeue(), timeout=1)
        if queue_kind == "redis_envelope":
            assert queue.redis.popped == [(serialized, score)]
            assert not queue.redis.rows and queue.fallback._queue.empty()
        assert envelope[1].parameters == {"nested": [1]}
        with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
            processing = asyncio.create_task(worker._process(*envelope))
            await asyncio.wait_for(entered.wait(), 1)
            cancellation = await client.delete("/tasks/" + task_id)
            assert cancellation.status_code == 200
            assert (await client.get("/tasks/" + task_id)).json()["status"] == "cancelled"
            try:
                await asyncio.wait_for(stopped.wait(), 1.5)
                locally_stopped = True
            except TimeoutError:
                locally_stopped = False
            finally:
                release.set()
                await asyncio.wait_for(processing, 1)
            assert locally_stopped, effects
            assert effects == ["upstream accepted before cancellation"]
        detail = await client.get("/tasks/" + task_id)
        assert detail.json()["status"] == "cancelled"
        adapter.invoke.assert_awaited_once()
        result = await client.get("/tasks/" + task_id + "/result")
        assert result.status_code == 404
    await manager.close()
