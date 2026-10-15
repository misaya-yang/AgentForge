"""Idempotency primitives for internal HTTP service calls."""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import time
from dataclasses import dataclass
from typing import Any, Protocol


@dataclass(frozen=True)
class CachedResponse:
    status_code: int
    headers: list[tuple[bytes, bytes]]
    body: bytes
    request_body_digest: str | None = None


class IdempotencyStore(Protocol):
    async def get_cached(self, key: str) -> CachedResponse | None:
        ...

    async def try_begin(self, key: str, ttl_seconds: int) -> bool:
        ...

    async def wait_for_cached(
        self,
        key: str,
        *,
        timeout_seconds: float,
        poll_seconds: float,
    ) -> CachedResponse | None:
        ...

    async def store_response(
        self,
        key: str,
        response: CachedResponse,
        ttl_seconds: int,
    ) -> None:
        ...

    async def abort(self, key: str) -> None:
        ...


class InMemoryIdempotencyStore:
    def __init__(self) -> None:
        self._records: dict[str, tuple[CachedResponse | None, float]] = {}
        self._conditions: dict[str, asyncio.Condition] = {}
        self._lock = asyncio.Lock()

    async def get_cached(self, key: str) -> CachedResponse | None:
        async with self._lock:
            self._evict_expired_locked()
            record = self._records.get(key)
            if record is None:
                return None
            response, _expires_at = record
            return response

    async def try_begin(self, key: str, ttl_seconds: int) -> bool:
        async with self._lock:
            self._evict_expired_locked()
            if key in self._records:
                return False
            self._records[key] = (None, time.monotonic() + max(ttl_seconds, 1))
            self._conditions.setdefault(key, asyncio.Condition())
            return True

    async def wait_for_cached(
        self,
        key: str,
        *,
        timeout_seconds: float,
        poll_seconds: float,
    ) -> CachedResponse | None:
        deadline = time.monotonic() + max(timeout_seconds, 0.0)
        while time.monotonic() < deadline:
            cached = await self.get_cached(key)
            if cached is not None:
                return cached
            condition = self._conditions.setdefault(key, asyncio.Condition())
            remaining = max(min(poll_seconds, deadline - time.monotonic()), 0.0)
            if remaining <= 0:
                break
            async with condition:
                with contextlib.suppress(asyncio.TimeoutError):
                    await asyncio.wait_for(condition.wait(), timeout=remaining)
        return await self.get_cached(key)

    async def store_response(
        self,
        key: str,
        response: CachedResponse,
        ttl_seconds: int,
    ) -> None:
        condition = self._conditions.setdefault(key, asyncio.Condition())
        async with self._lock:
            self._records[key] = (response, time.monotonic() + max(ttl_seconds, 1))
        async with condition:
            condition.notify_all()

    async def abort(self, key: str) -> None:
        condition = self._conditions.setdefault(key, asyncio.Condition())
        async with self._lock:
            self._records.pop(key, None)
        async with condition:
            condition.notify_all()

    def _evict_expired_locked(self) -> None:
        now = time.monotonic()
        for key, (_response, expires_at) in list(self._records.items()):
            if expires_at <= now:
                self._records.pop(key, None)
                self._conditions.pop(key, None)


