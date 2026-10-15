"""Gateway → knowledge-service streaming proxy.

Thin glue around ``ai_gateway_core.proxy.ServiceProxy``. Breaker, SSE
pass-through, header strip/inject, and internal signing are defined once in
the core module.

Public entry point: ``proxy_to_kb_service(request, user, ...)``.
"""
from __future__ import annotations

import io
import os
import threading
import time
from collections.abc import AsyncIterator, Mapping
from dataclasses import dataclass
from typing import Final

from ai_gateway_core.auth.gateway_secret import GatewaySecret
from ai_gateway_core.proxy import ServiceProxy, ServiceProxyConfig
from fastapi import HTTPException, Request
from starlette.responses import Response, StreamingResponse

from ...core.auth.user_resolver import UserContext
from ...services.eval.rag_trace_capture import is_retrieve_path, record_rag_retrieval_trace
from ...services.knowledge_deletion_confirmation import prepare_knowledge_body

KB_SERVICE_URL: Final[str] = os.getenv(
    "KB_SERVICE_URL", "http://knowledge-service:8092"
)
_INJECTED_IDENTITY_HEADERS: Final = frozenset(
    {
        "x-user-id",
        "x-tenant-id",
        "x-user-tier",
        "x-user-type",
        "x-user-roles",
        "x-user-email",
        "x-user-name",
    }
)

_MIB: Final = 1024 * 1024
_DEFAULT_JSON_BODY_MB: Final = 4
_DEFAULT_FILE_BODY_MB: Final = 16
_DEFAULT_BATCH_BODY_MB: Final = 32
_MAX_JSON_BODY_MB: Final = 16
# The request is replayable bytes for signing and one transport retry. Keep the
# configured file bytes plus multipart allowance below knowledge-service's
# 50 MiB IdempotencyMiddleware body limit, while a conservative two-copy memory
# reservation remains inside the process admission budget.
_MAX_FILE_BODY_MB: Final = 48
_MAX_BATCH_BODY_MB: Final = 48
# Multipart boundaries, per-part headers, filenames, and the processing mode
# are outside the file-byte limits enforced by knowledge-service. Keep that
# allowance fixed and small so it cannot become a second upload budget.
_MULTIPART_OVERHEAD_BYTES: Final = 256 * 1024
_BODY_METHODS: Final = frozenset({"POST", "PUT", "PATCH", "DELETE"})
_UPLOAD_MAX_CONCURRENCY: Final = 2
_UPLOAD_INFLIGHT_BUDGET_BYTES: Final = 128 * _MIB
_UPLOAD_MEMORY_RESERVATION_MULTIPLIER: Final = 2
_RAG_TRACE_RESPONSE_CAPTURE_BYTES: Final = 256 * 1024


@dataclass
class _UploadLease:
    reserved_bytes: int
    released: bool = False


