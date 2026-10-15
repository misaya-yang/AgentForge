"""
File Upload API for Conversation Module

Provides:
- POST /files/upload - Upload files for analysis
- GET /files - List user's uploaded files
- GET /files/{file_id} - Get file info
- DELETE /files/{file_id} - Delete uploaded file
- GET /files/admin/stats - Get storage statistics (admin only)
- POST /files/admin/cleanup - Trigger manual cleanup (admin only)

Storage backends:
- S3: When FILE_STORAGE_BACKEND=s3
- OSS: When FILE_STORAGE_BACKEND=oss
- Local: Default fallback
"""

from __future__ import annotations

import contextlib
import os
import re
import uuid
from collections.abc import AsyncIterator
from datetime import datetime, timezone
from pathlib import Path

from ai_gateway_core.logging import get_logger
from ai_gateway_core.storage import FileStorageService, get_file_storage
from fastapi import APIRouter, Depends, File, HTTPException, Query, Request, UploadFile
from pydantic import BaseModel, Field

from ...core.auth.user_resolver import UserContext
from ...core.file_cleanup import get_cleanup_service
from ..deps import get_user_context

logger = get_logger(__name__)

router = APIRouter(prefix="/files", tags=["File Upload"])


# ============ Configuration ============

# 可配置的存储路径（跨平台）
FILE_STORAGE_PATH = Path(os.getenv("FILE_STORAGE_PATH", "./uploads")).expanduser()
MAX_FILE_SIZE_MB = int(os.getenv("MAX_FILE_SIZE_MB", "50"))
MAX_FILE_SIZE_BYTES = MAX_FILE_SIZE_MB * 1024 * 1024

# Admin user IDs (可从环境变量配置)
ADMIN_USER_IDS = {
    uid.strip() for uid in os.getenv("FILE_ADMIN_USER_IDS", "").split(",") if uid.strip()
}

# File ID format: exactly 8 hex characters
FILE_ID_PATTERN = re.compile(r"^[a-f0-9]{8}$")

# User ID validation: alphanumeric, underscore, hyphen, max 64 chars
# This prevents path traversal attacks via user_id
USER_ID_PATTERN = re.compile(r"^[a-zA-Z0-9_-]{1,64}$")

# 支持的文件类型
ALLOWED_EXTENSIONS = {
    # Documents
    ".pdf",
    ".docx",
    ".doc",
    ".md",
    ".txt",
    ".csv",
    ".xlsx",
    # Images
    ".png",
    ".jpg",
    ".jpeg",
    ".gif",
    ".webp",
    ".bmp",
}


# ============ Schemas ============


class FileUploadResponse(BaseModel):
    """Response after successful file upload."""

    file_id: str = Field(..., description="Unique file identifier")
    file_path: str = Field(..., description="Path to use in analysis requests")
    filename: str = Field(..., description="Original filename")
    size_bytes: int = Field(..., description="File size in bytes")
    mime_type: str = Field(..., description="Detected MIME type")
    uploaded_at: str = Field(..., description="Upload timestamp ISO8601")
    message: str = Field(default="File uploaded successfully")


class FileInfo(BaseModel):
    """File information."""

    file_id: str
    file_path: str
    filename: str
    size_bytes: int
    mime_type: str
    uploaded_at: str


class FilesListResponse(BaseModel):
    """List of user's uploaded files."""

    files: list[FileInfo]
    total: int


# ============ Helper Functions ============


def _get_storage_service() -> FileStorageService | None:
    """
    Get the file storage service if available.

    Returns None if the service isn't initialized (falls back to local storage).
    """
    try:
        return get_file_storage()
    except RuntimeError:
        return None


async def _stream_upload_file(
    upload_file: UploadFile, chunk_size: int = 64 * 1024
) -> AsyncIterator[bytes]:
    """Convert UploadFile to async iterator for streaming upload."""
    while True:
        chunk = await upload_file.read(chunk_size)
        if not chunk:
            break
        yield chunk


def validate_user_id(user_id: str) -> str:
    """
    Validate user_id format to prevent path traversal attacks.

    Args:
        user_id: User identifier to validate

    Returns:
        Validated user_id

    Raises:
        HTTPException: If user_id contains invalid characters
    """
    if not user_id:
        raise HTTPException(status_code=400, detail="User ID is required")

    if not USER_ID_PATTERN.match(user_id):
        logger.warning(f"[Security] Invalid user_id format attempted: {user_id[:50]}")
        raise HTTPException(
            status_code=400,
            detail="Invalid user ID format. Only alphanumeric, underscore, and hyphen allowed (max 64 chars)",
        )

    return user_id


