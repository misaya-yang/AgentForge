"""
Async HTTP transport built on ``httpx``.

Responsibilities:
- Auth header injection on every request
- Bounded GET/HEAD retry; mutations and streams are never automatically replayed
- Same-origin destinations and no authenticated redirect following
- Status-code-to-exception mapping
- SSE streaming via ``SSEParser``
"""

from __future__ import annotations

import asyncio
import logging
import math
from contextlib import asynccontextmanager
from typing import TYPE_CHECKING, Any

import httpx
from ai_assistant.exceptions import (
    AssistantError,
    StreamError,
    TimeoutError,  # noqa: A004 - intentional public SDK exception
    error_from_status,
)
from ai_assistant.streaming.sse_parser import SSEParser

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

    from ai_assistant.auth import AuthManager
    from ai_assistant.models.events import StreamEvent

logger = logging.getLogger("ai_assistant.transport")

# Transient status codes eligible for automatic retry
_RETRYABLE_STATUSES = frozenset({429, 502, 503, 504})


class HTTPTransport:
    """Low-level async HTTP client used by every SDK module.

    Do **not** instantiate directly — use ``AssistantClient`` which wires
    ``AuthManager`` and ``HTTPTransport`` together.
    """

    def __init__(self, auth: AuthManager) -> None:
        if type(auth.config.max_retries) is not int or auth.config.max_retries < 0:
            raise ValueError("max_retries must be a nonnegative integer")
        self._auth = auth
        self._sse_parser = SSEParser()

        self._client = httpx.AsyncClient(
            base_url=auth.base_url,
            timeout=httpx.Timeout(auth.config.timeout, connect=10.0),
            follow_redirects=False,
            limits=httpx.Limits(
                max_connections=100,
                max_keepalive_connections=20,
            ),
        )

    # -- public API ----------------------------------------------------------

    async def request(
        self,
        method: str,
        path: str,
        *,
        json: Any | None = None,
        params: dict[str, Any] | None = None,
        content: bytes | None = None,
        headers: dict[str, str] | None = None,
        max_response_bytes: int | None = None,
    ) -> httpx.Response:
        """Send once for mutations, with bounded retries for GET/HEAD only.

        Returns the ``httpx.Response`` on success (2xx).
        Raises the appropriate ``AssistantError`` subclass on failure.
        """
        path = self._validated_url(path)
        method = method.upper()
        if max_response_bytes is not None and (
            type(max_response_bytes) is not int or max_response_bytes <= 0
            or method not in {"GET", "HEAD"}
        ):
            raise ValueError("Response byte caps require GET/HEAD and a positive integer")
        merged_headers = self._auth.get_headers()
        if headers:
            protected = {"authorization", "x-api-key", "x-tenant-id", "x-user-id",
                         "host", "cookie", "proxy-authorization"}
            if any(name.lower() in protected for name in headers):
                raise ValueError("Request headers cannot override authentication or owner scope")
            merged_headers.update(headers)

        if max_response_bytes is not None:
            merged_headers = {key: value for key, value in merged_headers.items()
                              if key.lower() != "accept-encoding"}
            merged_headers["Accept-Encoding"] = "identity"

        max_attempts = self._auth.config.max_retries + 1 if method in {"GET", "HEAD"} else 1
        last_exc: Exception | None = None

        for attempt in range(1, max_attempts + 1):
            try:
                send = self._client.request if max_response_bytes is None else self._bounded_request
                options = {} if max_response_bytes is None else {
                    "max_response_bytes": max_response_bytes,
                }
                resp = await send(
                    method,
                    path,
                    json=json,
                    params=params,
                    content=content,
                    headers=merged_headers,
                    follow_redirects=False,
                    **options,
                )
            except httpx.TimeoutException as exc:
                last_exc = exc
                if attempt < max_attempts:
                    await self._backoff(attempt)
                    continue
                raise TimeoutError(
                    f"Request timed out after {self._auth.config.timeout}s",
                    request_id=merged_headers.get("X-Request-Id"),
                ) from exc
            except httpx.HTTPError as exc:
                last_exc = exc
                if attempt < max_attempts:
                    await self._backoff(attempt)
                    continue
                raise AssistantError(
                    "HTTP transport error",
                    request_id=merged_headers.get("X-Request-Id"),
                ) from exc

            # Success
            if resp.is_success:
                return resp

            # Retryable server error
            if resp.status_code in _RETRYABLE_STATUSES and attempt < max_attempts:
                retry_after = self._parse_retry_after(resp)
                wait = retry_after if retry_after else self._backoff_seconds(attempt)
                logger.info(
                    "Retryable %d on %s %s — waiting %.1fs (attempt %d/%d)",
                    resp.status_code,
                    method,
                    path,
                    wait,
                    attempt,
                    max_attempts,
                )
                await asyncio.sleep(wait)
                continue

            # Non-retryable error
            self._raise_for_status(resp, merged_headers.get("X-Request-Id"))

        # Exhausted retries — should not reach here, but be safe
        raise AssistantError(
            f"Request failed after {max_attempts} attempts",
            request_id=merged_headers.get("X-Request-Id"),
        ) from last_exc

    async def _bounded_request(
        self, method: str, path: str, *, max_response_bytes: int, **kwargs: Any,
    ) -> httpx.Response:
        """Bound encoded JSON reads before buffering; refuse compression expansion.

        HTTPX transports own their individual chunks. Already-buffered custom
        transports cannot have prior allocations undone; their content is checked
        before making another buffer. Ordinary network responses are streamed.
        """
        try:
            async with self._stream_response(method, path, **kwargs) as response:
                encoding = response.headers.get("content-encoding", "identity").strip().lower()
                if encoding not in {"", "identity"}:
                    raise AssistantError("Compressed bounded response is unsupported")
                length = response.headers.get("content-length")
                if length is not None:
                    if not length.isascii() or not length.isdecimal():
                        raise AssistantError("Invalid bounded response length")
                    if len(length) > 20 or int(length) > max_response_bytes:
                        raise AssistantError("Response exceeds byte limit")
                if response.is_stream_consumed:
                    body = response.content
                    if len(body) > max_response_bytes:
                        raise AssistantError("Response exceeds byte limit")
                else:
                    buffer = bytearray()
                    async for chunk in response.aiter_raw():
                        if len(buffer) + len(chunk) > max_response_bytes:
                            raise AssistantError("Response exceeds byte limit")
                        buffer.extend(chunk)
                    body = bytes(buffer)
                return httpx.Response(
                    response.status_code, headers=response.headers, content=body,
                    request=response.request,
                )
        except (AssistantError, httpx.HTTPError):
            raise
        except Exception:
            raise AssistantError("Bounded response read failed") from None

    async def stream_sse(
        self,
        path: str,
        body: dict[str, Any],
    ) -> AsyncIterator[StreamEvent]:
        """POST to an SSE endpoint and yield parsed ``StreamEvent`` objects.

        The underlying connection is kept open until the server sends
        ``[DONE]`` or closes the stream.
        """
        path = self._validated_url(path)
        headers = self._auth.get_headers()
        headers["Accept"] = "text/event-stream"

        try:
            async with self._stream_response(
                "POST",
                path,
                json=body,
                headers=headers,
                follow_redirects=False,
                timeout=httpx.Timeout(None, connect=10.0),  # no read timeout for SSE
            ) as response:
                if not response.is_success:
                    # Read the body so we can report the error message
                    await response.aread()
                    self._raise_for_status(response, headers.get("X-Request-Id"))

                async for event in self._sse_parser.parse(response):
                    yield event
                    if event.is_terminal():
                        return

        except AssistantError:
            raise
        except httpx.TimeoutException as exc:
            raise TimeoutError(
                "SSE connection timed out",
                request_id=headers.get("X-Request-Id"),
            ) from exc
        except Exception as exc:
            raise StreamError(
                "SSE stream error",
                request_id=headers.get("X-Request-Id"),
            ) from exc

    async def stream_sse_get(
        self,
        path: str,
        *,
        params: dict[str, Any] | None = None,
    ) -> AsyncIterator[StreamEvent]:
        """GET an append-only SSE cursor (used by the native V2 Items API)."""
        path = self._validated_url(path)
        headers = self._auth.get_headers()
        headers["Accept"] = "text/event-stream"
        try:
            async with self._stream_response(
                "GET", path, params=params, headers=headers,
                follow_redirects=False,
                timeout=httpx.Timeout(None, connect=10.0),
            ) as response:
                if not response.is_success:
                    await response.aread()
                    self._raise_for_status(response, headers.get("X-Request-Id"))
                async for event in self._sse_parser.parse(response):
                    yield event
        except AssistantError:
            raise
        except httpx.TimeoutException as exc:
            raise TimeoutError(
                "SSE connection timed out", request_id=headers.get("X-Request-Id")
            ) from exc
        except Exception as exc:
            raise StreamError(
                "SSE stream error", request_id=headers.get("X-Request-Id")
            ) from exc

    @asynccontextmanager
    async def _stream_response(self, *args: Any, **kwargs: Any) -> AsyncIterator[httpx.Response]:
        """Preserve a read failure/cancellation if HTTP response cleanup also fails."""
        body_error: BaseException | None = None
        try:
            async with self._client.stream(*args, **kwargs) as response:
                try:
                    yield response
                except BaseException as exc:
                    body_error = exc
                    raise
        except BaseException:
            if body_error is not None and not isinstance(body_error, GeneratorExit):
                raise body_error from body_error.__cause__
            raise

    async def close(self) -> None:
        """Shut down the underlying ``httpx.AsyncClient``."""
        await self._client.aclose()

    # -- internal helpers ----------------------------------------------------

    def _validated_url(self, path: str) -> str:
        """Refuse credential forwarding to URLs supplied by an untrusted response."""
        if not isinstance(path, str) or any(ord(char) < 32 for char in path) or "\\" in path:
            raise ValueError("Invalid Gateway request URL")
        try:
            target = httpx.URL(path)
            base = self._client.base_url
            resolved = base.join(target)
        except (httpx.InvalidURL, ValueError) as exc:
            raise ValueError("Invalid Gateway request URL") from exc
        if (
            resolved.scheme not in {"http", "https"}
            or (resolved.scheme, resolved.host, resolved.port)
            != (base.scheme, base.host, base.port)
            or resolved.userinfo or resolved.fragment or path.startswith("//")
        ):
            raise ValueError("Request URL must stay on the authenticated Gateway origin")
        return path

    @staticmethod
    def _raise_for_status(resp: httpx.Response, request_id: str | None) -> None:
        """Map an error HTTP response to the right SDK exception."""
        try:
            body = resp.json()
            message = body.get("error", body.get("message", body.get("detail", resp.text)))
        except Exception:
            message = resp.text or f"HTTP {resp.status_code}"

        retry_after = HTTPTransport._parse_retry_after(resp)
        raise error_from_status(
            resp.status_code,
            str(message),
            request_id=request_id,
            retry_after=retry_after,
        )

    @staticmethod
    def _parse_retry_after(resp: httpx.Response) -> float | None:
        raw = resp.headers.get("Retry-After")
        if raw is None:
            return None
        try:
            value = float(raw)
            return min(value, 30.0) if math.isfinite(value) and value >= 0.0 else None
        except ValueError:
            return None

    @staticmethod
    def _backoff_seconds(attempt: int) -> float:
        """Exponential back-off: 0.5s, 1s, 2s, 4s, ..."""
        return min(0.5 * (2 ** (attempt - 1)), 30.0)

    @staticmethod
    async def _backoff(attempt: int) -> None:
        await asyncio.sleep(HTTPTransport._backoff_seconds(attempt))