class _UploadAdmission:
    """Process-local, fail-fast memory admission for replayable upload bodies."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._active = 0
        self._reserved_bytes = 0

    def acquire(self, *, envelope_bytes: int) -> _UploadLease:
        reserved_bytes = envelope_bytes * _UPLOAD_MEMORY_RESERVATION_MULTIPLIER
        with self._lock:
            if (
                self._active >= _UPLOAD_MAX_CONCURRENCY
                or self._reserved_bytes + reserved_bytes > _UPLOAD_INFLIGHT_BUDGET_BYTES
            ):
                raise HTTPException(
                    status_code=503,
                    detail="KB upload capacity is temporarily unavailable",
                    headers={"Retry-After": "1"},
                )
            self._active += 1
            self._reserved_bytes += reserved_bytes
        return _UploadLease(reserved_bytes=reserved_bytes)

    def release(self, lease: _UploadLease) -> None:
        with self._lock:
            if lease.released or self._active <= 0 or self._reserved_bytes < lease.reserved_bytes:
                raise RuntimeError("KB upload admission lease released more than once")
            lease.released = True
            self._active -= 1
            self._reserved_bytes -= lease.reserved_bytes


_upload_admission = _UploadAdmission()


def _bounded_env_mb(name: str, default: int, *, maximum: int) -> int:
    raw = os.getenv(name, "").strip()
    if not raw:
        return default
    try:
        value = int(raw)
    except ValueError:
        return default
    return max(1, min(value, maximum))


def _request_body_kind(path: str) -> str:
    parts = tuple(part for part in path.strip("/").split("/") if part)
    if parts[-2:] == ("documents", "batch-upload"):
        return "batch-upload"
    if parts[-2:] == ("documents", "upload"):
        return "upload"
    return "json"


def _request_body_limit_bytes(path: str) -> int:
    """Return the hard request envelope for one public KB proxy path."""

    body_kind = _request_body_kind(path)
    if body_kind == "batch-upload":
        body_mb = _bounded_env_mb(
            "KB_MAX_BATCH_SIZE_MB",
            _DEFAULT_BATCH_BODY_MB,
            maximum=_MAX_BATCH_BODY_MB,
        )
        return body_mb * _MIB + _MULTIPART_OVERHEAD_BYTES
    if body_kind == "upload":
        body_mb = _bounded_env_mb(
            "KB_MAX_FILE_SIZE_MB",
            _DEFAULT_FILE_BODY_MB,
            maximum=_MAX_FILE_BODY_MB,
        )
        return body_mb * _MIB + _MULTIPART_OVERHEAD_BYTES
    return (
        _bounded_env_mb(
            "KB_MAX_JSON_BODY_MB",
            _DEFAULT_JSON_BODY_MB,
            maximum=_MAX_JSON_BODY_MB,
        )
        * _MIB
    )


async def _read_bounded_body(request: Request, *, limit_bytes: int) -> bytes:
    """Consume the ASGI body once, without trusting ``Content-Length``."""

    # CPython BytesIO.getvalue() shares its internal immutable buffer until a
    # subsequent write, avoiding bytearray -> bytes' final full-size copy.
    body = io.BytesIO()
    size = 0
    async for chunk in request.stream():
        if not chunk:
            continue
        size += len(chunk)
        if size > limit_bytes:
            raise HTTPException(status_code=413, detail="Request body too large")
        body.write(chunk)
    return body.getvalue()


_KB_READ_SCOPE: Final = "knowledge:read"
_KB_WRITE_SCOPE: Final = "knowledge:write"
_KB_READ_METHODS: Final = frozenset({"GET", "HEAD", "OPTIONS"})
# Retrieval verbs are POST on the wire but read-only against KB state; a
# knowledge:read key must be able to search. Derived from the KS route
# surface (POST /knowledge/{dataset_id}/retrieve, /retrieve_batch).
_KB_READ_ACTIONS: Final = frozenset({"retrieve", "retrieve_batch"})


def _normalize_key_scopes(raw: object) -> list[str]:
    if raw is None or raw == "":
        return []
    if isinstance(raw, str):
        return [part.strip() for part in raw.split(",") if part.strip()]
    if isinstance(raw, (list, tuple, set)):
        return [str(item).strip() for item in raw if item is not None and str(item).strip()]
    return []


def _api_key_declared_scopes(request: Request) -> frozenset[str] | None:
    """Return scopes for a recognized DB API key, or ``None`` for other callers.

    The schema has exposed key scopes through both ``scopes`` and
    ``permissions``.  An empty set is therefore meaningful: the request was
    authenticated with a DB key, but that key grants no knowledge access.
    """
    state = getattr(request, "state", None)
    key_info = getattr(state, "api_key_info", None)
    if not isinstance(key_info, Mapping):
        return None
    scopes: list[str] = []
    for column in ("scopes", "permissions"):
        scopes.extend(_normalize_key_scopes(key_info.get(column)))
    return frozenset(scopes)


def _required_knowledge_scope(request: Request, *, path: str) -> str:
    """Classify one proxy request as read-only or state-changing."""
    method = str(getattr(request, "method", "GET") or "GET").upper()
    if method in _KB_READ_METHODS:
        return _KB_READ_SCOPE

    tail = path.rstrip("/").rsplit("/", 1)[-1].lower()
    if method == "POST" and tail in _KB_READ_ACTIONS:
        return _KB_READ_SCOPE
    return _KB_WRITE_SCOPE


def enforce_knowledge_scope(request: Request, *, path: str) -> None:
    """Gate the KB proxy surface on API-key scopes (PRD T8.1).

    DB-issued API keys must hold ``knowledge:read`` for read traffic and
    ``knowledge:write`` for everything else, so a read-scope key cannot write.
    Recognized keys with no scopes fail closed and must be reissued or updated.
    Callers outside the DB key model (including JWT sessions and static env
    keys) are not scope-gated here; dataset-level ACL remains enforced by
    knowledge-service on every proxied request.
    """
    scopes = _api_key_declared_scopes(request)
    if scopes is None:
        return
    required = _required_knowledge_scope(request, path=path)
    if required not in scopes:
        raise HTTPException(
            status_code=403,
            detail={
                "message": (
                    f"API key is missing required scope '{required}' "
                    "for this knowledge-base operation; reissue or update "
                    "keys that have no assigned scopes"
                ),
                "required_scope": required,
                "key_scopes": sorted(scopes),
            },
        )


def _build_signer() -> GatewaySecret | None:
    secret = os.getenv("AI_PLATFORM_INTERNAL_TOKEN", "").strip()
    if not secret:
        return None
    return GatewaySecret(
        secret=secret,
        caller_service="gateway",
        audience="knowledge-service",
        allowed_path_prefixes=("/api/v1",),
    )


_signer = _build_signer()


def _sign_request(
    request: Request,
    *,
    upstream_path: str = "",
    body: bytes | None = None,
    identity_headers: Mapping[str, str] | None = None,
) -> tuple[str, str] | None:
    if _signer is None:
        return None
    return (
        _signer.header_name,
        _signer.sign(
            method=request.method,
            path=upstream_path or request.url.path,
            query=request.url.query,
            body=body,
            identity_headers=identity_headers,
        ),
    )


_proxy = ServiceProxy(
    ServiceProxyConfig(
        name="KB Service",
        base_url=KB_SERVICE_URL,
    ),
    signer=_sign_request,
)


def _record_rag_proxy_trace(
    request: Request,
    user: UserContext,
    *,
    path: str,
    body: bytes,
    response_status: int,
    response_body: bytes,
    started_at: float,
) -> None:
    if not is_retrieve_path(path):
        return
    database = getattr(getattr(request, "app", None), "state", None)
    database = getattr(database, "database", None)
    if database is None:
        return
    request_state = getattr(request, "state", None)
    record_rag_retrieval_trace(
        database,
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        request_id=str(getattr(request_state, "request_id", "") or ""),
        path=path,
        body=body,
        response_status=response_status,
        response_body=response_body,
        started_at=started_at,
        traceparent=getattr(request_state, "traceparent", None)
        or request.headers.get("traceparent"),
    )


async def _capture_retrieve_stream(
    body_iterator: AsyncIterator[bytes | str],
    request: Request,
    user: UserContext,
    *,
    path: str,
    request_body: bytes,
    response_status: int,
    started_at: float,
) -> AsyncIterator[bytes | str]:
    """Pass through a retrieve stream while retaining only a bounded trace prefix."""

    captured = bytearray()
    completed = False
    capture_truncated = False
    try:
        async for chunk in body_iterator:
            if chunk:
                encoded = chunk.encode("utf-8") if isinstance(chunk, str) else chunk
                remaining = _RAG_TRACE_RESPONSE_CAPTURE_BYTES - len(captured)
                if len(encoded) > remaining:
                    capture_truncated = True
                if remaining > 0:
                    captured.extend(encoded[:remaining])
            yield chunk
        completed = True
    finally:
        # The trace schema has no stream-incomplete/capture-truncated state.
        # Skipping those samples prevents a partial JSON prefix from becoming
        # a false successful zero-document retrieval in the eval denominator.
        if completed and not capture_truncated:
            _record_rag_proxy_trace(
                request,
                user,
                path=path,
                body=request_body,
                response_status=response_status,
                response_body=bytes(captured),
                started_at=started_at,
            )


async def proxy_to_kb_service(
    request: Request,
    user: UserContext,
    *,
    path: str = "",
    upstream_prefix: str = "/api/v1/knowledge",
) -> Response:
    """Forward a request to knowledge-service, streaming both directions."""
    body: bytes | None = None
    upload_lease: _UploadLease | None = None
    if request.method.upper() in _BODY_METHODS:
        limit_bytes = _request_body_limit_bytes(path)
        if _request_body_kind(path) != "json":
            upload_lease = _upload_admission.acquire(envelope_bytes=limit_bytes)
    try:
        if request.method.upper() in _BODY_METHODS:
            body = await _read_bounded_body(request, limit_bytes=limit_bytes)
            if _request_body_kind(path) == "json":
                body = await prepare_knowledge_body(request, user, path=path, body=body)

        upstream_path = f"{upstream_prefix}/{path}" if path else upstream_prefix
        user_headers = {
            "X-User-Id": user.user_id,
            "X-Tenant-Id": user.tenant_id,
            "X-User-Tier": getattr(user, "tier", "normal"),
            "X-User-Type": getattr(user, "user_type", "user"),
            "X-User-Roles": ",".join(getattr(user, "roles", []) or []),
        }
        email = getattr(user, "email", None) or getattr(user, "user_email", None)
        if email:
            user_headers["X-User-Email"] = email
        name = getattr(user, "name", None) or getattr(user, "display_name", None)
        if name:
            user_headers["X-User-Name"] = name

        started_at = time.time()
        try:
            response = await _proxy.forward(
                request,
                user_headers,
                upstream_path=upstream_path,
                body=body,
            )
        except HTTPException as exc:
            _record_rag_proxy_trace(
                request,
                user,
                path=path,
                body=body or b"",
                response_status=exc.status_code,
                response_body=b"",
                started_at=started_at,
            )
            raise

        if not is_retrieve_path(path):
            return response
        if isinstance(response, StreamingResponse):
            response.body_iterator = _capture_retrieve_stream(
                response.body_iterator,
                request,
                user,
                path=path,
                request_body=body or b"",
                response_status=response.status_code,
                started_at=started_at,
            )
            return response

        response_body = getattr(response, "body", b"") or b""
        if len(response_body) > _RAG_TRACE_RESPONSE_CAPTURE_BYTES:
            return response
        _record_rag_proxy_trace(
            request,
            user,
            path=path,
            body=body or b"",
            response_status=response.status_code,
            response_body=response_body,
            started_at=started_at,
        )
        return response
    finally:
        if upload_lease is not None:
            _upload_admission.release(upload_lease)