def validate_file_id(file_id: str) -> str:
    """
    Validate file_id format for exact matching.

    Args:
        file_id: File identifier to validate

    Returns:
        Validated file_id

    Raises:
        HTTPException: If file_id format is invalid
    """
    if not file_id:
        raise HTTPException(status_code=400, detail="File ID is required")

    if not FILE_ID_PATTERN.match(file_id):
        raise HTTPException(
            status_code=400,
            detail="Invalid file ID format. Must be exactly 8 hexadecimal characters",
        )

    return file_id


def _is_platform_admin(user: UserContext) -> bool:
    return bool({str(role).strip().lower() for role in (user.roles or [])}
                & {"platform_admin", "superadmin", "super_admin"})


def require_admin(user: UserContext) -> None:
    """
    Check if user has admin privileges for file management.

    Args:
        user: Current user context

    Raises:
        HTTPException: If user is not an admin
    """
    if _is_platform_admin(user):
        return

    # Check if user_id is in admin list
    if user.user_id in ADMIN_USER_IDS:
        return

    # Check if user has admin role (if roles are available)
    user_roles = getattr(user, "roles", []) or []
    if "admin" in user_roles or "file_admin" in user_roles:
        return

    logger.warning(f"[Security] Non-admin user attempted admin operation: user_id={user.user_id}")
    raise HTTPException(status_code=403, detail="Admin access required for this operation")


def get_uploads_path() -> Path:
    """Get the uploads base directory, creating if needed."""
    path = FILE_STORAGE_PATH / "uploads"
    path.mkdir(parents=True, exist_ok=True)
    return path


def get_user_uploads_path(user_id: str, tenant_id: str = "") -> Path:
    """Get user-specific uploads directory with validation.

    Implements defense-in-depth with both input validation and path verification.
    """
    # Validate user_id to prevent path traversal
    validated_id = validate_user_id(user_id)
    base_path = get_uploads_path()
    validated_tenant = validate_user_id(tenant_id) if tenant_id else ""
    path = base_path / validated_tenant / validated_id if validated_tenant else base_path / validated_id

    # Defense-in-depth: Verify resolved path doesn't escape base directory
    try:
        resolved = path.resolve()
        base_resolved = base_path.resolve()
        resolved.relative_to(base_resolved)
    except ValueError:
        logger.error(f"[Security] Path escape attempt detected for user {user_id[:50]}")
        raise HTTPException(status_code=400, detail="Invalid path")

    path.mkdir(parents=True, exist_ok=True)
    return path


def validate_file_extension(filename: str) -> str:
    """Validate and return file extension."""
    ext = Path(filename).suffix.lower()
    if ext not in ALLOWED_EXTENSIONS:
        raise HTTPException(
            status_code=400,
            detail=f"File type not allowed: {ext}. Allowed types: {', '.join(sorted(ALLOWED_EXTENSIONS))}",
        )
    return ext


def get_mime_type(ext: str) -> str:
    """Get MIME type from extension."""
    mime_map = {
        ".pdf": "application/pdf",
        ".docx": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ".doc": "application/msword",
        ".md": "text/markdown",
        ".txt": "text/plain",
        ".csv": "text/csv",
        ".xlsx": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ".png": "image/png",
        ".jpg": "image/jpeg",
        ".jpeg": "image/jpeg",
        ".gif": "image/gif",
        ".webp": "image/webp",
        ".bmp": "image/bmp",
    }
    return mime_map.get(ext, "application/octet-stream")


def generate_file_id() -> str:
    """Generate unique file ID."""
    return str(uuid.uuid4())[:8]


# ============ Endpoints ============