class RedisIdempotencyStore:
    """Redis-backed idempotency store.

    Uses two keys per idempotency key:
    - ``<prefix>:lock:<key>`` protects the first request with SET NX EX.
    - ``<prefix>:response:<key>`` stores the serialized response.
    """

    def __init__(self, redis_client: Any, *, prefix: str = "ai-gateway:internal:idem") -> None:
        self._redis = redis_client
        self._prefix = prefix.rstrip(":")

    async def get_cached(self, key: str) -> CachedResponse | None:
        raw = await self._redis.get(self._response_key(key))
        if not raw:
            return None
        if isinstance(raw, bytes):
            raw = raw.decode("utf-8")
        data = json.loads(raw)
        return CachedResponse(
            status_code=int(data["status_code"]),
            headers=[
                (name.encode("latin-1"), value.encode("latin-1"))
                for name, value in data.get("headers", [])
            ],
            body=bytes.fromhex(data.get("body_hex", "")),
            request_body_digest=data.get("request_body_digest"),
        )

    async def try_begin(self, key: str, ttl_seconds: int) -> bool:
        result = await self._redis.set(
            self._lock_key(key),
            "1",
            nx=True,
            ex=max(int(ttl_seconds), 1),
        )
        return bool(result)

    async def wait_for_cached(
        self,
        key: str,
        *,
        timeout_seconds: float,
        poll_seconds: float,
    ) -> CachedResponse | None:
        deadline = time.monotonic() + max(timeout_seconds, 0.0)
        while time.monotonic() < deadline:
            cached = await self.get_cached(key)
            if cached is not None:
                return cached
            await asyncio.sleep(max(poll_seconds, 0.001))
        return await self.get_cached(key)

    async def store_response(
        self,
        key: str,
        response: CachedResponse,
        ttl_seconds: int,
    ) -> None:
        payload = json.dumps(
            {
                "status_code": response.status_code,
                "headers": [
                    (name.decode("latin-1"), value.decode("latin-1"))
                    for name, value in response.headers
                ],
                "body_hex": response.body.hex(),
                "request_body_digest": response.request_body_digest,
            },
            separators=(",", ":"),
        )
        await self._redis.set(self._response_key(key), payload, ex=max(int(ttl_seconds), 1))
        await self._redis.delete(self._lock_key(key))

    async def abort(self, key: str) -> None:
        await self._redis.delete(self._lock_key(key))

    def _lock_key(self, key: str) -> str:
        return f"{self._prefix}:lock:{key}"

    def _response_key(self, key: str) -> str:
        return f"{self._prefix}:response:{key}"


class IdempotencyMiddleware:
    def __init__(
        self,
        app,
        *,
        store: IdempotencyStore | None = None,
        ttl_seconds: int = 86_400,
        wait_timeout_seconds: float = 5.0,
        wait_poll_seconds: float = 0.05,
        max_body_bytes: int = 50 * 1024 * 1024,
        header_name: str = "idempotency-key",
    ) -> None:
        self.app = app
        self.store = store or InMemoryIdempotencyStore()
        self.ttl_seconds = max(int(ttl_seconds), 1)
        self.wait_timeout_seconds = max(float(wait_timeout_seconds), 0.0)
        self.wait_poll_seconds = max(float(wait_poll_seconds), 0.001)
        self.max_body_bytes = max(int(max_body_bytes), 0)
        self.header_name = header_name.lower()

    async def __call__(self, scope, receive, send) -> None:
        if scope.get("type") != "http":
            await self.app(scope, receive, send)
            return
        method = str(scope.get("method") or "GET").upper()
        if method in {"GET", "HEAD", "OPTIONS"}:
            await self.app(scope, receive, send)
            return
        idem_key = _header_value(scope.get("headers") or [], self.header_name)
        if not idem_key:
            await self.app(scope, receive, send)
            return

        try:
            body = await _read_body(receive, max_body_bytes=self.max_body_bytes)
        except _RequestBodyTooLarge:
            await _send_plain(send, 413, b"Idempotency request body is too large")
            return
        except _RequestDisconnected:
            # The peer is gone and no idempotency lease has been acquired yet.
            # Returning prevents a tight receive loop on repeated disconnects.
            return

        request_body_digest = hashlib.sha256(body).hexdigest()
        store_key = _store_key(scope, idem_key)

        cached = await self.store.get_cached(store_key)
        if cached is not None:
            await _send_cached_or_conflict(send, cached, request_body_digest)
            return

        owns_request = await self.store.try_begin(store_key, self.ttl_seconds)
        if not owns_request:
            cached = await self.store.wait_for_cached(
                store_key,
                timeout_seconds=self.wait_timeout_seconds,
                poll_seconds=self.wait_poll_seconds,
            )
            if cached is not None:
                await _send_cached_or_conflict(send, cached, request_body_digest)
                return
            await _send_plain(send, 409, b"Idempotency request still in progress")
            return

        replay = _ReplayReceive(body, receive)
        status_code = 500
        response_headers: list[tuple[bytes, bytes]] = []
        chunks: list[bytes] = []
        is_streaming = False

        async def wrapped_send(message):
            nonlocal status_code, response_headers, is_streaming
            if message["type"] == "http.response.start":
                status_code = int(message["status"])
                response_headers = list(message.get("headers") or [])
                content_type = _header_value(response_headers, "content-type") or ""
                is_streaming = content_type.lower().startswith("text/event-stream")
            elif message["type"] == "http.response.body" and not is_streaming:
                chunks.append(message.get("body", b""))
            await send(message)

        try:
            await self.app(scope, replay, wrapped_send)
        except asyncio.CancelledError:
            # CancelledError is a BaseException, so the generic Exception
            # branch below cannot release the owned key.
            await self.store.abort(store_key)
            raise
        except Exception:
            await self.store.abort(store_key)
            raise

        if is_streaming:
            await self.store.abort(store_key)
            return

        if status_code >= 500:
            await self.store.abort(store_key)
            return

        await self.store.store_response(
            store_key,
            CachedResponse(
                status_code=status_code,
                headers=response_headers,
                body=b"".join(chunks),
                request_body_digest=request_body_digest,
            ),
            self.ttl_seconds,
        )


