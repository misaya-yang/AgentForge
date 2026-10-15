"""
File Storage Service.

Provides unified interface for storing user-uploaded files across various backends:
- S3 (Amazon Web Services)
- OSS (Alibaba Cloud Object Storage Service)
- Local filesystem (for development)

Used for storing user file uploads for AI assistant analysis.
"""

from __future__ import annotations

import asyncio
import hashlib
import os
import uuid
from collections.abc import AsyncIterator
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

from ai_gateway_core.logging import get_logger

from .image_storage import (
    BaseStorageBackend,
    LocalStorageBackend,
    OSSStorageBackend,
    S3StorageBackend,
    StorageBackend,
    StorageConfig,
    storage_key_log_hash,
)

logger = get_logger(__name__)


@dataclass
class FileInfo:
    """Information about an uploaded file."""

    file_id: str
    user_id: str
    filename: str  # Original filename
    storage_key: str  # Full storage path
    size_bytes: int
    content_type: str
    uploaded_at: datetime
    metadata: dict[str, str] | None = None
    tenant_id: str | None = None

    @property
    def file_path(self) -> str:
        """Get API-facing file path."""
        if self.tenant_id:
            return f"/uploads/{self.tenant_id}/{self.user_id}/{self.filename}"
        return f"/uploads/{self.user_id}/{self.filename}"