@router.post(
    "/upload",
    response_model=FileUploadResponse,
    summary="Upload file for analysis",
    description="""
    Upload a file for later analysis by the AI agent.

    Supported file types:
    - Documents: PDF, DOCX, DOC, MD, TXT, CSV, XLSX
    - Images: PNG, JPG, JPEG, GIF, WEBP, BMP

    After upload, use the returned `file_path` in your message to the agent:
    "Please analyze this file: /uploads/user123/abc123_document.pdf"

    Storage backend is configurable via FILE_STORAGE_BACKEND env var (s3/oss/local).
    """,
)
async def upload_file(
    request: Request,
    file: UploadFile = File(..., description="File to upload"),
    user: UserContext = Depends(get_user_context),
) -> FileUploadResponse:
    """Handle file upload with streaming size validation."""
    from ..deps import enforce_rate_limit
    await enforce_rate_limit(request, user, operation="file_upload")

    # Validate file
    if not file.filename:
        raise HTTPException(status_code=400, detail="Filename is required")

    ext = validate_file_extension(file.filename)

    # Try to use storage service (S3/OSS)
    storage_service = _get_storage_service()

    if storage_service:
        # Use unified storage service (S3/OSS/Local)
        try:
            file_info = await storage_service.upload_file_streaming(
                user_id=user.user_id,
                tenant_id=user.tenant_id,
                filename=file.filename,
                content_iterator=_stream_upload_file(file),
                content_type=get_mime_type(ext),
                max_size_bytes=MAX_FILE_SIZE_BYTES,
            )

            # Build response path (API-facing path format)
            relative_path = file_info.file_path
            size_mb = file_info.size_bytes / (1024 * 1024)

            logger.info(
                f"[FileUpload] user={user.user_id} file={file.filename} "
                f"size={size_mb:.2f}MB backend={storage_service.config.backend.value} "
                f"key={file_info.storage_key}"
            )

            # Attachment parsing is performed on demand through the
            # scope-bound capability broker. Upload stays a storage-only
            # operation and never starts a second model/tool loop.

            return FileUploadResponse(
                file_id=file_info.file_id,
                file_path=relative_path,
                filename=file.filename,
                size_bytes=file_info.size_bytes,
                mime_type=file_info.content_type,
                uploaded_at=file_info.uploaded_at.isoformat(),
                message=f"File uploaded successfully. Use this path in your message: {relative_path}",
            )

        except ValueError as e:
            # Size limit exceeded
            raise HTTPException(status_code=413, detail=str(e))
        except Exception as e:
            # Log full error details for debugging
            import traceback

            logger.error(
                f"Storage service upload failed: {e}\n"
                f"File: {file.filename}, User: {user.user_id}\n"
                f"Traceback: {traceback.format_exc()}"
            )
            raise HTTPException(status_code=500, detail=f"Failed to save file: {str(e)}")

    # Fallback to direct local storage (when storage service not initialized)
    file_id = generate_file_id()
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
    safe_filename = f"{file_id}_{timestamp}{ext}"

    # Create user directory and prepare file path
    user_dir = get_user_uploads_path(user.user_id, user.tenant_id)
    file_path = user_dir / safe_filename
    temp_path = file_path.with_suffix(file_path.suffix + ".tmp")

    # Stream file to disk with chunked size checks (prevents memory exhaustion)
    size_bytes = 0
    chunk_size = 64 * 1024  # 64KB chunks

    try:
        with open(temp_path, "wb") as f:
            while True:
                chunk = await file.read(chunk_size)
                if not chunk:
                    break

                size_bytes += len(chunk)

                # Check size limit BEFORE writing more data
                if size_bytes > MAX_FILE_SIZE_BYTES:
                    # Clean up temp file
                    f.close()
                    temp_path.unlink(missing_ok=True)
                    raise HTTPException(
                        status_code=413,
                        detail=f"File too large: exceeds {MAX_FILE_SIZE_MB} MB limit",
                    )

                f.write(chunk)

        # Rename temp file to final path (atomic on most systems)
        temp_path.rename(file_path)

    except HTTPException:
        raise
    except Exception as e:
        # Clean up temp file on error
        temp_path.unlink(missing_ok=True)
        logger.error(f"Failed to save file: {e}")
        raise HTTPException(status_code=500, detail="Failed to save file")

    # Build response path (relative to uploads root for agent access)
    if user.tenant_id:
        relative_path = f"/uploads/{user.tenant_id}/{user.user_id}/{safe_filename}"
    else:
        relative_path = f"/uploads/{user.user_id}/{safe_filename}"
    size_mb = size_bytes / (1024 * 1024)

    logger.info(
        f"[FileUpload] user={user.user_id} file={file.filename} "
        f"size={size_mb:.2f}MB path={relative_path} (local fallback)"
    )

    # Phase 5d: pre-processing task removed; AS handles on-demand. See the
    # storage-backed branch above for the same rationale.

    return FileUploadResponse(
        file_id=file_id,
        file_path=relative_path,
        filename=file.filename,
        size_bytes=size_bytes,
        mime_type=get_mime_type(ext),
        uploaded_at=datetime.now(timezone.utc).isoformat(),
        message=f"File uploaded successfully. Use this path in your message: {relative_path}",
    )


