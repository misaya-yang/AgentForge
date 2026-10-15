"""Gateway-owned public image API.

The module is mounted by the Gateway composition during the Assistant cutover;
it intentionally has no dependency on the legacy Assistant service.
"""

from __future__ import annotations

import hashlib
import re
import uuid
from datetime import datetime, timedelta, timezone

from ai_gateway_core.image.image_state import compute_owner_scope
from ai_gateway_core.media.image_generation import validate_image_bytes
from ai_gateway_core.security import SafeFetchError, safe_fetch_with_response
from ai_gateway_core.storage import get_artifact_storage
from fastapi import APIRouter, Depends, HTTPException, Query, Request

from ...core.auth.user_resolver import UserContext
from ...services.images.service import ImageGenerationService, public_image_error
from ..deps import enforce_rate_limit, get_user_context
from ..schemas.assistant import (
    AsyncImageGenerationRequest,
    AsyncImageTaskStatusResponse,
    AsyncImageTaskSubmitResponse,
    ImageBlobCompleteRequest,
    ImageBlobFetchUrlRequest,
    ImageBlobResponse,
    ImageBlobUploadUrlRequest,
    ImageBlobUploadUrlResponse,
    ImageGenerationRequest,
    ImageGenerationResponse,
)

router = APIRouter(prefix="/assistant", tags=["assistant-images"])
_MAX_BLOB_BYTES = 8 * 1024 * 1024


def _owner_scope(user: UserContext) -> str:
    """Bind image state to the authenticated tenant and user."""

    return compute_owner_scope(
        user.user_id,
        app_tenant_id=user.tenant_id,
        app_user_id=user.user_id,
    )
_SUPPORTED_IMAGE_MIMES = {"image/png", "image/jpeg", "image/gif", "image/webp"}


def _sniff_mime(content: bytes) -> str | None:
    if content.startswith(b"\x89PNG\r\n\x1a\n"):
        return "image/png"
    if content.startswith(b"\xff\xd8\xff"):
        return "image/jpeg"
    if content.startswith((b"GIF87a", b"GIF89a")):
        return "image/gif"
    if content.startswith(b"RIFF") and content[8:12] == b"WEBP":
        return "image/webp"
    return None


def _service(request: Request, user: UserContext) -> ImageGenerationService:
    return ImageGenerationService(request, user)


@router.post("/generate-image", response_model=ImageGenerationResponse)
async def generate_image(
    body: ImageGenerationRequest, request: Request, user: UserContext = Depends(get_user_context)
):
    await enforce_rate_limit(request, user, operation="image_generate")
    return await _service(request, user).generate(body)


