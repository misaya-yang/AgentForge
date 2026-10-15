from __future__ import annotations

import asyncio
from types import SimpleNamespace
from typing import Any

import httpx
import pytest
from ai_gateway_core.comm.retry import RetryBudget, RetryPolicy
from ai_gateway_core.proxy import ServiceProxy, ServiceProxyConfig
from fastapi import HTTPException
from starlette.requests import Request
from starlette.responses import Response, StreamingResponse

from src.api.v1 import _proxy_utils as proxy_utils
from src.core.auth.user_resolver import UserContext


class _CapturingProxy:
    def __init__(self, response: Response | None = None) -> None:
        self.calls: list[dict[str, Any]] = []
        self.response = response

    async def forward(
        self,
        request: Request,
        user_headers: dict[str, str],
        **kwargs: Any,
    ) -> Response:
        self.calls.append(
            {
                "request": request,
                "user_headers": user_headers,
                **kwargs,
            }
        )
        if self.response is not None:
            return self.response
        return Response(content=b'{"ok":true}', media_type="application/json")


class _BlockingProxy(_CapturingProxy):
    def __init__(self, *, blocked_calls: int) -> None:
        super().__init__()
        self.blocked_calls = blocked_calls
        self.all_started = asyncio.Event()
        self.release_all = asyncio.Event()

    async def forward(
        self,
        request: Request,
        user_headers: dict[str, str],
        **kwargs: Any,
    ) -> Response:
        response = await super().forward(request, user_headers, **kwargs)
        if len(self.calls) <= self.blocked_calls:
            if len(self.calls) == self.blocked_calls:
                self.all_started.set()
            await self.release_all.wait()
        return response


class _FailingProxy:
    async def forward(self, *_args: Any, **_kwargs: Any) -> Response:
        raise RuntimeError("upstream failed")


class _TrackingAdmission(proxy_utils._UploadAdmission):
    def __init__(self) -> None:
        super().__init__()
        self.acquire_calls = 0
        self.release_calls = 0

    def acquire(self, *, envelope_bytes: int) -> proxy_utils._UploadLease:
        lease = super().acquire(envelope_bytes=envelope_bytes)
        self.acquire_calls += 1
        return lease

    def release(self, lease: proxy_utils._UploadLease) -> None:
        self.release_calls += 1
        super().release(lease)


def _user() -> UserContext:
    return UserContext(
        user_id="user-1",
        tenant_id="tenant-1",
        tier="normal",
        roles=["user"],
        is_authenticated=True,
    )


def _request(
    chunks: list[bytes],
    *,
    method: str = "POST",
    headers: list[tuple[bytes, bytes]] | None = None,
) -> Request:
    if chunks:
        messages = [
            {
                "type": "http.request",
                "body": chunk,
                "more_body": index < len(chunks) - 1,
            }
            for index, chunk in enumerate(chunks)
        ]
    else:
        messages = [{"type": "http.request", "body": b"", "more_body": False}]

    async def receive() -> dict[str, Any]:
        if messages:
            return messages.pop(0)
        return {"type": "http.request", "body": b"", "more_body": False}

    scope = {
        "type": "http",
        "asgi": {"version": "3.0"},
        "http_version": "1.1",
        "method": method,
        "scheme": "http",
        "path": "/v1/knowledge/test",
        "raw_path": b"/v1/knowledge/test",
        "query_string": b"",
        "headers": headers or [],
        "client": ("127.0.0.1", 1234),
        "server": ("testserver", 80),
        "app": SimpleNamespace(state=SimpleNamespace(database=None)),
    }
    request = Request(scope, receive)

    async def forbidden_body() -> bytes:
        raise AssertionError("proxy envelope must consume request.stream(), not request.body()")

    request.body = forbidden_body  # type: ignore[method-assign]
    return request


def test_knowledge_proxy_signer_uses_the_platform_internal_token(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "i" * 32)
    monkeypatch.setenv("GATEWAY_ENCRYPTION_KEY", "e" * 32)
    monkeypatch.setenv("GATEWAY_KNOWLEDGE_SHARED_SECRET", "k" * 32)

    signer = proxy_utils._build_signer()

    assert signer is not None
    assert signer.secret == "i" * 32