@router.post(
    "/upload/multiple",
    response_model=list[FileUploadResponse],
    summary="Upload multiple files",
)
async def upload_multiple_files(
    request: Request,
    files: list[UploadFile] = File(..., max_length=5),
    user: UserContext = Depends(get_user_context),
) -> list[FileUploadResponse]:
    """Upload multiple files (max 5)."""

    if len(files) > 5:
        raise HTTPException(status_code=400, detail="Maximum 5 files per upload")

    results = []
    for f in files:
        result = await upload_file(request=request, file=f, user=user)
        results.append(result)

    return results


@router.get(
    "",
    response_model=FilesListResponse,
    summary="List uploaded files",
)
async def list_files(
    user: UserContext = Depends(get_user_context),
) -> FilesListResponse:
    """List files uploaded by the current user."""

    user_dir = get_user_uploads_path(user.user_id, user.tenant_id)

    files = []
    if user_dir.exists():
        for file_path in sorted(user_dir.iterdir(), key=lambda p: p.stat().st_mtime, reverse=True)[
            :50
        ]:
            if file_path.is_file():
                stat = file_path.stat()
                # Extract file_id from filename (first 8 chars before _)
                file_id = (
                    file_path.stem.split("_")[0] if "_" in file_path.stem else file_path.stem[:8]
                )
                ext = file_path.suffix.lower()

                files.append(
                    FileInfo(
                        file_id=file_id,
                        file_path=f"/uploads/{user.tenant_id}/{user.user_id}/{file_path.name}",
                        filename=file_path.name,
                        size_bytes=stat.st_size,
                        mime_type=get_mime_type(ext),
                        uploaded_at=datetime.fromtimestamp(
                            stat.st_mtime, tz=timezone.utc
                        ).isoformat(),
                    )
                )

    return FilesListResponse(files=files, total=len(files))


@router.get(
    "/{file_id}",
    response_model=FileInfo,
    summary="Get file info",
)
async def get_file_info(
    file_id: str,
    user: UserContext = Depends(get_user_context),
) -> FileInfo:
    """Get information about a specific file."""

    # Validate file_id format (exactly 8 hex chars)
    validated_file_id = validate_file_id(file_id)
    user_dir = get_user_uploads_path(user.user_id, user.tenant_id)

    # Find file by exact ID match (file_id followed by underscore)
    # Format: {file_id}_{timestamp}.{ext}
    for file_path in user_dir.iterdir():
        if file_path.is_file() and file_path.name.startswith(f"{validated_file_id}_"):
            stat = file_path.stat()
            ext = file_path.suffix.lower()
            return FileInfo(
                file_id=validated_file_id,
                file_path=f"/uploads/{user.tenant_id}/{user.user_id}/{file_path.name}",
                filename=file_path.name,
                size_bytes=stat.st_size,
                mime_type=get_mime_type(ext),
                uploaded_at=datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc).isoformat(),
            )

    raise HTTPException(status_code=404, detail="File not found")


@router.delete(
    "/{file_id}",
    summary="Delete uploaded file",
)
async def delete_file(
    file_id: str,
    user: UserContext = Depends(get_user_context),
) -> dict:
    """Delete an uploaded file."""

    # Validate file_id format (exactly 8 hex chars)
    validated_file_id = validate_file_id(file_id)

    # Try to use storage service
    storage_service = _get_storage_service()

    if storage_service:
        # Build storage key pattern to find the file
        # Key format: uploads/{user_id}/{file_id}_{timestamp}.{ext}
        prefix = f"uploads/{user.tenant_id}/{user.user_id}/{validated_file_id}_"

        # Use public delete_by_prefix API to delete matching files
        deleted = await storage_service.delete_by_prefix(prefix)

        if deleted > 0:
            logger.info(
                f"[FileDelete] user={user.user_id} file_id={validated_file_id} "
                f"backend={storage_service.config.backend.value}"
            )
            return {"status": "deleted", "file_id": validated_file_id}

        raise HTTPException(status_code=404, detail="File not found")

    # Fallback to direct local storage
    user_dir = get_user_uploads_path(user.user_id, user.tenant_id)

    # Find file by exact ID match (file_id followed by underscore)
    for file_path in user_dir.iterdir():
        if file_path.is_file() and file_path.name.startswith(f"{validated_file_id}_"):
            try:
                file_path.unlink()
                logger.info(f"[FileDelete] user={user.user_id} file_id={validated_file_id}")
                return {"status": "deleted", "file_id": validated_file_id}
            except Exception as e:
                logger.error(f"Failed to delete file: {e}")
                raise HTTPException(status_code=500, detail="Failed to delete file")

    raise HTTPException(status_code=404, detail="File not found")