@router.post("/generate-image-async", response_model=AsyncImageTaskSubmitResponse)
async def submit_image_generation(
    body: AsyncImageGenerationRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    await enforce_rate_limit(request, user, operation="image_generate")
    return await _service(request, user).submit(body)


@router.get("/image-task/{task_id}", response_model=AsyncImageTaskStatusResponse)
async def get_image_task_status(
    task_id: str, request: Request, user: UserContext = Depends(get_user_context)
):
    service = _service(request, user)
    row = await service.task(task_id)
    result = row.get("result") or {}
    return {
        "task_id": task_id,
        "status": row.get("status", "pending"),
        "progress": row.get("progress", 0),
        "prompt": row.get("prompt", ""),
        "model_id": row.get("model_id", ""),
        "provider": row.get("provider"),
        # Historical rows without a receipt cannot inherit today's image model.
        "effective_model_id": result.get("effective_model_id"),
        "images": result.get("images", []),
        "duration_ms": result.get("duration_ms"),
        "error": public_image_error(row.get("status"), row.get("error_code")),
        "error_code": row.get("error_code"),
        "created_at": row["created_at"].isoformat()
        if hasattr(row.get("created_at"), "isoformat")
        else str(row.get("created_at", "")),
        "completed_at": row.get("completed_at").isoformat()
        if hasattr(row.get("completed_at"), "isoformat")
        else None,
        "turn_id": row.get("turn_id"),
        "session_id": row.get("session_id"),
        "parent_artifact_id": row.get("parent_artifact_id"),
        "output_artifact_id": row.get("output_artifact_id"),
        "client_request_id": row.get("client_request_id"),
        "latest_advanced": result.get("latest_advanced"),
    }


@router.get("/image-sessions/{session_id}")
async def get_image_session_view(
    session_id: str,
    request: Request,
    limit: int = Query(50, ge=1, le=200),
    cursor: str | None = Query(None),
    include_urls: bool = Query(False),
    user: UserContext = Depends(get_user_context),
):
    return await _service(request, user).session(session_id, limit, cursor, include_urls)


@router.get("/artifacts/{artifact_id}/download-url")
async def get_artifact_download_url(
    artifact_id: str,
    request: Request,
    variant: str = Query("display", pattern="^(raw|display|thumbnail)$"),
    expires_in: int = Query(3600, ge=60, le=3600),
    user: UserContext = Depends(get_user_context),
):
    storage = get_artifact_storage()
    if not storage:
        raise HTTPException(503, detail={"error_code": "storage_unavailable"})
    owner = _owner_scope(user)
    url, actual = await storage.get_presigned_download_url_for_variant(
        artifact_id,
        variant,
        expires_in,
        owner_scope=owner,
        tenant_id=user.tenant_id,
        user_id=user.user_id,
    )
    if not url:
        raise HTTPException(
            404, detail={"error_code": "not_found", "message": "artifact not found"}
        )
    return {
        "artifact_id": artifact_id,
        "variant": actual or variant,
        "url": url,
        "expires_at": datetime.fromtimestamp(
            datetime.now(timezone.utc).timestamp() + expires_in, timezone.utc
        ).isoformat(),
    }


@router.post("/image-blobs/upload-url", response_model=ImageBlobUploadUrlResponse)
async def create_image_blob_upload_url(
    body: ImageBlobUploadUrlRequest, request: Request, user: UserContext = Depends(get_user_context)
):
    await enforce_rate_limit(request, user, operation="image_generate")
    if (
        (body.byte_size is not None and body.byte_size > _MAX_BLOB_BYTES)
        or body.mime_type.lower() not in _SUPPORTED_IMAGE_MIMES
        or (body.content_sha256 and not re.fullmatch(r"[0-9a-f]{64}", body.content_sha256))
    ):
        raise HTTPException(422, detail={"error_code": "invalid_upload"})
    storage = get_artifact_storage()
    if not storage:
        raise HTTPException(503, detail={"error_code": "storage_unavailable"})
    blob_id = f"iblob_{uuid.uuid4().hex[:24]}"
    owner = _owner_scope(user)
    try:
        storage_key, upload = await storage.create_image_blob_upload_url(
            owner_scope=owner,
            blob_id=blob_id,
            filename=body.filename,
            mime_type=body.mime_type,
            expiry_seconds=900,
        )
    except (ValueError, RuntimeError):
        raise HTTPException(422, detail={"error_code": "invalid_upload"}) from None
    if not upload:
        raise HTTPException(503, detail={"error_code": "storage_unavailable"})
    from ai_gateway_core.image.image_state import create_image_blob

    if not await create_image_blob(
        getattr(getattr(request.app.state, "database", None), "_pool", None),
        blob_id=blob_id,
        owner_scope=owner,
        content_sha256=body.content_sha256,
        byte_size=body.byte_size,
        mime_type=body.mime_type,
        storage_key=storage_key,
        source="user_upload",
        status="pending",
    ):
        raise HTTPException(503, detail={"error_code": "storage_unavailable"})
    if isinstance(upload, dict):
        upload_url, headers = upload.get("url", ""), upload.get("headers", {})
    elif isinstance(upload, tuple):
        upload_url, headers = upload
    else:
        upload_url, headers = upload, {"Content-Type": body.mime_type}
    return {
        "blob_id": blob_id,
        "upload_url": upload_url,
        "method": "PUT",
        "headers": headers or {},
        "fields": None,
        "storage_key": storage_key,
        "expires_at": (datetime.now(timezone.utc) + timedelta(seconds=900)).isoformat(),
    }


@router.post("/image-blobs/complete", response_model=ImageBlobResponse)
async def complete_image_blob_upload(
    body: ImageBlobCompleteRequest, request: Request, user: UserContext = Depends(get_user_context)
):
    await enforce_rate_limit(request, user, operation="image_generate")
    storage = get_artifact_storage()
    from ai_gateway_core.image.image_state import get_image_blob, update_image_blob_status

    if not storage:
        raise HTTPException(503, detail={"error_code": "storage_unavailable"})

    pool = getattr(getattr(request.app.state, "database", None), "_pool", None)
    owner = _owner_scope(user)
    if body.content_sha256 and not re.fullmatch(r"[0-9a-f]{64}", body.content_sha256):
        raise HTTPException(422, detail={"error_code": "invalid_upload"})
    if body.mime_type and body.mime_type.lower() not in _SUPPORTED_IMAGE_MIMES:
        raise HTTPException(422, detail={"error_code": "invalid_upload"})
    row = await get_image_blob(pool, blob_id=body.blob_id, owner_scope=owner)
    if not row:
        raise HTTPException(404, detail={"error_code": "not_found"})
    if row.get("status") == "ready":
        if (
            (body.content_sha256 and body.content_sha256 != row.get("content_sha256"))
            or (
                body.byte_size is not None and body.byte_size != row.get("byte_size")
            )
            or (body.mime_type and body.mime_type != row.get("mime_type"))
        ):
            raise HTTPException(409, detail={"error_code": "blob_integrity_mismatch"})
        return {
            "blob_id": body.blob_id,
            "status": "ready",
            "content_sha256": row.get("content_sha256"),
            "byte_size": row.get("byte_size"),
            "mime_type": row.get("mime_type"),
            "storage_key": row["storage_key"],
        }
    if row.get("status") != "pending":
        raise HTTPException(409, detail={"error_code": "blob_invalid"})
    try:
        info = await storage.inspect_image_blob_object(
            row["storage_key"], max_bytes=_MAX_BLOB_BYTES
        )
        content = await storage.read_image_blob_object(
            row["storage_key"], max_bytes=_MAX_BLOB_BYTES
        )
    except (FileNotFoundError, ValueError):
        raise HTTPException(
            409,
            detail={
                "error_code": "blob_invalid",
                "message": "uploaded object is missing or changed",
            },
        ) from None
    actual_mime = _sniff_mime(content)
    actual_sha = hashlib.sha256(content).hexdigest()
    declared_mime = body.mime_type or row.get("mime_type")
    declared_sha = body.content_sha256 or row.get("content_sha256")
    declared_size = body.byte_size if body.byte_size is not None else row.get("byte_size")
    if (
        not actual_mime
        or (declared_mime and actual_mime != declared_mime)
        or not validate_image_bytes(content, actual_mime)
        or (declared_sha and actual_sha != declared_sha)
            or (declared_size is not None and len(content) != declared_size)
        or (
            info and info.content_type and info.content_type.split(";", 1)[0].lower() != actual_mime
        )
    ):
        raise HTTPException(
            409,
            detail={
                "error_code": "blob_integrity_mismatch",
                "message": "uploaded object metadata does not match",
            },
        )
    if not await update_image_blob_status(
        pool,
        blob_id=body.blob_id,
        owner_scope=owner,
        status="ready",
        content_sha256=actual_sha,
        byte_size=len(content),
        mime_type=actual_mime,
    ):
        raise HTTPException(503, detail={"error_code": "storage_unavailable"})
    return {
        "blob_id": body.blob_id,
        "status": "ready",
        "content_sha256": actual_sha,
        "byte_size": len(content),
        "mime_type": actual_mime,
        "storage_key": row["storage_key"],
    }


@router.post("/image-blobs/fetch-url", response_model=ImageBlobResponse)
async def fetch_image_blob_from_url(
    body: ImageBlobFetchUrlRequest, request: Request, user: UserContext = Depends(get_user_context)
):
    await enforce_rate_limit(request, user, operation="image_generate")
    storage = get_artifact_storage()
    if not storage:
        raise HTTPException(503, detail={"error_code": "storage_unavailable"})
    try:
        fetched = await safe_fetch_with_response(body.url, max_bytes=_MAX_BLOB_BYTES, timeout=30.0)
    except (SafeFetchError, ValueError):
        raise HTTPException(422, detail={"error_code": "invalid_image_url"}) from None
    if fetched.status_code < 200 or fetched.status_code >= 300:
        raise HTTPException(422, detail={"error_code": "invalid_image_url"})
    mime = _sniff_mime(fetched.body)
    declared_mime = (body.mime_type or "").lower()
    response_mime = getattr(fetched, "content_type", "").split(";", 1)[0].strip().lower()
    if (
        not mime
        or mime not in _SUPPORTED_IMAGE_MIMES
        or (declared_mime and declared_mime != mime)
        or (response_mime and response_mime != mime)
        or not validate_image_bytes(fetched.body, mime)
    ):
        raise HTTPException(422, detail={"error_code": "invalid_image"})
    from ai_gateway_core.image.image_state import create_image_blob

    blob_id = f"iblob_{uuid.uuid4().hex[:24]}"
    owner = _owner_scope(user)
    key = storage.image_blob_storage_key(owner, blob_id, "reference." + mime.split("/", 1)[1])
    await storage.store_image_blob_object(
        key, content=fetched.body, mime_type=mime, max_bytes=_MAX_BLOB_BYTES
    )
    digest = hashlib.sha256(fetched.body).hexdigest()
    if not await create_image_blob(
        getattr(getattr(request.app.state, "database", None), "_pool", None),
        blob_id=blob_id,
        owner_scope=owner,
        content_sha256=digest,
        byte_size=len(fetched.body),
        mime_type=mime,
        storage_key=key,
        source="remote_fetch",
        status="ready",
    ):
        raise HTTPException(503, detail={"error_code": "storage_unavailable"})
    return {
        "blob_id": blob_id,
        "status": "ready",
        "content_sha256": digest,
        "byte_size": len(fetched.body),
        "mime_type": mime,
        "storage_key": key,
    }
