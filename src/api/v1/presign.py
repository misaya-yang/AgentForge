"""
Presigned URL API for Direct Upload.

Part of P2: Architecture Decoupling

The direct-upload flow is unimplemented: every endpoint validates the request
shape and then fails closed with 501, before any storage state or upload
session is created. The schemas stay because they are part of the frozen
public contract (``sdk/openapi.json``); clients must use the standard upload
API (proxied to knowledge-service).

PRD T8.2 note: the former in-gateway document/dataset ownership check was
removed with it — the gateway does not read KB tables. If direct upload is
ever implemented, document authorization must go through knowledge-service's
internal authorization endpoint rather than re-implementing the dataset ACL
here.
"""

from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel, Field

from ...core.auth.user_resolver import UserContext
from ..deps import get_user_context

router = APIRouter(prefix="/presign", tags=["Presigned Upload"])

_DIRECT_UPLOAD_UNAVAILABLE = (
    "Direct presigned upload is not implemented; use the standard upload API."
)


# ============ Schemas ============


class PresignedUploadRequest(BaseModel):
    """Request for presigned upload URL."""

    filename: str = Field(..., description="Original filename", max_length=255)
    content_type: str = Field(..., description="MIME type of the file")
    document_id: str = Field(..., description="Document ID to associate with")
    file_size_bytes: int | None = Field(None, description="Expected file size in bytes")
    metadata: dict | None = Field(None, description="Optional metadata")


class PresignedUploadResponse(BaseModel):
    """Response with presigned upload URL."""

    upload_url: str = Field(..., description="Presigned URL for PUT upload")
    method: str = Field(default="PUT", description="HTTP method to use")
    headers: dict = Field(default_factory=dict, description="Required headers for upload")
    storage_key: str = Field(..., description="Storage key for the uploaded file")
    upload_id: str = Field(..., description="Unique ID for this upload session")
    expiry_seconds: int = Field(..., description="URL expiry time in seconds")
    expires_at: str = Field(..., description="URL expiry timestamp ISO8601")


class UploadConfirmRequest(BaseModel):
    """Request to confirm upload completion."""

    upload_id: str = Field(..., description="Upload ID from presigned URL response")
    storage_key: str = Field(..., description="Storage key from presigned URL response")
    document_id: str = Field(..., description="Document ID")
    filename: str = Field(..., description="Original filename")
    content_type: str = Field(..., description="MIME type")
    file_size_bytes: int | None = Field(None, description="Actual file size")


class UploadConfirmResponse(BaseModel):
    """Response after confirming upload."""

    status: str = Field(..., description="Status: processing, queued, error")
    task_id: str | None = Field(None, description="Task ID for async processing")
    message: str = Field(..., description="Status message")
    storage_url: str = Field(..., description="Final storage URL")


# ============ Endpoints ============


@router.post("/upload", response_model=PresignedUploadResponse, deprecated=True)
async def get_presigned_upload_url(
    request: PresignedUploadRequest,
    user: UserContext = Depends(get_user_context),
):
    """Reject the incomplete direct-upload flow before creating storage state."""
    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="Authentication required")
    raise HTTPException(status_code=501, detail=_DIRECT_UPLOAD_UNAVAILABLE)


@router.post("/confirm", response_model=UploadConfirmResponse, deprecated=True)
async def confirm_upload(
    request: UploadConfirmRequest,
    user: UserContext = Depends(get_user_context),
):
    """Reject confirmation because direct uploads are never issued."""
    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="Authentication required")
    raise HTTPException(status_code=501, detail=_DIRECT_UPLOAD_UNAVAILABLE)


@router.get("/status/{task_id}", deprecated=True)
async def get_task_status(
    task_id: str,
    user: UserContext = Depends(get_user_context),
):
    """Reject status lookup because the direct-upload queue is not implemented."""
    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="Authentication required")
    raise HTTPException(status_code=501, detail=_DIRECT_UPLOAD_UNAVAILABLE)
