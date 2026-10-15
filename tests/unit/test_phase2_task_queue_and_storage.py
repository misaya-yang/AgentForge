"""Unit tests for Phase 2: RedisTaskQueue and Incremental Session Storage."""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest
from ai_gateway_core.enums import ContentType

from src.models.request import ContentItem, UnifiedRequest
from src.services.task.task_queue import (
    MemoryTaskQueue,
    RedisTaskQueue,
    _deserialize_task_envelope,
    _serialize_task_envelope,
)


@pytest.mark.asyncio
async def test_memory_task_queue_priority_ordering() -> None:
    queue = MemoryTaskQueue()

    req_low = UnifiedRequest(
        request_id="req_low",
        service_id="svc_test",
        inputs=[ContentItem(type=ContentType.TEXT, data="low priority")],
        priority=1,
    )
    req_high = UnifiedRequest(
        request_id="req_high",
        service_id="svc_test",
        inputs=[ContentItem(type=ContentType.TEXT, data="high priority")],
        priority=10,
    )

    await queue.enqueue("task_low", req_low, ["user"], "127.0.0.1")
    await queue.enqueue("task_high", req_high, ["admin"], "127.0.0.1")

    # Higher priority (10) must dequeue first
    task_id1, req1, roles1, ip1 = await queue.dequeue()
    assert task_id1 == "task_high"
    assert req1.priority == 10

    task_id2, req2, roles2, ip2 = await queue.dequeue()
    assert task_id2 == "task_low"
    assert req2.priority == 1


def test_task_envelope_serialization_roundtrip() -> None:
    req = UnifiedRequest(
        request_id="req_test_123",
        service_id="agent-runtime",
        inputs=[ContentItem(type=ContentType.TEXT, data="Hello world!")],
        session_id="sess_abc",
        priority=5,
        user_id="usr_001",
        tenant_id="tenant_001",
    )

    raw_json = _serialize_task_envelope("task_xyz", req, ["admin"], "10.0.0.1")
    task_id, deserialized_req, roles, client_ip = _deserialize_task_envelope(raw_json)

    assert task_id == "task_xyz"
    assert deserialized_req.request_id == "req_test_123"
    assert deserialized_req.service_id == "agent-runtime"
    assert deserialized_req.session_id == "sess_abc"
    assert deserialized_req.priority == 5
    assert len(deserialized_req.inputs) == 1
    assert deserialized_req.inputs[0].data == "Hello world!"
    assert roles == ["admin"]
    assert client_ip == "10.0.0.1"


@pytest.mark.asyncio
async def test_redis_task_queue_mock_enqueue_dequeue() -> None:
    mock_redis = MagicMock()
    storage: list[tuple[bytes, float]] = []

    def mock_zadd(key: str, mapping: dict[str, float]) -> None:
        for member, score in mapping.items():
            storage.append((member.encode("utf-8"), score))
        storage.sort(key=lambda x: x[1])

    def mock_zpopmin(key: str, count: int = 1) -> list[tuple[bytes, float]]:
        if not storage:
            return []
        return [storage.pop(0)]

    mock_redis.zadd.side_effect = mock_zadd
    mock_redis.zpopmin.side_effect = mock_zpopmin

    queue = RedisTaskQueue(redis_client=mock_redis)

    req1 = UnifiedRequest(
        request_id="r1",
        service_id="svc1",
        inputs=[ContentItem(type=ContentType.TEXT, data="first")],
        priority=2,
    )
    req2 = UnifiedRequest(
        request_id="r2",
        service_id="svc1",
        inputs=[ContentItem(type=ContentType.TEXT, data="second")],
        priority=8,
    )

    await queue.enqueue("t1", req1, [], None)
    await queue.enqueue("t2", req2, [], None)

    # Score is -priority, so -8 pops before -2
    task_id1, popped_req1, _, _ = await queue.dequeue()
    assert task_id1 == "t2"
    assert popped_req1.priority == 8

    task_id2, popped_req2, _, _ = await queue.dequeue()
    assert task_id2 == "t1"
    assert popped_req2.priority == 2


@pytest.mark.asyncio
async def test_database_storage_append_session_history() -> None:
    from ai_gateway_core.persistence.database import DatabaseStorage

    storage = DatabaseStorage("postgresql://fake:5432/db", enabled=True, auto_init=False)
    # When pool is None, returns False safely
    res = await storage.append_session_history("sess_001", [{"role": "user", "content": "hi"}])
    assert res is False
