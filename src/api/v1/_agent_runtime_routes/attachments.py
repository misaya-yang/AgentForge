"""Published Agent Runtime attachment storage, resolution and upload route.

ARC-01B split of ``src/api/v1/agent_runtime.py``.  Moved verbatim; route
registration stays in the facade.  The upload handler is a plain function; the
facade registers it with ``router.add_api_route`` to preserve the original
route order and operation id.
"""

from __future__ import annotations

import contextlib
import hashlib
from typing import Any

from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentRepositoryError,
)
from fastapi import File, HTTPException, Request, UploadFile

from ....core.auth.user_resolver import UserContext
from ...schemas.agent_runtime import AgentRuntimeAttachmentUploadResponse
from .._agent_runtime_headers import reject_client_agent_forgery
from ..files import MAX_FILE_SIZE_BYTES, _stream_upload_file, get_mime_type, validate_file_extension
from .core import (
    _map_repository_error,
    _raise_runtime_error,
    _repository,
    _request_id,
    _resolve_api_caller,
)
from .rate_limit import _enforce_channel_limits
from .snapshot import _assert_attachments_allowed, _build_snapshot


def _file_storage(request: Request) -> Any:
    storage = getattr(request.app.state, "file_storage", None)
    if storage is not None:
        return storage
    try:
        from ai_gateway_core.storage import get_file_storage

        return get_file_storage()
    except RuntimeError:
        _raise_runtime_error(
            request,
            503,
            "AGENT_RUNTIME_ATTACHMENT_STORAGE_UNAVAILABLE",
            "Attachment storage is unavailable",
        )


async def _store_runtime_attachment(
    request: Request,
    user: UserContext,
    *,
    publication_id: str,
    channel: str,
    file: UploadFile,
) -> AgentRuntimeAttachmentUploadResponse:
    """Store bytes first, then publish only an opaque DB-scoped handle."""

    filename = str(file.filename or "").strip()
    if (
        not filename
        or len(filename) > 255
        or "\x00" in filename
        or filename != filename.replace("\\", "/").rsplit("/", 1)[-1]
    ):
        _raise_runtime_error(request, 422, "AGENT_RUNTIME_ATTACHMENT_INVALID", "Filename required")
    try:
        extension = validate_file_extension(filename)
    except HTTPException as exc:
        _raise_runtime_error(
            request,
            422 if exc.status_code < 500 else exc.status_code,
            "AGENT_RUNTIME_ATTACHMENT_INVALID",
            "Attachment type is unsupported",
        )
    storage = _file_storage(request)
    storage_owner = "ar_" + hashlib.sha256(
        f"{user.tenant_id}:{user.user_id}".encode()
    ).hexdigest()[:40]
    try:
        uploaded = await storage.upload_file_streaming(
            user_id=storage_owner,
            tenant_id=user.tenant_id,
            filename=filename,
            content_iterator=_stream_upload_file(file),
            content_type=get_mime_type(extension),
            max_size_bytes=MAX_FILE_SIZE_BYTES,
            metadata={
                "agent_runtime": "true",
                "publication_id": publication_id,
                "channel": channel,
            },
        )
    except ValueError as exc:
        _raise_runtime_error(
            request,
            413,
            "AGENT_RUNTIME_ATTACHMENT_TOO_LARGE",
            str(exc),
        )
    except Exception as exc:
        raise HTTPException(
            status_code=503,
            detail={
                "code": "AGENT_RUNTIME_ATTACHMENT_STORAGE_UNAVAILABLE",
                "message": "Attachment storage is unavailable",
                "request_id": _request_id(request),
            },
        ) from exc
    try:
        row = await _repository(request).create_runtime_attachment(
            tenant_id=user.tenant_id,
            publication_id=publication_id,
            principal_id=user.user_id,
            channel=channel,
            storage_key=uploaded.storage_key,
            filename=filename,
            mime_type=uploaded.content_type,
            size_bytes=uploaded.size_bytes,
        )
    except Exception as exc:
        with contextlib.suppress(Exception):
            await storage.delete_file(uploaded.storage_key)
        _map_repository_error(request, exc)
    return AgentRuntimeAttachmentUploadResponse(
        artifact_id=str(row["attachment_id"]),
        filename=str(row["filename"]),
        mime_type=str(row["mime_type"]),
        size_bytes=int(row["size_bytes"]),
        expires_at=row["expires_at"],
        request_id=_request_id(request),
    )


async def _resolve_runtime_attachments(
    request: Request,
    user: UserContext,
    *,
    publication_id: str,
    channel: str,
    attachments: list[Any],
) -> list[dict[str, Any]]:
    if not attachments:
        return []
    try:
        return await _repository(request).resolve_runtime_attachments(
            tenant_id=user.tenant_id,
            publication_id=publication_id,
            principal_id=user.user_id,
            channel=channel,
            attachment_ids=[str(item.artifact_id) for item in attachments],
        )
    except AgentRepositoryError as exc:
        _map_repository_error(request, exc)


async def upload_published_attachment(
    publication_id: str,
    request: Request,
    file: UploadFile = File(...),
) -> AgentRuntimeAttachmentUploadResponse:
    reject_client_agent_forgery(request)
    resolution, user = await _resolve_api_caller(
        request,
        publication_id=publication_id,
        required_scopes=["attachments:write"],
    )
    await _enforce_channel_limits(
        request,
        publication=resolution["publication"],
        principal_id=user.user_id,
    )
    snapshot = await _build_snapshot(request, resolution, user, channel="api")
    _assert_attachments_allowed(request, snapshot, [file])
    return await _store_runtime_attachment(
        request,
        user,
        publication_id=publication_id,
        channel="api",
        file=file,
    )
