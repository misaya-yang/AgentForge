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
@pytest.mark.parametrize("outcome", ["success", "failed", "cancelled", "session", "recovered_read"])
async def test_public_submit_to_real_dispatcher_worker_and_result(queue_kind, outcome):
    queue = MemoryTaskQueue() if queue_kind == "memory" else RedisTaskQueue(RecordingRedis())
    storage = MemoryTaskStorage()
    manager = TaskManager(storage, queue)
    answer = UnifiedResponse(
        "consumer-request", "success", [ContentItem(ContentType.IMAGE, b"\x00\xff")]
    )
    adapter = MagicMock()
    adapter.invoke = AsyncMock(return_value=answer)
    if outcome == "failed":
        adapter.invoke.side_effect = ValueError("synthetic adapter failure")
    service = ServiceDefinition(
        service_id="agent_service",
        name="Agent Service",
        service_type=ServiceType.LANGGRAPH,
        connector_type=TransportType.HTTP,
        supported_modes=[InvocationMode.ASYNC],
        accepted_content_types=[ContentType.TEXT],
        output_content_types=[ContentType.IMAGE],
        session_enabled=outcome == "session",
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
        if outcome == "session":
            assert envelope[1].session_id == receipt.json()["session_id"] == "owned-session"
        if outcome == "cancelled":
            assert (await client.delete("/tasks/" + task_id)).status_code == 200
        if outcome == "recovered_read":
            authoritative_get = storage.get
            reads = 0
            async def unavailable_once(task_id):
                nonlocal reads
                reads += 1
                if reads == 1:
                    raise ConnectionError("synthetic pre-claim read outage")
                return await authoritative_get(task_id)
            storage.get = unavailable_once
        # Usage persistence is also external; dispatch and its admission remain real.
        with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
            await worker._process(*envelope)
            await worker._process(*envelope)
        detail = await client.get("/tasks/" + task_id)
        expected_status = {
            "success": "completed",
            "recovered_read": "completed",
            "session": "completed",
            "failed": "failed",
            "cancelled": "cancelled",
        }[outcome]
        assert detail.json()["status"] == expected_status
        assert adapter.invoke.await_count == (0 if outcome == "cancelled" else 1)
        result = await client.get("/tasks/" + task_id + "/result")
        if outcome in {"success", "session", "recovered_read"}:
            assert result.status_code == 200 and result.json()["outputs"][0]["data"] == "AP8="
            returned = await dispatcher.get_task_result(task_id)
            returned.outputs[0].data = "caller mutation"
            assert (await manager.get_result(task_id)).outputs[0].data == b"\x00\xff"
            assert (await client.delete("/tasks/" + task_id)).status_code == 409
        else:
            assert result.status_code == 404
    await manager.close()
