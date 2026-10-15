from __future__ import annotations

import asyncio
import json
import logging
from datetime import datetime
from typing import Any

from ai_gateway_core.enums import ContentType

from ...models.request import ContentItem, UnifiedRequest

logger = logging.getLogger(__name__)


class TaskQueue:
    async def enqueue(
        self,
        task_id: str,
        request: UnifiedRequest,
        roles: list[str],
        client_ip: str | None,
    ) -> None:
        raise NotImplementedError

    async def dequeue(
        self,
    ) -> tuple[str, UnifiedRequest, list[str], str | None]:
        raise NotImplementedError


class MemoryTaskQueue(TaskQueue):
    def __init__(self):
        self._queue: asyncio.PriorityQueue = asyncio.PriorityQueue()

    async def enqueue(
        self,
        task_id: str,
        request: UnifiedRequest,
        roles: list[str],
        client_ip: str | None,
    ) -> None:
        await self._queue.put((-request.priority, task_id, request, roles, client_ip))

    async def dequeue(
        self,
    ) -> tuple[str, UnifiedRequest, list[str], str | None]:
        _, task_id, request, roles, client_ip = await self._queue.get()
        return task_id, request, roles, client_ip


def _serialize_task_envelope(
    task_id: str,
    request: UnifiedRequest,
    roles: list[str],
    client_ip: str | None,
) -> str:
    """Serialize task execution envelope for durable storage."""
    inputs_data = []
    for item in request.inputs:
        data_val = item.data.decode("utf-8") if isinstance(item.data, bytes) else item.data
        inputs_data.append({
            "type": item.type.value if hasattr(item.type, "value") else str(item.type),
            "data": data_val,
            "url": item.url,
            "mime_type": item.mime_type,
            "metadata": item.metadata,
        })

    payload = {
        "task_id": task_id,
        "roles": roles,
        "client_ip": client_ip,
        "request": {
            "request_id": request.request_id,
            "service_id": request.service_id,
            "inputs": inputs_data,
            "session_id": request.session_id,
            "context": request.context,
            "parameters": request.parameters,
            "callback_url": request.callback_url,
            "priority": request.priority,
            "user_id": request.user_id,
            "tenant_id": request.tenant_id,
            "timestamp": request.timestamp.isoformat(),
        },
    }
    return json.dumps(payload, ensure_ascii=False)


def _deserialize_task_envelope(
    raw_json: str,
) -> tuple[str, UnifiedRequest, list[str], str | None]:
    """Deserialize task execution envelope back into domain objects."""
    payload = json.loads(raw_json)
    task_id = payload["task_id"]
    roles = payload.get("roles", [])
    client_ip = payload.get("client_ip")
    req_dict = payload["request"]

    content_items = []
    for raw_item in req_dict.get("inputs", []):
        raw_type = raw_item.get("type", "text")
        try:
            c_type = ContentType(raw_type)
        except Exception:
            c_type = ContentType.TEXT
        content_items.append(
            ContentItem(
                type=c_type,
                data=raw_item.get("data"),
                url=raw_item.get("url"),
                mime_type=raw_item.get("mime_type"),
                metadata=raw_item.get("metadata"),
            )
        )

    ts_str = req_dict.get("timestamp")
    try:
        ts = datetime.fromisoformat(ts_str) if ts_str else datetime.utcnow()
    except Exception:
        ts = datetime.utcnow()

    request = UnifiedRequest(
        request_id=req_dict["request_id"],
        service_id=req_dict["service_id"],
        inputs=content_items,
        session_id=req_dict.get("session_id"),
        context=req_dict.get("context"),
        parameters=req_dict.get("parameters"),
        callback_url=req_dict.get("callback_url"),
        priority=int(req_dict.get("priority", 0)),
        user_id=req_dict.get("user_id", ""),
        tenant_id=req_dict.get("tenant_id", ""),
        timestamp=ts,
    )
    return task_id, request, roles, client_ip


class RedisTaskQueue(TaskQueue):
    """Durable Redis-backed priority queue for gateway async tasks.

    Uses Redis Sorted Sets (ZADD with score = -priority) for strict priority scheduling
    with graceful fallback to MemoryTaskQueue if Redis is unavailable.
    """

    def __init__(
        self,
        redis_client: Any,
        queue_key: str = "gateway:tasks:priority_queue",
        fallback_queue: TaskQueue | None = None,
    ):
        self.redis = redis_client
        self.queue_key = queue_key
        self.fallback = fallback_queue or MemoryTaskQueue()

    async def enqueue(
        self,
        task_id: str,
        request: UnifiedRequest,
        roles: list[str],
        client_ip: str | None,
    ) -> None:
        if self.redis is None:
            await self.fallback.enqueue(task_id, request, roles, client_ip)
            return

        envelope_json = _serialize_task_envelope(task_id, request, roles, client_ip)
        score = float(-request.priority)

        try:
            if hasattr(self.redis, "zadd"):
                res = self.redis.zadd(self.queue_key, {envelope_json: score})
                if asyncio.iscoroutine(res):
                    await res
            else:
                await self.fallback.enqueue(task_id, request, roles, client_ip)
        except Exception as exc:
            logger.warning(f"RedisTaskQueue.enqueue failed, falling back to memory queue: {exc}")
            await self.fallback.enqueue(task_id, request, roles, client_ip)

    async def dequeue(
        self,
    ) -> tuple[str, UnifiedRequest, list[str], str | None]:
        if self.redis is None:
            return await self.fallback.dequeue()

        if not hasattr(self.redis, "zpopmin"):
            return await self.fallback.dequeue()

        failures = 0
        while True:
            try:
                res = self.redis.zpopmin(self.queue_key, count=1)
                popped = await res if asyncio.iscoroutine(res) else res
                if popped:
                    member, _score = popped[0]
                    raw_json = member.decode("utf-8") if isinstance(member, bytes) else str(member)
                    return _deserialize_task_envelope(raw_json)
                failures = 0
            except Exception:
                failures += 1
                if failures == 1 or failures % 60 == 0:
                    logger.warning("Redis task dequeue unavailable; waiting for queue recovery")
                # Never reconstruct/re-enqueue a possibly popped envelope. The
                # next poll can only claim a separate remaining queue member.
                if not isinstance(self.fallback, MemoryTaskQueue):
                    # Preserve custom fallback ownership: its dequeue may not
                    # support cancellation or nonblocking availability checks.
                    return await self.fallback.dequeue()

            if hasattr(self.fallback, "_queue") and not self.fallback._queue.empty():
                return await self.fallback.dequeue()
            # Do not abandon Redis forever on an empty in-process fallback.
            # Cancellation propagates through the pop, fallback and this wait.
            await asyncio.sleep(1.0 if failures else 0.1)