# ============ Admin Endpoints ============


@router.get(
    "/admin/stats",
    summary="Get storage statistics",
    description="Get storage usage statistics for all users. Requires admin access.",
)
async def get_storage_stats(
    user: UserContext = Depends(get_user_context),
) -> dict:
    """Get storage statistics including per-user breakdown."""
    # Require admin access
    require_admin(user)
    if not _is_platform_admin(user):
        raise HTTPException(status_code=403, detail="Platform administrator required for global file administration")

    cleanup_service = get_cleanup_service()
    return await cleanup_service.get_storage_stats()


@router.post(
    "/admin/cleanup",
    summary="Trigger manual cleanup",
    description="Manually trigger file cleanup based on TTL and quota rules. Requires admin access.",
)
async def trigger_cleanup(
    user_id: str | None = Query(None, description="Clean up specific user only"),
    user: UserContext = Depends(get_user_context),
) -> dict:
    """Trigger manual file cleanup."""
    # Require admin access
    require_admin(user)
    if not _is_platform_admin(user):
        raise HTTPException(status_code=403, detail="Platform administrator required for global file administration")

    cleanup_service = get_cleanup_service()

    if user_id:
        # Validate target user_id format
        validate_user_id(user_id)
        # Clean specific user
        result = await cleanup_service.cleanup_user(user_id)
        logger.info(f"[FileCleanup] Manual cleanup for user={user_id} by={user.user_id}: {result}")
        return result
    else:
        # Full cleanup
        stats = await cleanup_service.run_cleanup()
        result = {
            "files_deleted": stats.files_deleted,
            "bytes_freed": stats.bytes_freed,
            "bytes_freed_formatted": f"{stats.bytes_freed / 1024 / 1024:.2f} MB",
            "users_cleaned": stats.users_cleaned,
            "errors": stats.errors,
            "duration_ms": stats.duration_ms,
        }
        logger.info(f"[FileCleanup] Manual cleanup by={user.user_id}: {result}")
        return result


@router.delete(
    "/admin/user/{target_user_id}",
    summary="Delete all files for a user",
    description="Delete all uploaded files for a specific user. Requires admin access.",
)
async def delete_user_files(
    target_user_id: str,
    target_tenant_id: str | None = Query(None),
    user: UserContext = Depends(get_user_context),
) -> dict:
    """Delete all files for a specific user."""
    # Require admin access
    require_admin(user)

    # Validate target user_id format
    validate_user_id(target_user_id)
    tenant_id = target_tenant_id or user.tenant_id
    if not _is_platform_admin(user) and (not user.tenant_id or tenant_id != user.tenant_id):
        raise HTTPException(status_code=403, detail="Cross-tenant file administration denied")
    user_dir = get_user_uploads_path(target_user_id, tenant_id)

    if not user_dir.exists():
        raise HTTPException(status_code=404, detail="User directory not found")

    files_deleted = 0
    bytes_freed = 0

    for file_path in list(user_dir.iterdir()):
        if file_path.is_file():
            try:
                size = file_path.stat().st_size
                file_path.unlink()
                files_deleted += 1
                bytes_freed += size
            except Exception as e:
                logger.error(f"Failed to delete {file_path}: {e}")

    # Remove empty directory
    with contextlib.suppress(Exception):
        user_dir.rmdir()

    logger.info(
        f"[FileDelete] Admin delete all files: target_user={target_user_id} "
        f"by={user.user_id} files={files_deleted}"
    )

    return {
        "status": "deleted",
        "user_id": target_user_id,
        "files_deleted": files_deleted,
        "bytes_freed": bytes_freed,
        "bytes_freed_formatted": f"{bytes_freed / 1024 / 1024:.2f} MB",
    }
