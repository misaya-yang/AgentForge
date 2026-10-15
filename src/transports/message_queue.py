from __future__ import annotations

import json
from typing import Any

from ..models.service import ServiceDefinition
from .base import BaseConnector


class MessageQueueConnector(BaseConnector):
    def __init__(self, service: ServiceDefinition, *, client: Any | None = None):
        super().__init__(service)
        config = service.connector_config or {}
        self.redis_url = str(config.get("redis_url") or "redis://localhost:6379/0")
        self.streams = dict(config.get("streams") or {})
        self.group = str(config.get("group") or f"{service.service_id}:workers")
        self.max_retries = int(config.get("max_retries", 3))
        self.block_ms = max(int(config.get("block_ms", 0)), 0)
        self._client = client
        self._owns_client = client is None
        self._messages: dict[tuple[str, str], dict[str, Any]] = {}
        self._message_meta: dict[tuple[str, str], dict[str, Any]] = {}

    async def request(self, method: str, url: str, **kwargs: Any) -> Any:
        method_upper = method.upper()
        path = (url or "").split("?", 1)[0].rstrip("/") or "/"
        if method_upper == "POST" and path == "/publish":
            return await self.publish(
                kwargs.get("json") or kwargs.get("payload") or {},
                priority=str(kwargs.get("priority") or "normal"),
            )
        if method_upper == "GET" and path == "/consume":
            return await self.consume(
                priority=str(kwargs.get("priority") or "normal"),
                consumer=str(kwargs.get("consumer") or "worker-0"),
                count=int(kwargs.get("count") or 1),
                block_ms=int(kwargs.get("block_ms", self.block_ms)),
            )
        if method_upper == "POST" and path == "/ack":
            return await self.ack(
                stream=str(kwargs["stream"]),
                message_id=str(kwargs["message_id"]),
            )
        if method_upper == "POST" and path == "/fail":
            return await self.fail(
                stream=str(kwargs["stream"]),
                message_id=str(kwargs["message_id"]),
                error=str(kwargs.get("error") or "handler failed"),
            )
        raise ValueError(f"Unsupported message queue operation: {method} {url}")

    async def publish(self, payload: dict[str, Any], *, priority: str = "normal") -> dict[str, Any]:
        client = await self._get_client()
        stream = self._stream_for(priority)
        await self._ensure_group(client, stream)
        message_id = await client.xadd(
            stream,
            {"payload": json.dumps(payload, separators=(",", ":"))},
        )
        message_id_str = _to_str(message_id)
        self._messages[(stream, message_id_str)] = payload
        return {"stream": stream, "message_id": message_id_str}

    async def consume(
        self,
        *,
        priority: str = "normal",
        consumer: str = "worker-0",
        count: int = 1,
        block_ms: int | None = None,
    ) -> dict[str, Any] | None:
        client = await self._get_client()
        stream = self._stream_for(priority)
        retry_stream = self._retry_stream(stream)
        entries = None
        selected_stream = stream
        for candidate in (retry_stream, stream):
            await self._ensure_group(client, candidate)
            entries = await client.xreadgroup(
                groupname=self.group,
                consumername=consumer,
                streams={candidate: ">"},
                count=max(int(count), 1),
                block=max(int(block_ms if block_ms is not None else self.block_ms), 0) or None,
            )
            if entries:
                selected_stream = candidate
                break
        if not entries:
            return None
        _stream_name, messages = entries[0]
        message_id, fields = messages[0]
        message_id_str = _to_str(message_id)
        raw = fields.get(b"payload") or fields.get("payload")
        payload = json.loads(_to_str(raw))
        attempts_raw = fields.get(b"attempts") or fields.get("attempts") or "0"
        original_stream_raw = fields.get(b"original_stream") or fields.get("original_stream")
        original_stream = _to_str(original_stream_raw) if original_stream_raw else stream
        try:
            attempts = int(_to_str(attempts_raw))
        except ValueError:
            attempts = 0
        self._messages[(selected_stream, message_id_str)] = payload
        self._message_meta[(selected_stream, message_id_str)] = {
            "attempts": attempts,
            "original_stream": original_stream,
        }
        return {
            "stream": selected_stream,
            "message_id": message_id_str,
            "payload": payload,
            "attempts": attempts,
            "original_stream": original_stream,
        }

    async def ack(self, *, stream: str, message_id: str) -> dict[str, Any]:
        client = await self._get_client()
        count = await client.xack(stream, self.group, message_id)
        return {"acked": int(count)}

    async def fail(self, *, stream: str, message_id: str, error: str) -> dict[str, Any]:
        client = await self._get_client()
        key = (stream, message_id)
        payload = self._messages.get(key, {})
        meta = self._message_meta.get(key, {})
        try:
            attempts = int(meta.get("attempts", 0)) + 1
        except (TypeError, ValueError):
            attempts = 1
        original_stream = str(meta.get("original_stream") or stream)
        target = (
            self._retry_stream(original_stream)
            if attempts <= self.max_retries
            else self._dlq_stream(original_stream)
        )
        await client.xadd(
            target,
            {
                "payload": json.dumps(payload, separators=(",", ":")),
                "original_stream": original_stream,
                "original_id": message_id,
                "error": error,
                "attempts": str(attempts),
            },
        )
        await client.xack(stream, self.group, message_id)
        self._messages.pop(key, None)
        self._message_meta.pop(key, None)
        return {"stream": target, "attempts": attempts}

    async def close(self) -> None:
        if self._client is not None and self._owns_client:
            close = getattr(self._client, "aclose", None)
            if callable(close):
                await close()
        self._client = None

    async def _get_client(self):
        if self._client is not None:
            return self._client
        import redis.asyncio as aioredis

        self._client = aioredis.from_url(self.redis_url, decode_responses=False)
        self._owns_client = True
        return self._client

    async def _ensure_group(self, client: Any, stream: str) -> None:
        try:
            await client.xgroup_create(name=stream, groupname=self.group, id="0", mkstream=True)
        except Exception as exc:  # noqa: BLE001
            if "BUSYGROUP" not in str(exc):
                raise

    def _stream_for(self, priority: str) -> str:
        return str(self.streams.get(priority) or self.streams.get("normal") or "queue:normal")

    @staticmethod
    def _retry_stream(stream: str) -> str:
        return f"queue:retry:{stream}"

    @staticmethod
    def _dlq_stream(stream: str) -> str:
        return f"queue:dlq:{stream}"


def _to_str(value: Any) -> str:
    if isinstance(value, bytes):
        return value.decode("utf-8")
    return str(value)