class FileStorageService:
    """
    High-level file storage service for user uploads.

    Provides storage operations for files uploaded by users for AI analysis.
    Supports S3, OSS, and local filesystem backends.

    Example:
        service = FileStorageService(config)

        # Upload a file
        file_info = await service.upload_file(
            user_id="user123",
            filename="document.pdf",
            content=file_bytes,
            content_type="application/pdf",
        )

        # Get presigned download URL
        url = await service.get_download_url(file_info.storage_key)
    """

    # Storage key prefix for all user uploads
    KEY_PREFIX = "uploads"

    def __init__(self, config: StorageConfig):
        """
        Initialize the file storage service.

        Args:
            config: Storage configuration
        """
        self.config = config
        self._backend = self._create_backend()

    def _create_backend(self) -> BaseStorageBackend:
        """Create storage backend based on configuration."""
        kp = self.config.key_prefix
        if self.config.backend == StorageBackend.S3:
            return S3StorageBackend(
                bucket=self.config.s3_bucket,
                region=self.config.s3_region,
                access_key=self.config.s3_access_key,
                secret_key=self.config.s3_secret_key,
                endpoint_url=self.config.s3_endpoint_url,
                key_prefix=kp,
            )
        elif self.config.backend == StorageBackend.OSS:
            return OSSStorageBackend(
                bucket=self.config.oss_bucket,
                endpoint=self.config.oss_endpoint,
                access_key=self.config.oss_access_key,
                secret_key=self.config.oss_secret_key,
                key_prefix=kp,
            )
        else:
            return LocalStorageBackend(self.config.local_base_path, key_prefix=kp)

    @staticmethod
    def generate_file_id() -> str:
        """Generate a unique file ID (8 hex characters)."""
        return str(uuid.uuid4())[:8]

    @staticmethod
    def _validate_scope_id(value: str, label: str) -> None:
        if not value or not value.replace("_", "").replace("-", "").isalnum():
            raise ValueError(f"Invalid {label}: {value}")

    def _generate_key(
        self,
        user_id: str,
        file_id: str,
        ext: str,
        tenant_id: str | None = None,
    ) -> tuple[str, str]:
        """
        Generate storage key and safe filename for a file.

        Args:
            user_id: User ID
            file_id: Unique file ID
            ext: File extension (including dot)
            tenant_id: Tenant ID for path isolation (ADR-002)

        Returns:
            Tuple of (storage_key, safe_filename)

        Key structure: uploads/{tenant_id}/{user_id}/{file_id}_{timestamp}{ext}
        Falls back to: uploads/{user_id}/{file_id}_{timestamp}{ext} when tenant_id is empty.
        """
        timestamp = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
        safe_filename = f"{file_id}_{timestamp}{ext}"
        if tenant_id:
            storage_key = f"{self.KEY_PREFIX}/{tenant_id}/{user_id}/{safe_filename}"
        else:
            storage_key = f"{self.KEY_PREFIX}/{user_id}/{safe_filename}"
        return storage_key, safe_filename

    async def upload_file(
        self,
        user_id: str,
        filename: str,
        content: bytes,
        content_type: str,
        metadata: dict[str, str] | None = None,
        file_id: str | None = None,
        tenant_id: str | None = None,
    ) -> FileInfo:
        """
        Upload a file to storage.

        Args:
            user_id: User ID
            filename: Original filename
            content: File binary content
            content_type: MIME type
            metadata: Optional metadata
            file_id: Optional file ID (generated if not provided)
            tenant_id: Tenant ID for path isolation (ADR-002)

        Returns:
            FileInfo with upload details
        """
        # Validate user_id
        self._validate_scope_id(user_id, "user_id")
        if tenant_id:
            self._validate_scope_id(tenant_id, "tenant_id")

        # Get extension from original filename
        ext = Path(filename).suffix.lower()
        if not ext:
            ext = ".bin"

        # Generate file ID and storage key
        if not file_id:
            file_id = self.generate_file_id()

        storage_key, safe_filename = self._generate_key(user_id, file_id, ext, tenant_id=tenant_id)

        # Build metadata
        file_metadata = {
            "user_id": user_id,
            "file_id": file_id,
            "original_filename": filename,
            "content_hash": hashlib.sha256(content).hexdigest(),
        }
        if metadata:
            file_metadata.update(metadata)

        # Upload to storage
        await self._backend.upload(
            key=storage_key,
            content=content,
            content_type=content_type,
            metadata=file_metadata,
        )

        uploaded_at = datetime.now(timezone.utc)

        logger.info(
            f"[FileStorage] Uploaded {filename} ({len(content)} bytes) "
            f"for user {user_id} -> {storage_key}"
        )

        return FileInfo(
            file_id=file_id,
            user_id=user_id,
            filename=safe_filename,
            storage_key=storage_key,
            size_bytes=len(content),
            content_type=content_type,
            uploaded_at=uploaded_at,
            metadata=file_metadata,
            tenant_id=tenant_id,
        )

    async def upload_file_streaming(
        self,
        user_id: str,
        filename: str,
        content_iterator: AsyncIterator[bytes],
        content_type: str,
        max_size_bytes: int = 50 * 1024 * 1024,  # 50MB default
        metadata: dict[str, str] | None = None,
        file_id: str | None = None,
        tenant_id: str | None = None,
    ) -> FileInfo:
        """
        Upload a file using streaming to handle large files.

        For cloud backends (S3/OSS), this collects chunks and uploads.
        For local backend, streams directly to disk.

        Args:
            user_id: User ID
            filename: Original filename
            content_iterator: Async iterator yielding file chunks
            content_type: MIME type
            max_size_bytes: Maximum allowed file size
            metadata: Optional metadata
            file_id: Optional file ID

        Returns:
            FileInfo with upload details

        Raises:
            ValueError: If file exceeds max size
        """
        # Validate user_id
        self._validate_scope_id(user_id, "user_id")
        if tenant_id:
            self._validate_scope_id(tenant_id, "tenant_id")

        ext = Path(filename).suffix.lower() or ".bin"

        if not file_id:
            file_id = self.generate_file_id()

        storage_key, safe_filename = self._generate_key(
            user_id,
            file_id,
            ext,
            tenant_id=tenant_id,
        )

        # For local storage, stream directly to disk
        if self.config.backend == StorageBackend.LOCAL:
            return await self._upload_local_streaming(
                user_id=user_id,
                file_id=file_id,
                filename=filename,
                safe_filename=safe_filename,
                storage_key=storage_key,
                content_iterator=content_iterator,
                content_type=content_type,
                max_size_bytes=max_size_bytes,
                metadata=metadata,
                tenant_id=tenant_id,
            )

        # For cloud storage, collect chunks then upload
        chunks = []
        total_size = 0

        async for chunk in content_iterator:
            total_size += len(chunk)
            if total_size > max_size_bytes:
                raise ValueError(
                    f"File exceeds maximum size of {max_size_bytes / 1024 / 1024:.1f}MB"
                )
            chunks.append(chunk)

        content = b"".join(chunks)

        return await self.upload_file(
            user_id=user_id,
            filename=filename,
            content=content,
            content_type=content_type,
            metadata=metadata,
            file_id=file_id,
            tenant_id=tenant_id,
        )

    async def _upload_local_streaming(
        self,
        user_id: str,
        file_id: str,
        filename: str,
        safe_filename: str,
        storage_key: str,
        content_iterator: AsyncIterator[bytes],
        content_type: str,
        max_size_bytes: int,
        metadata: dict[str, str] | None = None,
        tenant_id: str | None = None,
    ) -> FileInfo:
        """Stream upload to local filesystem."""
        assert isinstance(self._backend, LocalStorageBackend)

        full_path = self._backend._get_full_path(storage_key)
        full_path.parent.mkdir(parents=True, exist_ok=True)

        temp_path = full_path.with_suffix(full_path.suffix + ".tmp")
        total_size = 0
        hasher = hashlib.md5()

        try:
            with open(temp_path, "wb") as f:
                async for chunk in content_iterator:
                    total_size += len(chunk)
                    if total_size > max_size_bytes:
                        f.close()
                        temp_path.unlink(missing_ok=True)
                        raise ValueError(
                            f"File exceeds maximum size of {max_size_bytes / 1024 / 1024:.1f}MB"
                        )
                    hasher.update(chunk)
                    await asyncio.to_thread(f.write, chunk)

            # Atomic rename
            await asyncio.to_thread(temp_path.rename, full_path)

            # Write metadata
            file_metadata = {
                "user_id": user_id,
                "file_id": file_id,
                "original_filename": filename,
                "content_hash": hasher.hexdigest(),
                "content_type": content_type,
            }
            if metadata:
                file_metadata.update(metadata)

            import json

            meta_path = full_path.with_suffix(full_path.suffix + ".meta")
            await asyncio.to_thread(meta_path.write_text, json.dumps(file_metadata))

        except Exception:
            temp_path.unlink(missing_ok=True)
            raise

        uploaded_at = datetime.now(timezone.utc)

        logger.info(
            f"[FileStorage] Streamed {filename} ({total_size} bytes) "
            f"for user {user_id} -> {storage_key}"
        )

        return FileInfo(
            file_id=file_id,
            user_id=user_id,
            filename=safe_filename,
            storage_key=storage_key,
            size_bytes=total_size,
            content_type=content_type,
            uploaded_at=uploaded_at,
            metadata=file_metadata,
            tenant_id=tenant_id,
        )

    async def download_file(self, storage_key: str) -> bytes:
        """
        Download a file from storage.

        Args:
            storage_key: Full storage key

        Returns:
            File binary content
        """
        return await self._backend.download(storage_key)

    async def delete_file(self, storage_key: str) -> bool:
        """
        Delete a file from storage.

        Args:
            storage_key: Full storage key

        Returns:
            True if deleted successfully
        """
        result = await self._backend.delete(storage_key)
        if result:
            logger.info(
                "File storage operation completed (operation=delete, key_hash=%s)",
                storage_key_log_hash(storage_key),
            )
        return result

    async def delete_user_files(self, user_id: str, tenant_id: str | None = None) -> int:
        """
        Delete all files for a user.

        Args:
            user_id: User ID

        Returns:
            Number of deleted files
        """
        prefix = (
            f"{self.KEY_PREFIX}/{tenant_id}/{user_id}/"
            if tenant_id
            else f"{self.KEY_PREFIX}/{user_id}/"
        )
        deleted = await self._backend.delete_prefix(prefix)
        logger.info(f"[FileStorage] Deleted {deleted} files for user {user_id}")
        return deleted

    async def delete_by_prefix(self, prefix: str) -> int:
        """
        Delete all files matching a prefix.

        This is a public API for deleting files by prefix pattern.

        Args:
            prefix: Storage key prefix (e.g., "uploads/user123/abc12345_")

        Returns:
            Number of deleted files
        """
        deleted = await self._backend.delete_prefix(prefix)
        if deleted > 0:
            logger.info(f"[FileStorage] Deleted {deleted} files with prefix {prefix}")
        return deleted

    async def file_exists(self, storage_key: str) -> bool:
        """
        Check if a file exists.

        Args:
            storage_key: Full storage key

        Returns:
            True if file exists
        """
        return await self._backend.exists(storage_key)

    def get_url(self, storage_key: str, expiry_seconds: int = 3600) -> str:
        """
        Get URL for accessing a file.

        Args:
            storage_key: Full storage key
            expiry_seconds: URL expiry time

        Returns:
            File URL
        """
        return self._backend.get_url(storage_key, expiry_seconds)

    async def get_download_url(
        self,
        storage_key: str,
        expiry_seconds: int = 3600,
        filename: str | None = None,
    ) -> str | None:
        """
        Get presigned download URL for a file.

        Args:
            storage_key: Full storage key
            expiry_seconds: URL expiry time
            filename: Optional filename for Content-Disposition

        Returns:
            Presigned URL or direct URL
        """
        # S3 backend supports presigned URLs
        if self.config.backend == StorageBackend.S3 and isinstance(self._backend, S3StorageBackend):
            return await self._backend.generate_presigned_download_url(
                    key=storage_key,
                    expiry_seconds=expiry_seconds,
                    filename=filename,
                )

        # For other backends, return direct URL
        return self.get_url(storage_key, expiry_seconds)

    async def generate_presigned_upload_url(
        self,
        user_id: str,
        filename: str,
        content_type: str,
        expiry_seconds: int = 900,
        metadata: dict[str, str] | None = None,
    ) -> dict[str, str] | None:
        """
        Generate presigned URL for direct client upload.

        Allows frontend to upload directly to S3/OSS without
        going through backend.

        Args:
            user_id: User ID
            filename: Original filename
            content_type: MIME type
            expiry_seconds: URL expiry time
            metadata: Optional metadata

        Returns:
            Dict with upload URL and instructions, or None if not supported
        """
        ext = Path(filename).suffix.lower() or ".bin"
        file_id = self.generate_file_id()
        storage_key, safe_filename = self._generate_key(user_id, file_id, ext)

        # Add standard metadata
        full_metadata = {
            "user-id": user_id,
            "file-id": file_id,
            "original-filename": filename,
        }
        if metadata:
            full_metadata.update(metadata)

        result = await self._backend.generate_presigned_upload_url(
            key=storage_key,
            content_type=content_type,
            expiry_seconds=expiry_seconds,
            metadata=full_metadata,
        )

        if result:
            result["file_id"] = file_id
            result["storage_key"] = storage_key
            result["filename"] = safe_filename
            logger.info(
                f"[FileStorage] Generated presigned upload URL for user {user_id} -> {storage_key}"
            )

        return result

    def supports_presigned_urls(self) -> bool:
        """Check if backend supports presigned URLs."""
        return self.config.backend == StorageBackend.S3

    def get_local_path(self, storage_key: str) -> Path | None:
        """
        Get local filesystem path for a file.

        Only works for local storage backend.

        Args:
            storage_key: Full storage key

        Returns:
            Path to the file or None if not local storage
        """
        if self.config.backend == StorageBackend.LOCAL and isinstance(self._backend, LocalStorageBackend):
            return self._backend._get_full_path(storage_key)
        return None

    async def close(self) -> None:
        """Close the storage service and release resources."""
        if self._backend is not None:
            await self._backend.close()

    async def __aenter__(self) -> FileStorageService:
        """Async context manager entry."""
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb) -> None:
        """Async context manager exit."""
        await self.close()


