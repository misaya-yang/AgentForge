from __future__ import annotations

import json

import pytest
from ai_gateway_core.enums import TransportType

from src.models.service import ServiceDefinition
from src.transports.message_queue import MessageQueueConnector


class FakeStreamRedis:
    def __init__(self) -> None:
        self.streams: dict[str, list[tuple[bytes, dict[bytes, bytes]]]] = {}
        self.groups: set[tuple[str, str]] = set()
        self.pending: dict[tuple[str, str], set[bytes]] = {}
        self._seq = 0

    async def xgroup_create(  # noqa: A002
        self,
        name: str,
        groupname: str,
        id: str = "0",  # noqa: ARG002
        mkstream: bool = True,
    ) -> None:
        self.groups.add((name, groupname))
        if mkstream:
            self.streams.setdefault(name, [])

    async def xadd(self, name: str, fields: dict[str, str | bytes], **_kwargs) -> bytes:
        self._seq += 1
        message_id = f"{self._seq}-0".encode()
        encoded = {
            (key.encode() if isinstance(key, str) else key): (
                value.encode() if isinstance(value, str) else value
            )
            for key, value in fields.items()
        }
        self.streams.setdefault(name, []).append((message_id, encoded))
        return message_id

    async def xreadgroup(
        self,
        *,
        groupname: str,
        consumername: str,
        streams: dict[str, str],
        count: int = 1,
        block: int | None = None,
    ):
        del consumername, block
        result = []
        for stream, last_id in streams.items():
            messages = []
            pending = self.pending.setdefault((stream, groupname), set())
            for message_id, fields in self.streams.get(stream, []):
                if last_id == ">" and message_id in pending:
                    continue
                if last_id == ">" and message_id not in pending:
                    pending.add(message_id)
                    messages.append((message_id, fields))
                if len(messages) >= count:
                    break
            if messages:
                result.append((stream.encode(), messages))
        return result

    async def xack(self, stream: str, group: str, message_id: bytes | str) -> int:
        encoded = message_id.encode() if isinstance(message_id, str) else message_id
        self.pending.setdefault((stream, group), set()).discard(encoded)
        return 1

    async def xpending(self, stream: str, group: str):
        return {"pending": len(self.pending.setdefault((stream, group), set()))}

    async def xrange(self, stream: str):
        return list(self.streams.get(stream, []))

    async def hget(self, key: str, field: str):  # noqa: ARG002
        return None

    async def hset(self, key: str, field: str, value: str) -> int:  # noqa: ARG002
        return 1

    async def hincrby(self, key: str, field: str, amount: int) -> int:  # noqa: ARG002
        return amount

    async def aclose(self) -> None:
        return None


@pytest.mark.asyncio
async def test_message_queue_connector_publishes_by_priority_and_consumes_with_ack() -> None:
    redis = FakeStreamRedis()
    service = ServiceDefinition(
        service_id="queue-service",
        name="Queue Service",
        connector_type=TransportType.MESSAGE_QUEUE,
        connector_config={
            "redis_url": "redis://unused",
            "streams": {
                "high": "queue:high",
                "normal": "queue:normal",
                "low": "queue:low",
            },
            "group": "workers",
        },
    )
    connector = MessageQueueConnector(service, client=redis)

    try:
        published = await connector.request(
            "POST",
            "/publish",
            json={"task": "index"},
            priority="high",
        )
        assert published["stream"] == "queue:high"

        consumed = await connector.request("GET", "/consume", priority="high", consumer="worker-a")
        assert consumed["payload"] == {"task": "index"}
        assert consumed["stream"] == "queue:high"

        pending = await redis.xpending("queue:high", "workers")
        pending_count = pending["pending"] if isinstance(pending, dict) else pending[0]
        assert pending_count == 1

        acked = await connector.request(
            "POST",
            "/ack",
            stream=consumed["stream"],
            message_id=consumed["message_id"],
        )
        assert acked == {"acked": 1}

        pending = await redis.xpending("queue:high", "workers")
        pending_count = pending["pending"] if isinstance(pending, dict) else pending[0]
        assert pending_count == 0
    finally:
        await connector.close()
        await redis.aclose()


@pytest.mark.asyncio
async def test_message_queue_connector_routes_failed_message_to_retry_then_dlq() -> None:
    redis = FakeStreamRedis()
    service = ServiceDefinition(
        service_id="queue-service",
        name="Queue Service",
        connector_type=TransportType.MESSAGE_QUEUE,
        connector_config={
            "redis_url": "redis://unused",
            "streams": {"normal": "queue:normal"},
            "group": "workers",
            "max_retries": 1,
        },
    )
    connector = MessageQueueConnector(service, client=redis)

    try:
        await connector.request("POST", "/publish", json={"task": "index"})
        consumed = await connector.request("GET", "/consume", consumer="worker-a")
        await connector.request(
            "POST",
            "/fail",
            message_id=consumed["message_id"],
            stream=consumed["stream"],
            error="handler failed",
        )
        retry_entries = await redis.xrange("queue:retry:queue:normal")
        assert retry_entries
        pending = await redis.xpending("queue:normal", "workers")
        assert pending["pending"] == 0

        retried = await connector.request("GET", "/consume", consumer="worker-a")
        assert retried["stream"] == "queue:retry:queue:normal"
        assert retried["attempts"] == 1
        assert retried["original_stream"] == "queue:normal"

        await connector.request(
            "POST",
            "/fail",
            message_id=retried["message_id"],
            stream=retried["stream"],
            error="handler failed again",
        )
        dlq_entries = await redis.xrange("queue:dlq:queue:normal")
        assert dlq_entries
        _dlq_id, fields = dlq_entries[-1]
        assert json.loads(fields[b"payload"].decode())["task"] == "index"
    finally:
        await connector.close()
        await redis.aclose()


@pytest.mark.asyncio
async def test_message_queue_connector_empty_consume_is_non_blocking() -> None:
    redis = FakeStreamRedis()
    service = ServiceDefinition(
        service_id="queue-service",
        name="Queue Service",
        connector_type=TransportType.MESSAGE_QUEUE,
        connector_config={
            "redis_url": "redis://unused",
            "streams": {"normal": "queue:normal"},
            "group": "workers",
        },
    )
    connector = MessageQueueConnector(service, client=redis)

    try:
        consumed = await connector.request("GET", "/consume", consumer="worker-a")
        assert consumed is None
    finally:
        await connector.close()
        await redis.aclose()