class _ReplayReceive:
    def __init__(self, body: bytes, upstream_receive) -> None:
        self._body = body
        self._upstream_receive = upstream_receive
        self._sent = False

    async def __call__(self):
        if self._sent:
            return await self._upstream_receive()
        self._sent = True
        return {"type": "http.request", "body": self._body, "more_body": False}


class _RequestBodyTooLarge(Exception):
    """Raised when an idempotency-protected request exceeds its replay limit."""


class _RequestDisconnected(Exception):
    """Raised when the client disconnects before its request body is complete."""


async def _read_body(receive, *, max_body_bytes: int) -> bytes:
    chunks: list[bytes] = []
    total_bytes = 0
    while True:
        message = await receive()
        if message["type"] == "http.disconnect":
            raise _RequestDisconnected
        if message["type"] != "http.request":
            continue
        chunk = message.get("body", b"")
        total_bytes += len(chunk)
        if total_bytes > max_body_bytes:
            raise _RequestBodyTooLarge
        chunks.append(chunk)
        if not message.get("more_body", False):
            return b"".join(chunks)


async def _send_cached(send, response: CachedResponse) -> None:
    headers = [
        (name, value)
        for name, value in response.headers
        if name.lower() not in {b"content-length", b"transfer-encoding"}
    ]
    headers.append((b"x-idempotency-replayed", b"true"))
    await send(
        {
            "type": "http.response.start",
            "status": response.status_code,
            "headers": headers,
        }
    )
    await send(
        {
            "type": "http.response.body",
            "body": response.body,
            "more_body": False,
        }
    )


async def _send_cached_or_conflict(
    send,
    response: CachedResponse,
    request_body_digest: str,
) -> None:
    """Replay only an identical request body for an existing idempotency key."""

    if response.request_body_digest != request_body_digest:
        await _send_plain(
            send,
            409,
            b"Idempotency key was already used with a different request body",
        )
        return
    await _send_cached(send, response)


async def _send_plain(send, status_code: int, body: bytes) -> None:
    await send(
        {
            "type": "http.response.start",
            "status": status_code,
            "headers": [(b"content-type", b"text/plain; charset=utf-8")],
        }
    )
    await send({"type": "http.response.body", "body": body, "more_body": False})


def _header_value(headers: list[tuple[bytes, bytes]], name: str) -> str:
    wanted = name.lower().encode("latin-1")
    for key, value in headers:
        if key.lower() == wanted:
            return value.decode("latin-1")
    return ""


def _store_key(scope, idem_key: str) -> str:
    headers = list(scope.get("headers") or [])
    parts = [
        str(scope.get("method") or "GET").upper(),
        str(scope.get("path") or "/"),
        _header_value(headers, "x-tenant-id"),
        _header_value(headers, "x-user-id"),
        idem_key,
    ]
    return hashlib.sha256("\n".join(parts).encode("utf-8")).hexdigest()


__all__ = [
    "CachedResponse",
    "IdempotencyMiddleware",
    "IdempotencyStore",
    "InMemoryIdempotencyStore",
    "RedisIdempotencyStore",
]