def test_path_specific_defaults_are_narrow_and_exact(monkeypatch: pytest.MonkeyPatch) -> None:
    for name in ("KB_MAX_JSON_BODY_MB", "KB_MAX_FILE_SIZE_MB", "KB_MAX_BATCH_SIZE_MB"):
        monkeypatch.delenv(name, raising=False)

    assert proxy_utils._request_body_limit_bytes("datasets/ds-1/retrieve") == 4 * 1024 * 1024
    assert proxy_utils._request_body_limit_bytes("documents/upload") == (
        16 * 1024 * 1024 + proxy_utils._MULTIPART_OVERHEAD_BYTES
    )
    assert proxy_utils._request_body_limit_bytes("datasets/ds-1/documents/upload/") == (
        16 * 1024 * 1024 + proxy_utils._MULTIPART_OVERHEAD_BYTES
    )
    assert proxy_utils._request_body_limit_bytes("datasets/ds-1/documents/batch-upload") == (
        32 * 1024 * 1024 + proxy_utils._MULTIPART_OVERHEAD_BYTES
    )
    assert proxy_utils._request_body_limit_bytes("datasets/ds-1/documents/upload/extra") == (
        4 * 1024 * 1024
    )


def test_body_limit_environment_values_are_sanitized(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("KB_MAX_JSON_BODY_MB", "not-an-integer")
    monkeypatch.setenv("KB_MAX_FILE_SIZE_MB", "999999999")
    monkeypatch.setenv("KB_MAX_BATCH_SIZE_MB", "999999999")

    assert proxy_utils._request_body_limit_bytes("datasets/ds-1/retrieve") == 4 * 1024 * 1024
    assert proxy_utils._request_body_limit_bytes("datasets/ds-1/documents/upload") == (
        48 * 1024 * 1024 + proxy_utils._MULTIPART_OVERHEAD_BYTES
    )
    assert proxy_utils._request_body_limit_bytes("datasets/ds-1/documents/batch-upload") == (
        48 * 1024 * 1024 + proxy_utils._MULTIPART_OVERHEAD_BYTES
    )
    assert (
        proxy_utils._request_body_limit_bytes("datasets/ds-1/documents/batch-upload")
        < 50 * 1024 * 1024
    )


def test_zero_upload_environment_value_clamps_to_one_mib(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KB_MAX_FILE_SIZE_MB", "0")

    assert proxy_utils._request_body_limit_bytes("datasets/ds-1/documents/upload") == (
        1024 * 1024 + proxy_utils._MULTIPART_OVERHEAD_BYTES
    )


@pytest.mark.asyncio
async def test_absent_content_length_is_streamed_once_and_forwarded_explicitly(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    capturing_proxy = _CapturingProxy()
    monkeypatch.setattr(proxy_utils, "_proxy", capturing_proxy)
    request = _request([b'{"query":', b'"hello"}'])

    response = await proxy_utils.proxy_to_kb_service(
        request,
        _user(),
        path="datasets/ds-1/retrieve",
    )

    assert response.status_code == 200
    assert request.headers.get("content-length") is None
    assert len(capturing_proxy.calls) == 1
    assert capturing_proxy.calls[0]["body"] == b'{"query":"hello"}'


@pytest.mark.asyncio
async def test_exact_boundary_is_accepted(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("KB_MAX_JSON_BODY_MB", "1")
    capturing_proxy = _CapturingProxy()
    monkeypatch.setattr(proxy_utils, "_proxy", capturing_proxy)
    limit = proxy_utils._request_body_limit_bytes("datasets/ds-1/retrieve")
    request = _request([b"a" * (limit // 2), b"b" * (limit - limit // 2)])

    await proxy_utils.proxy_to_kb_service(
        request,
        _user(),
        path="datasets/ds-1/retrieve",
    )

    assert len(capturing_proxy.calls) == 1
    assert len(capturing_proxy.calls[0]["body"]) == limit


@pytest.mark.asyncio
async def test_forged_small_content_length_cannot_bypass_hard_cap(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KB_MAX_JSON_BODY_MB", "1")
    capturing_proxy = _CapturingProxy()
    trace_calls: list[dict[str, Any]] = []
    monkeypatch.setattr(proxy_utils, "_proxy", capturing_proxy)
    monkeypatch.setattr(
        proxy_utils,
        "_record_rag_proxy_trace",
        lambda *_args, **kwargs: trace_calls.append(kwargs),
    )
    limit = proxy_utils._request_body_limit_bytes("datasets/ds-1/retrieve")
    request = _request(
        [b"x" * (limit + 1)],
        headers=[(b"content-length", b"1")],
    )

    with pytest.raises(HTTPException) as exc_info:
        await proxy_utils.proxy_to_kb_service(
            request,
            _user(),
            path="datasets/ds-1/retrieve",
        )

    assert exc_info.value.status_code == 413
    assert capturing_proxy.calls == []
    assert trace_calls == []


@pytest.mark.asyncio
async def test_chunked_cap_plus_one_stops_before_proxy_sign_trace_or_retry(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KB_MAX_JSON_BODY_MB", "1")
    capturing_proxy = _CapturingProxy()
    trace_calls: list[dict[str, Any]] = []
    monkeypatch.setattr(proxy_utils, "_proxy", capturing_proxy)
    monkeypatch.setattr(
        proxy_utils,
        "_record_rag_proxy_trace",
        lambda *_args, **kwargs: trace_calls.append(kwargs),
    )
    limit = proxy_utils._request_body_limit_bytes("datasets/ds-1/retrieve")
    request = _request([b"x" * (limit - 1), b"y", b"z"])

    with pytest.raises(HTTPException) as exc_info:
        await proxy_utils.proxy_to_kb_service(
            request,
            _user(),
            path="datasets/ds-1/retrieve",
        )

    assert exc_info.value.status_code == 413
    assert request.headers.get("content-length") is None
    # Signing, breaker admission, and retry accounting all begin inside
    # ServiceProxy.forward; not entering it keeps every upstream side effect at zero.
    assert capturing_proxy.calls == []
    assert trace_calls == []


@pytest.mark.asyncio
async def test_upload_path_uses_file_envelope_not_generic_json_cap(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KB_MAX_JSON_BODY_MB", "1")
    monkeypatch.setenv("KB_MAX_FILE_SIZE_MB", "2")
    capturing_proxy = _CapturingProxy()
    monkeypatch.setattr(proxy_utils, "_proxy", capturing_proxy)
    generic_limit = proxy_utils._request_body_limit_bytes("datasets/ds-1/retrieve")
    upload_limit = proxy_utils._request_body_limit_bytes("datasets/ds-1/documents/upload")
    request = _request([b"x" * (generic_limit + 1)])

    await proxy_utils.proxy_to_kb_service(
        request,
        _user(),
        path="datasets/ds-1/documents/upload",
    )

    assert upload_limit > generic_limit + 1
    assert len(capturing_proxy.calls) == 1
    assert len(capturing_proxy.calls[0]["body"]) == generic_limit + 1


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "idempotency_key",
    [None, "upload-boundary-1"],
    ids=["without-idempotency-key", "with-idempotency-key"],
)
async def test_max_upload_envelope_is_accepted_with_or_without_idempotency_key(
    monkeypatch: pytest.MonkeyPatch,
    idempotency_key: str | None,
) -> None:
    monkeypatch.setenv("KB_MAX_FILE_SIZE_MB", "999999999")
    capturing_proxy = _CapturingProxy()
    monkeypatch.setattr(proxy_utils, "_proxy", capturing_proxy)
    monkeypatch.setattr(proxy_utils, "_upload_admission", proxy_utils._UploadAdmission())
    path = "datasets/ds-1/documents/upload"
    limit = proxy_utils._request_body_limit_bytes(path)
    headers = (
        [] if idempotency_key is None else [(b"idempotency-key", idempotency_key.encode("utf-8"))]
    )

    response = await proxy_utils.proxy_to_kb_service(
        _request([b"x" * limit], headers=headers),
        _user(),
        path=path,
    )

    assert limit == 48 * 1024 * 1024 + proxy_utils._MULTIPART_OVERHEAD_BYTES
    assert limit < 50 * 1024 * 1024
    assert response.status_code == 200
    assert len(capturing_proxy.calls) == 1
    assert len(capturing_proxy.calls[0]["body"]) == limit


@pytest.mark.asyncio
async def test_max_upload_envelope_cap_plus_one_is_rejected_before_upstream(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KB_MAX_FILE_SIZE_MB", "999999999")
    capturing_proxy = _CapturingProxy()
    admission = _TrackingAdmission()
    monkeypatch.setattr(proxy_utils, "_proxy", capturing_proxy)
    monkeypatch.setattr(proxy_utils, "_upload_admission", admission)
    path = "datasets/ds-1/documents/upload"
    limit = proxy_utils._request_body_limit_bytes(path)

    with pytest.raises(HTTPException) as exc_info:
        await proxy_utils.proxy_to_kb_service(
            _request([b"x" * limit, b"y"]),
            _user(),
            path=path,
        )

    assert exc_info.value.status_code == 413
    assert capturing_proxy.calls == []
    assert (admission.acquire_calls, admission.release_calls) == (1, 1)
    assert (admission._active, admission._reserved_bytes) == (0, 0)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "idempotency_key",
    [
        pytest.param(None, id="missing-key"),
        pytest.param("", id="empty-key"),
        pytest.param("   ", id="whitespace-key"),
        pytest.param("upload-operation-1", id="valid-key"),
    ],
)
async def test_accepted_upload_retries_only_with_nonempty_idempotency_key(
    monkeypatch: pytest.MonkeyPatch,
    idempotency_key: str | None,
) -> None:
    attempts: list[bytes] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        attempts.append(await request.aread())
        if len(attempts) == 1:
            raise httpx.RemoteProtocolError(
                "temporary upstream disconnect",
                request=request,
            )
        return httpx.Response(200, json={"ok": True})

    upstream_client = httpx.AsyncClient(
        transport=httpx.MockTransport(handler),
        base_url="http://knowledge-service.test",
    )
    service_proxy = ServiceProxy(
        ServiceProxyConfig(
            name="KB Service test",
            base_url="http://knowledge-service.test",
            retry_policy=RetryPolicy(
                max_attempts=2,
                base_delay_ms=0,
                max_delay_ms=0,
                jitter=False,
            ),
            retry_budget=RetryBudget(budget_ratio=1.0),
        )
    )

    async def get_client() -> httpx.AsyncClient:
        return upstream_client

    async def reset_client() -> None:
        return None

    service_proxy._get_client = get_client  # type: ignore[method-assign]
    service_proxy._reset_client = reset_client  # type: ignore[method-assign]
    monkeypatch.setattr(proxy_utils, "_proxy", service_proxy)
    monkeypatch.setattr(proxy_utils, "_upload_admission", proxy_utils._UploadAdmission())
    headers = (
        [] if idempotency_key is None else [(b"idempotency-key", idempotency_key.encode("utf-8"))]
    )

    try:
        # A client-supplied key is not proof that Knowledge deduplicates uploads.
        with pytest.raises(HTTPException) as exc_info:
            await proxy_utils.proxy_to_kb_service(
                _request([b"upload-body"], headers=headers),
                _user(),
                path="datasets/ds-1/documents/upload",
            )
        assert exc_info.value.status_code == 502
    finally:
        await upstream_client.aclose()

    assert len(attempts) == 1
    assert attempts == [b"upload-body"] * len(attempts)


@pytest.mark.asyncio
async def test_upload_admission_rejects_third_concurrent_request_and_releases_slots(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    for name in ("KB_MAX_FILE_SIZE_MB", "KB_MAX_BATCH_SIZE_MB"):
        monkeypatch.delenv(name, raising=False)
    admission = proxy_utils._UploadAdmission()
    blocking_proxy = _BlockingProxy(blocked_calls=2)
    monkeypatch.setattr(proxy_utils, "_upload_admission", admission)
    monkeypatch.setattr(proxy_utils, "_proxy", blocking_proxy)
    path = "datasets/ds-1/documents/upload"

    first = asyncio.create_task(
        proxy_utils.proxy_to_kb_service(_request([b"first"]), _user(), path=path)
    )
    second = asyncio.create_task(
        proxy_utils.proxy_to_kb_service(_request([b"second"]), _user(), path=path)
    )
    await asyncio.wait_for(blocking_proxy.all_started.wait(), timeout=1)

    third_request = _request([b"third"])

    async def forbidden_stream():  # type: ignore[no-untyped-def]
        raise AssertionError("capacity rejection must happen before request body read")
        yield b""  # pragma: no cover

    third_request.stream = forbidden_stream  # type: ignore[method-assign]

    with pytest.raises(HTTPException) as exc_info:
        await proxy_utils.proxy_to_kb_service(third_request, _user(), path=path)

    assert exc_info.value.status_code == 503
    assert exc_info.value.headers == {"Retry-After": "1"}
    assert len(blocking_proxy.calls) == 2
    assert admission._active == 2
    assert 0 < admission._reserved_bytes <= proxy_utils._UPLOAD_INFLIGHT_BUDGET_BYTES

    blocking_proxy.release_all.set()
    responses = await asyncio.gather(first, second)
    assert [response.status_code for response in responses] == [200, 200]
    assert admission._active == 0
    assert admission._reserved_bytes == 0

    response = await proxy_utils.proxy_to_kb_service(
        _request([b"after-release"]),
        _user(),
        path=path,
    )
    assert response.status_code == 200
    assert len(blocking_proxy.calls) == 3


def test_upload_admission_rejects_duplicate_release_without_underflow() -> None:
    admission = proxy_utils._UploadAdmission()
    lease = admission.acquire(envelope_bytes=1024)

    admission.release(lease)
    with pytest.raises(RuntimeError, match="released more than once"):
        admission.release(lease)

    assert admission._active == 0
    assert admission._reserved_bytes == 0


@pytest.mark.asyncio
async def test_upload_lease_releases_exactly_once_after_body_413(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KB_MAX_FILE_SIZE_MB", "1")
    admission = _TrackingAdmission()
    capturing_proxy = _CapturingProxy()
    monkeypatch.setattr(proxy_utils, "_upload_admission", admission)
    monkeypatch.setattr(proxy_utils, "_proxy", capturing_proxy)
    path = "datasets/ds-1/documents/upload"
    limit = proxy_utils._request_body_limit_bytes(path)

    with pytest.raises(HTTPException) as exc_info:
        await proxy_utils.proxy_to_kb_service(
            _request([b"x" * (limit + 1)]),
            _user(),
            path=path,
        )

    assert exc_info.value.status_code == 413
    assert capturing_proxy.calls == []
    assert (admission.acquire_calls, admission.release_calls) == (1, 1)
    assert (admission._active, admission._reserved_bytes) == (0, 0)


@pytest.mark.asyncio
async def test_upload_lease_releases_exactly_once_after_forward_exception(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    admission = _TrackingAdmission()
    monkeypatch.setattr(proxy_utils, "_upload_admission", admission)
    monkeypatch.setattr(proxy_utils, "_proxy", _FailingProxy())

    with pytest.raises(RuntimeError, match="upstream failed"):
        await proxy_utils.proxy_to_kb_service(
            _request([b"upload"]),
            _user(),
            path="datasets/ds-1/documents/upload",
        )

    assert (admission.acquire_calls, admission.release_calls) == (1, 1)
    assert (admission._active, admission._reserved_bytes) == (0, 0)


@pytest.mark.asyncio
async def test_upload_lease_releases_exactly_once_after_normal_response(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    admission = _TrackingAdmission()
    monkeypatch.setattr(proxy_utils, "_upload_admission", admission)
    monkeypatch.setattr(proxy_utils, "_proxy", _CapturingProxy())

    response = await proxy_utils.proxy_to_kb_service(
        _request([b"upload"]),
        _user(),
        path="datasets/ds-1/documents/upload",
    )

    assert response.status_code == 200
    assert (admission.acquire_calls, admission.release_calls) == (1, 1)
    assert (admission._active, admission._reserved_bytes) == (0, 0)


@pytest.mark.asyncio
async def test_upload_lease_releases_before_streaming_response_is_consumed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    pulled: list[bytes] = []

    async def upstream_body():  # type: ignore[no-untyped-def]
        pulled.append(b"chunk")
        yield b"chunk"

    upstream_response = StreamingResponse(upstream_body())
    original_iterator = upstream_response.body_iterator
    admission = _TrackingAdmission()
    monkeypatch.setattr(proxy_utils, "_upload_admission", admission)
    monkeypatch.setattr(proxy_utils, "_proxy", _CapturingProxy(upstream_response))

    response = await proxy_utils.proxy_to_kb_service(
        _request([b"upload"]),
        _user(),
        path="datasets/ds-1/documents/upload",
    )

    assert response is upstream_response
    assert response.body_iterator is original_iterator
    assert pulled == []
    assert (admission.acquire_calls, admission.release_calls) == (1, 1)
    assert (admission._active, admission._reserved_bytes) == (0, 0)

    assert [chunk async for chunk in response.body_iterator] == [b"chunk"]
    assert admission.release_calls == 1


def test_batch_upload_admission_reserves_worst_case_under_128_mib(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("KB_MAX_BATCH_SIZE_MB", raising=False)
    admission = proxy_utils._UploadAdmission()
    envelope = proxy_utils._request_body_limit_bytes("datasets/ds-1/documents/batch-upload")

    lease = admission.acquire(envelope_bytes=envelope)
    assert admission._reserved_bytes == (
        envelope * proxy_utils._UPLOAD_MEMORY_RESERVATION_MULTIPLIER
    )
    assert admission._reserved_bytes <= proxy_utils._UPLOAD_INFLIGHT_BUDGET_BYTES

    with pytest.raises(HTTPException) as exc_info:
        admission.acquire(envelope_bytes=envelope)

    assert exc_info.value.status_code == 503
    admission.release(lease)
    assert admission._active == 0
    assert admission._reserved_bytes == 0


@pytest.mark.asyncio
async def test_non_retrieve_stream_is_returned_untouched_and_not_eagerly_consumed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    pulled: list[bytes] = []
    chunks = [b"a" * (128 * 1024), b"b" * (256 * 1024)]

    async def upstream_body():  # type: ignore[no-untyped-def]
        for chunk in chunks:
            pulled.append(chunk)
            yield chunk

    upstream_response = StreamingResponse(upstream_body())
    original_iterator = upstream_response.body_iterator
    trace_calls: list[dict[str, Any]] = []
    monkeypatch.setattr(proxy_utils, "_proxy", _CapturingProxy(upstream_response))
    monkeypatch.setattr(
        proxy_utils,
        "_record_rag_proxy_trace",
        lambda *_args, **kwargs: trace_calls.append(kwargs),
    )

    response = await proxy_utils.proxy_to_kb_service(
        _request([], method="GET"),
        _user(),
        path="datasets/ds-1/documents",
    )

    assert response is upstream_response
    assert response.body_iterator is original_iterator
    assert pulled == []
    assert trace_calls == []

    observed = [chunk async for chunk in response.body_iterator]
    assert b"".join(observed) == b"".join(chunks)
    assert trace_calls == []


@pytest.mark.asyncio
async def test_complete_retrieve_stream_records_bounded_trace_projection(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    pulled: list[bytes] = []
    chunks = [b"a" * (128 * 1024), b"b" * (100 * 1024)]

    async def upstream_body():  # type: ignore[no-untyped-def]
        for chunk in chunks:
            pulled.append(chunk)
            yield chunk

    upstream_response = StreamingResponse(upstream_body())
    original_iterator = upstream_response.body_iterator
    trace_calls: list[dict[str, Any]] = []
    monkeypatch.setattr(proxy_utils, "_proxy", _CapturingProxy(upstream_response))
    monkeypatch.setattr(
        proxy_utils,
        "_record_rag_proxy_trace",
        lambda *_args, **kwargs: trace_calls.append(kwargs),
    )

    response = await proxy_utils.proxy_to_kb_service(
        _request([b'{"query":"hello"}']),
        _user(),
        path="datasets/ds-1/retrieve",
    )

    assert response is upstream_response
    assert response.body_iterator is not original_iterator
    assert pulled == []
    assert trace_calls == []

    observed = [chunk async for chunk in response.body_iterator]
    full_body = b"".join(chunks)
    assert b"".join(observed) == full_body
    assert len(trace_calls) == 1
    assert trace_calls[0]["response_body"] == full_body


@pytest.mark.asyncio
async def test_oversize_valid_json_stream_passes_all_bytes_but_skips_success_trace(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    full_body = (
        b'{"results":[{"text":"' + b"x" * proxy_utils._RAG_TRACE_RESPONSE_CAPTURE_BYTES + b'"}]}'
    )

    async def upstream_body():  # type: ignore[no-untyped-def]
        yield full_body[: 128 * 1024]
        yield full_body[128 * 1024 :]

    upstream_response = StreamingResponse(upstream_body())
    trace_calls: list[dict[str, Any]] = []
    monkeypatch.setattr(proxy_utils, "_proxy", _CapturingProxy(upstream_response))
    monkeypatch.setattr(
        proxy_utils,
        "_record_rag_proxy_trace",
        lambda *_args, **kwargs: trace_calls.append(kwargs),
    )

    response = await proxy_utils.proxy_to_kb_service(
        _request([b'{"query":"hello"}']),
        _user(),
        path="datasets/ds-1/retrieve",
    )

    observed = [chunk async for chunk in response.body_iterator]
    assert b"".join(observed) == full_body
    assert trace_calls == []


@pytest.mark.asyncio
async def test_retrieve_stream_exception_propagates_and_skips_success_trace(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def upstream_body():  # type: ignore[no-untyped-def]
        yield b"partial"
        raise RuntimeError("upstream stream broke")

    upstream_response = StreamingResponse(upstream_body())
    trace_calls: list[dict[str, Any]] = []
    monkeypatch.setattr(proxy_utils, "_proxy", _CapturingProxy(upstream_response))
    monkeypatch.setattr(
        proxy_utils,
        "_record_rag_proxy_trace",
        lambda *_args, **kwargs: trace_calls.append(kwargs),
    )
    response = await proxy_utils.proxy_to_kb_service(
        _request([b'{"query":"hello"}']),
        _user(),
        path="datasets/ds-1/retrieve",
    )

    observed: list[bytes] = []
    with pytest.raises(RuntimeError, match="upstream stream broke"):
        async for chunk in response.body_iterator:
            observed.append(chunk)

    assert observed == [b"partial"]
    assert trace_calls == []


@pytest.mark.asyncio
async def test_oversize_plain_retrieve_response_skips_truncated_success_trace(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    full_body = b"x" * (proxy_utils._RAG_TRACE_RESPONSE_CAPTURE_BYTES + 17)
    upstream_response = Response(content=full_body)
    trace_calls: list[dict[str, Any]] = []
    monkeypatch.setattr(proxy_utils, "_proxy", _CapturingProxy(upstream_response))
    monkeypatch.setattr(
        proxy_utils,
        "_record_rag_proxy_trace",
        lambda *_args, **kwargs: trace_calls.append(kwargs),
    )

    response = await proxy_utils.proxy_to_kb_service(
        _request([b'{"query":"hello"}']),
        _user(),
        path="datasets/ds-1/retrieve",
    )

    assert response is upstream_response
    assert response.body == full_body
    assert trace_calls == []


@pytest.mark.asyncio
async def test_complete_plain_retrieve_response_records_full_bounded_trace(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    full_body = b'{"results":[]}'
    upstream_response = Response(content=full_body)
    trace_calls: list[dict[str, Any]] = []
    monkeypatch.setattr(proxy_utils, "_proxy", _CapturingProxy(upstream_response))
    monkeypatch.setattr(
        proxy_utils,
        "_record_rag_proxy_trace",
        lambda *_args, **kwargs: trace_calls.append(kwargs),
    )

    response = await proxy_utils.proxy_to_kb_service(
        _request([b'{"query":"hello"}']),
        _user(),
        path="datasets/ds-1/retrieve",
    )

    assert response is upstream_response
    assert response.body == full_body
    assert len(trace_calls) == 1
    assert trace_calls[0]["response_body"] == full_body