# ============ Global Instance Management ============

_file_storage_service: FileStorageService | None = None


def get_file_storage() -> FileStorageService:
    """
    Get the global file storage service instance.

    Returns:
        FileStorageService instance

    Raises:
        RuntimeError: If service not initialized
    """
    global _file_storage_service
    if _file_storage_service is None:
        raise RuntimeError("FileStorageService not initialized. Call init_file_storage() first.")
    return _file_storage_service


def init_file_storage(config: StorageConfig | None = None) -> FileStorageService:
    """
    Initialize the global file storage service.

    Reads configuration from environment variables if not provided:
    - FILE_STORAGE_BACKEND: "s3", "oss", or "local" (default: "local")
    - FILE_STORAGE_S3_BUCKET, FILE_STORAGE_S3_REGION, etc.
    - FILE_STORAGE_OSS_BUCKET, FILE_STORAGE_OSS_ENDPOINT, etc.
    - FILE_STORAGE_LOCAL_PATH (default: "./uploads")

    Args:
        config: Optional StorageConfig override

    Returns:
        Initialized FileStorageService
    """
    global _file_storage_service

    if config is None:
        # Read from environment
        backend_str = os.getenv("FILE_STORAGE_BACKEND", "local").lower()
        backend = (
            StorageBackend(backend_str)
            if backend_str in ("s3", "oss", "local")
            else StorageBackend.LOCAL
        )

        config = StorageConfig(
            backend=backend,
            # S3 config
            s3_bucket=os.getenv("FILE_STORAGE_S3_BUCKET", ""),
            s3_region=os.getenv("FILE_STORAGE_S3_REGION", "us-east-1"),
            s3_access_key=os.getenv("FILE_STORAGE_S3_ACCESS_KEY", ""),
            s3_secret_key=os.getenv("FILE_STORAGE_S3_SECRET_KEY", ""),
            s3_endpoint_url=os.getenv("FILE_STORAGE_S3_ENDPOINT_URL"),
            # OSS config
            oss_bucket=os.getenv("FILE_STORAGE_OSS_BUCKET", ""),
            oss_endpoint=os.getenv("FILE_STORAGE_OSS_ENDPOINT", ""),
            oss_access_key=os.getenv("FILE_STORAGE_OSS_ACCESS_KEY", ""),
            oss_secret_key=os.getenv("FILE_STORAGE_OSS_SECRET_KEY", ""),
            # Local config
            local_base_path=os.getenv("FILE_STORAGE_PATH", "./uploads"),
        )

    _file_storage_service = FileStorageService(config)

    logger.info(
        f"[FileStorage] Initialized with backend={config.backend.value}, "
        f"path={config.local_base_path if config.backend == StorageBackend.LOCAL else config.s3_bucket or config.oss_bucket}"
    )

    return _file_storage_service


async def shutdown_file_storage() -> None:
    """Shutdown the global file storage service."""
    global _file_storage_service
    if _file_storage_service is not None:
        await _file_storage_service.close()
        _file_storage_service = None
        logger.info("[FileStorage] Shutdown complete")
