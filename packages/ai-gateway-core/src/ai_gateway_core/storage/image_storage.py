"""
Image Storage Service.

Provides unified interface for storing and retrieving images from various backends:
- S3 (Amazon Web Services)
- OSS (Alibaba Cloud Object Storage Service)
- Local filesystem (for development)

Used for storing Confluence images before multimodal embedding.
"""

from __future__ import annotations

import asyncio
import hashlib
import hmac
import logging
import shutil
import time
import unicodedata
from abc import ABC, abstractmethod
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, quote, urlencode, urlparse, urlunparse

from ai_gateway_core.logging import record_internal_exception

logger = logging.getLogger(__name__)


def storage_key_log_hash(storage_key: str) -> str:
    """Return a stable opaque identifier for logs without exposing object paths."""

    return hashlib.sha256(storage_key.encode("utf-8", errors="replace")).hexdigest()


def _sanitize_for_s3_metadata(value: str) -> str:
    """
    Sanitize a string for S3 metadata (ASCII only).

    S3 metadata can only contain ASCII characters. This function:
    1. Normalizes unicode to ASCII equivalents where possible
    2. Removes non-ASCII characters that can't be normalized
    3. Replaces special whitespace characters with regular spaces
    """
    if not value:
        return value
    # Normalize unicode (e.g., convert special spaces to regular spaces)
    normalized = unicodedata.normalize("NFKD", value)
    # Keep only ASCII characters
    ascii_str = normalized.encode("ascii", "ignore").decode("ascii")
    # Clean up multiple spaces
    return " ".join(ascii_str.split())


class StorageBackend(str, Enum):
    """Supported storage backends"""

    S3 = "s3"
    OSS = "oss"
    LOCAL = "local"


@dataclass
class ImageUploadParams:
    """Parameters for batch image upload"""

    tenant_id: str
    document_id: str
    attachment_id: str
    filename: str
    content: bytes
    content_type: str
    metadata: dict[str, str] | None = None


@dataclass(frozen=True)
class StorageObjectInfo:
    """Bounded object metadata used before downloading untrusted uploads."""

    size_bytes: int
    content_type: str | None = None
    metadata: dict[str, str] | None = None


@dataclass
class StorageConfig:
    """Storage configuration"""

    backend: StorageBackend = StorageBackend.LOCAL

    # S3 configuration
    s3_bucket: str = ""
    s3_region: str = "us-east-1"
    s3_access_key: str = ""
    s3_secret_key: str = ""
    s3_endpoint_url: str | None = None  # For S3-compatible services

    # OSS configuration
    oss_bucket: str = ""
    oss_endpoint: str = ""
    oss_access_key: str = ""
    oss_secret_key: str = ""

    # Local storage configuration
    local_base_path: str = "./data/images"

    # Common settings
    url_expiry_seconds: int = 3600  # Pre-signed URL expiry

    # Environment key prefix (e.g., "dev", "staging", "prod")
    # Prepended to all storage keys for environment isolation
    key_prefix: str = ""

    @classmethod
    def from_env(cls) -> StorageConfig:
        """Build a StorageConfig from GATEWAY_STORAGE__* env vars.

        AS and other services that need storage but don't want to drag the gateway
        Settings tree should call this. Reads the same env contract as
        ``src/main.py``'s storage init block.
        """
        import os

        backend_str = os.getenv("GATEWAY_STORAGE__BACKEND", "local").lower()
        try:
            backend = StorageBackend(backend_str)
        except ValueError:
            backend = StorageBackend.LOCAL

        return cls(
            backend=backend,
            s3_bucket=os.getenv("GATEWAY_STORAGE__S3__BUCKET", ""),
            s3_region=os.getenv("GATEWAY_STORAGE__S3__REGION", "us-east-1"),
            s3_access_key=os.getenv("GATEWAY_STORAGE__S3__ACCESS_KEY", ""),
            s3_secret_key=os.getenv("GATEWAY_STORAGE__S3__SECRET_KEY", ""),
            s3_endpoint_url=os.getenv("GATEWAY_STORAGE__S3__ENDPOINT_URL") or None,
            oss_bucket=os.getenv("GATEWAY_STORAGE__OSS__BUCKET", ""),
            oss_endpoint=os.getenv("GATEWAY_STORAGE__OSS__ENDPOINT", ""),
            oss_access_key=os.getenv("GATEWAY_STORAGE__OSS__ACCESS_KEY", ""),
            oss_secret_key=os.getenv("GATEWAY_STORAGE__OSS__SECRET_KEY", ""),
            local_base_path=os.getenv("GATEWAY_STORAGE__LOCAL_BASE_PATH", "./data/artifacts"),
            url_expiry_seconds=int(os.getenv("GATEWAY_STORAGE__URL_EXPIRY_SECONDS", "3600")),
            key_prefix=os.getenv("GATEWAY_STORAGE__KEY_PREFIX", ""),
        )


class BaseStorageBackend(ABC):
    """Abstract base class for storage backends"""

    def __init__(self, key_prefix: str = ""):
        self._key_prefix = key_prefix.strip("/") if key_prefix else ""

    def _prefixed_key(self, key: str) -> str:
        """Prepend environment prefix to storage key with path traversal protection."""
        # Sanitize key to prevent path traversal
        # Remove leading slashes and parent directory references
        sanitized = key.lstrip("/")
        # Replace any '..' sequences to prevent directory traversal
        while ".." in sanitized:
            sanitized = sanitized.replace("..", "_")
        # Prevent null byte injection
        sanitized = sanitized.replace("\x00", "")

        if self._key_prefix and (
            sanitized == self._key_prefix
            or sanitized.startswith(f"{self._key_prefix}/")
        ):
            return sanitized
        if self._key_prefix:
            return f"{self._key_prefix}/{sanitized}"
        return sanitized

    @abstractmethod
    async def upload(
        self,
        key: str,
        content: bytes,
        content_type: str,
        metadata: dict[str, str] | None = None,
    ) -> str:
        """
        Upload content to storage.

        Args:
            key: Storage key (path)
            content: Binary content
            content_type: MIME type
            metadata: Optional metadata

        Returns:
            URL or key for accessing the content
        """
        pass

    @abstractmethod
    async def download(self, key: str) -> bytes:
        """
        Download content from storage.

        Args:
            key: Storage key

        Returns:
            Binary content
        """
        pass

    async def download_to_path(self, key: str, target_path: str) -> str:
        """
        Download content and write it to a local file path.

        Backends can override for streaming efficiency.
        """
        try:
            import aiofiles

            data = await self.download(key)
            path = Path(target_path)
            path.parent.mkdir(parents=True, exist_ok=True)
            async with aiofiles.open(path, "wb") as handle:
                await handle.write(data)
            return str(path)
        except ImportError:
            # Fallback to sync I/O if aiofiles not available
            data = await self.download(key)
            path = Path(target_path)
            path.parent.mkdir(parents=True, exist_ok=True)
            await asyncio.to_thread(path.write_bytes, data)
            return str(path)

    @abstractmethod
    async def delete(self, key: str) -> bool:
        """
        Delete content from storage.

        Args:
            key: Storage key

        Returns:
            True if deleted, False otherwise
        """
        pass

    @abstractmethod
    async def delete_prefix(self, prefix: str) -> int:
        """
        Delete all objects with a given prefix.

        Args:
            prefix: Key prefix

        Returns:
            Number of deleted objects
        """
        pass

    @abstractmethod
    async def exists(self, key: str) -> bool:
        """
        Check if object exists.

        Args:
            key: Storage key

        Returns:
            True if exists
        """
        pass

    @abstractmethod
    def get_url(self, key: str, expiry_seconds: int = 3600) -> str:
        """
        Get URL for accessing the content.

        Args:
            key: Storage key
            expiry_seconds: URL expiry time for pre-signed URLs

        Returns:
            URL for accessing the content
        """
        pass

    async def generate_presigned_upload_url(
        self,
        key: str,
        content_type: str,
        expiry_seconds: int = 900,
        metadata: dict[str, str] | None = None,
    ) -> dict[str, str] | None:
        """
        Generate a presigned URL for direct upload (client-side upload).

        This allows clients to upload directly to storage without going through
        the backend, reducing server load and enabling larger file uploads.

        Args:
            key: Storage key (path) where the file will be stored
            content_type: Expected MIME type of the file
            expiry_seconds: URL expiry time in seconds (default 15 minutes)
            metadata: Optional metadata to attach to the object

        Returns:
            Dictionary with 'url' and optional 'fields' for POST/PUT upload,
            or None if presigned URLs are not supported by this backend.
        """
        del key, content_type, expiry_seconds, metadata
        return None  # Default: not supported

    async def head(self, key: str) -> StorageObjectInfo | None:
        """Return object metadata without reading the object body when supported."""

        if not await self.exists(key):
            return None
        return None

    async def close(self) -> None:
        """
        Close and cleanup resources.

        Override in subclasses that need cleanup.
        """
        return None


class LocalStorageBackend(BaseStorageBackend):
    """Local filesystem storage backend for development"""

    def __init__(self, base_path: str, key_prefix: str = ""):
        super().__init__(key_prefix)
        self.base_path = Path(base_path)
        self.base_path.mkdir(parents=True, exist_ok=True)

    def _get_full_path(self, key: str) -> Path:
        """Get full path for a key, with path traversal protection"""
        full = (self.base_path / key).resolve()
        if not str(full).startswith(str(self.base_path.resolve())):
            raise ValueError(f"Path traversal detected: {key}")
        return full

    async def upload(
        self,
        key: str,
        content: bytes,
        content_type: str,
        metadata: dict[str, str] | None = None,
    ) -> str:
        pkey = self._prefixed_key(key)
        full_path = self._get_full_path(pkey)
        full_path.parent.mkdir(parents=True, exist_ok=True)

        # Write content
        await asyncio.to_thread(full_path.write_bytes, content)

        # Store metadata in a sidecar file
        if metadata:
            import json

            meta_path = full_path.with_suffix(full_path.suffix + ".meta")
            await asyncio.to_thread(
                meta_path.write_text, json.dumps({"content_type": content_type, **metadata})
            )

        logger.debug(f"Uploaded {len(content)} bytes to local: {pkey}")
        return f"file://{full_path.absolute()}"

    async def download(self, key: str) -> bytes:
        pkey = self._prefixed_key(key)
        full_path = self._get_full_path(pkey)
        if not full_path.exists():
            raise FileNotFoundError(f"File not found: {pkey}")
        return await asyncio.to_thread(full_path.read_bytes)

    async def download_to_path(self, key: str, target_path: str) -> str:
        pkey = self._prefixed_key(key)
        full_path = self._get_full_path(pkey)
        if not full_path.exists():
            raise FileNotFoundError(f"File not found: {pkey}")
        target = Path(target_path)
        target.parent.mkdir(parents=True, exist_ok=True)
        await asyncio.to_thread(shutil.copyfile, full_path, target)
        return str(target)

    async def delete(self, key: str) -> bool:
        pkey = self._prefixed_key(key)
        full_path = self._get_full_path(pkey)
        if full_path.exists():
            await asyncio.to_thread(full_path.unlink)
            # Also delete metadata file if exists
            meta_path = full_path.with_suffix(full_path.suffix + ".meta")
            if meta_path.exists():
                await asyncio.to_thread(meta_path.unlink)
            return True
        return False

    async def delete_prefix(self, prefix: str) -> int:
        pprefix = self._prefixed_key(prefix)
        prefix_path = self._get_full_path(pprefix)

        # If prefix ends with /, treat it as a directory and delete all files inside
        if prefix.endswith("/"):
            if not prefix_path.exists():
                return 0
            deleted = 0
            for path in prefix_path.rglob("*"):
                if path.is_file():
                    await asyncio.to_thread(path.unlink)
                    # Don't count .meta sidecar files
                    if not path.name.endswith(".meta"):
                        deleted += 1
            return deleted

        # Otherwise, match files starting with the prefix name
        if not prefix_path.parent.exists():
            return 0

        deleted = 0
        for path in prefix_path.parent.glob(f"{prefix_path.name}*"):
            if path.is_file():
                await asyncio.to_thread(path.unlink)
                # Don't count .meta sidecar files
                if not path.name.endswith(".meta"):
                    deleted += 1
        return deleted

    async def exists(self, key: str) -> bool:
        return self._get_full_path(self._prefixed_key(key)).exists()

    async def head(self, key: str) -> StorageObjectInfo | None:
        full_path = self._get_full_path(self._prefixed_key(key))
        if not full_path.is_file():
            return None
        size_bytes = (await asyncio.to_thread(full_path.stat)).st_size
        content_type: str | None = None
        metadata: dict[str, str] = {}
        meta_path = full_path.with_suffix(full_path.suffix + ".meta")
        if meta_path.is_file():
            import json

            try:
                raw = await asyncio.to_thread(meta_path.read_text)
                value = json.loads(raw)
                if isinstance(value, dict):
                    content_type = (
                        str(value.get("content_type")) if value.get("content_type") else None
                    )
                    metadata = {
                        str(name): str(item)
                        for name, item in value.items()
                        if name != "content_type"
                    }
            except (OSError, ValueError, TypeError):
                logger.warning(
                    "Local storage metadata unreadable (key_hash=%s)",
                    storage_key_log_hash(key),
                )
        return StorageObjectInfo(
            size_bytes=size_bytes,
            content_type=content_type,
            metadata=metadata,
        )

    def get_url(self, key: str, expiry_seconds: int = 3600) -> str:
        del expiry_seconds
        full_path = self._get_full_path(self._prefixed_key(key))
        return f"file://{full_path.absolute()}"


class S3StorageBackend(BaseStorageBackend):
    """Amazon S3 storage backend"""

    def __init__(
        self,
        bucket: str,
        region: str,
        access_key: str,
        secret_key: str,
        endpoint_url: str | None = None,
        key_prefix: str = "",
    ):
        super().__init__(key_prefix)
        self.bucket = bucket
        self.region = region
        self.access_key = access_key
        self.secret_key = secret_key
        self.endpoint_url = endpoint_url
        self._client = None
        self._client_context = None

    async def _get_client(self):
        """Get or create S3 client"""
        if self._client is None:
            try:
                import aioboto3
            except ImportError:
                raise ImportError(
                    "aioboto3 is required for S3 storage. Install with: pip install aioboto3"
                )

            session = aioboto3.Session()
            self._client_context = session.client(
                "s3",
                region_name=self.region,
                aws_access_key_id=self.access_key,
                aws_secret_access_key=self.secret_key,
                endpoint_url=self.endpoint_url,
            )
            self._client = await self._client_context.__aenter__()
        return self._client

    async def close(self):
        """Close the S3 client and release resources"""
        if self._client_context is not None:
            try:
                await self._client_context.__aexit__(None, None, None)
            except Exception as e:
                logger.warning(f"Error closing S3 client: {e}")
            finally:
                self._client = None
                self._client_context = None

    async def upload(
        self,
        key: str,
        content: bytes,
        content_type: str,
        metadata: dict[str, str] | None = None,
    ) -> str:
        pkey = self._prefixed_key(key)
        client = await self._get_client()
        extra_args = {"ContentType": content_type}
        if metadata:
            # Sanitize metadata values to ASCII-only for S3 compatibility
            extra_args["Metadata"] = {
                _sanitize_for_s3_metadata(k): _sanitize_for_s3_metadata(v)
                for k, v in metadata.items()
            }

        await client.put_object(
            Bucket=self.bucket,
            Key=pkey,
            Body=content,
            **extra_args,
        )

        logger.debug(f"Uploaded {len(content)} bytes to S3: {pkey}")
        return self.get_url(key)

    async def download(self, key: str) -> bytes:
        pkey = self._prefixed_key(key)
        client = await self._get_client()
        try:
            logger.debug(f"[S3] Downloading key={pkey} from bucket={self.bucket}")
            response = await client.get_object(Bucket=self.bucket, Key=pkey)
            async with response["Body"] as stream:
                data = await stream.read()
                logger.debug(f"[S3] Downloaded {len(data)} bytes from key={pkey}")
                return data
        except client.exceptions.NoSuchKey:
            logger.error(f"[S3] Key not found: bucket={self.bucket}, key={pkey}")
            raise FileNotFoundError(f"S3 object not found: {pkey}")
        except Exception as e:
            logger.error(f"[S3] Download failed: bucket={self.bucket}, key={pkey}, error={e}")
            raise

    async def download_to_path(self, key: str, target_path: str) -> str:
        pkey = self._prefixed_key(key)
        client = await self._get_client()
        target = Path(target_path)
        target.parent.mkdir(parents=True, exist_ok=True)
        try:
            logger.debug(f"[S3] Streaming download key={pkey} to {target}")
            response = await client.get_object(Bucket=self.bucket, Key=pkey)

            async def _read_chunk(stream, size: int) -> bytes:
                if hasattr(stream, "content") and hasattr(stream.content, "read"):
                    return await stream.content.read(size)
                try:
                    return await stream.read(size)
                except TypeError:
                    return await stream.read()

            # Use aiofiles for non-blocking file I/O
            try:
                import aiofiles

                async with response["Body"] as stream, aiofiles.open(target, "wb") as handle:
                    while True:
                        chunk = await _read_chunk(stream, 1024 * 1024)
                        if not chunk:
                            break
                        await handle.write(chunk)
            except ImportError:
                # Fallback to sync I/O
                async with response["Body"] as stream:
                    with open(target, "wb") as handle:
                        while True:
                            chunk = await _read_chunk(stream, 1024 * 1024)
                            if not chunk:
                                break
                            handle.write(chunk)
            return str(target)
        except client.exceptions.NoSuchKey:
            logger.error(f"[S3] Key not found: bucket={self.bucket}, key={pkey}")
            raise FileNotFoundError(f"S3 object not found: {pkey}")
        except Exception as e:
            logger.error(
                f"[S3] Streaming download failed: bucket={self.bucket}, key={pkey}, error={e}"
            )
            raise

    async def delete(self, key: str) -> bool:
        pkey = self._prefixed_key(key)
        client = await self._get_client()
        try:
            await client.delete_object(Bucket=self.bucket, Key=pkey)
            return True
        except Exception as exc:
            record_internal_exception(
                logger,
                "assistant.storage.s3_delete_failed",
                exc,
                level=logging.WARNING,
            )
            logger.warning(
                "Storage object delete failed (provider=s3, operation=delete, key_hash=%s)",
                storage_key_log_hash(pkey),
            )
            return False

    async def delete_prefix(self, prefix: str) -> int:
        pprefix = self._prefixed_key(prefix)
        client = await self._get_client()
        deleted = 0

        paginator = client.get_paginator("list_objects_v2")
        async for page in paginator.paginate(Bucket=self.bucket, Prefix=pprefix):
            objects = page.get("Contents", [])
            if not objects:
                continue

            delete_keys = [{"Key": obj["Key"]} for obj in objects]
            response = await client.delete_objects(
                Bucket=self.bucket,
                Delete={"Objects": delete_keys},
            )
            errors = response.get("Errors", []) if isinstance(response, dict) else []
            if errors:
                raise RuntimeError(
                    "S3 prefix deletion was only partially applied; "
                    f"{len(errors)} object(s) remain retryable"
                )
            deleted += len(delete_keys)

        return deleted

    async def exists(self, key: str) -> bool:
        pkey = self._prefixed_key(key)
        client = await self._get_client()
        try:
            await client.head_object(Bucket=self.bucket, Key=pkey)
            return True
        except Exception as exc:
            response = getattr(exc, "response", None)
            error = response.get("Error", {}) if isinstance(response, dict) else {}
            metadata = response.get("ResponseMetadata", {}) if isinstance(response, dict) else {}
            if (
                str(error.get("Code") or "") in {"404", "NoSuchKey", "NotFound"}
                or str(metadata.get("HTTPStatusCode") or "") == "404"
            ):
                return False
            raise

    async def head(self, key: str) -> StorageObjectInfo | None:
        pkey = self._prefixed_key(key)
        client = await self._get_client()
        try:
            response = await client.head_object(Bucket=self.bucket, Key=pkey)
        except Exception as exc:
            response_data = getattr(exc, "response", None)
            error = response_data.get("Error", {}) if isinstance(response_data, dict) else {}
            metadata = (
                response_data.get("ResponseMetadata", {})
                if isinstance(response_data, dict)
                else {}
            )
            if (
                str(error.get("Code") or "") in {"404", "NoSuchKey", "NotFound"}
                or str(metadata.get("HTTPStatusCode") or "") == "404"
            ):
                return None
            raise
        return StorageObjectInfo(
            size_bytes=int(response.get("ContentLength") or 0),
            content_type=(str(response["ContentType"]) if response.get("ContentType") else None),
            metadata={str(k): str(v) for k, v in (response.get("Metadata") or {}).items()},
        )

    def get_url(self, key: str, expiry_seconds: int = 3600) -> str:
        """Get S3 URL (non-presigned for now, can be enhanced)"""
        del expiry_seconds
        pkey = self._prefixed_key(key)
        if self.endpoint_url:
            return f"{self.endpoint_url}/{self.bucket}/{pkey}"
        return f"https://{self.bucket}.s3.{self.region}.amazonaws.com/{pkey}"

    async def generate_presigned_upload_url(
        self,
        key: str,
        content_type: str,
        expiry_seconds: int = 900,
        metadata: dict[str, str] | None = None,
    ) -> dict[str, str] | None:
        """
        Generate a presigned URL for direct upload to S3.

        Uses PUT method for simple uploads. For multipart uploads with additional
        form fields, use generate_presigned_post instead.

        Args:
            key: Storage key (path) where the file will be stored
            content_type: Expected MIME type of the file
            expiry_seconds: URL expiry time in seconds (default 15 minutes)
            metadata: Optional metadata to attach to the object

        Returns:
            Dictionary with 'url', 'method', and 'headers' for PUT upload
        """
        pkey = self._prefixed_key(key)
        client = await self._get_client()

        # Build params for presigned URL
        params = {
            "Bucket": self.bucket,
            "Key": pkey,
            "ContentType": content_type,
        }

        # Add metadata if provided
        if metadata:
            params["Metadata"] = {
                _sanitize_for_s3_metadata(k): _sanitize_for_s3_metadata(v)
                for k, v in metadata.items()
            }

        try:
            # Generate presigned PUT URL
            presigned_url = await client.generate_presigned_url(
                "put_object",
                Params=params,
                ExpiresIn=expiry_seconds,
            )

            return {
                "url": presigned_url,
                "method": "PUT",
                "headers": {
                    "Content-Type": content_type,
                },
                "key": key,
                "storage_key": pkey,
                "bucket": self.bucket,
                "expiry_seconds": expiry_seconds,
            }
        except Exception as e:
            logger.error(f"Failed to generate presigned URL for {pkey}: {e}")
            return None

    async def generate_presigned_download_url(
        self,
        key: str,
        expiry_seconds: int = 3600,
        filename: str | None = None,
    ) -> str | None:
        """
        Generate a presigned URL for downloading from S3.

        Args:
            key: Storage key
            expiry_seconds: URL expiry time in seconds
            filename: Optional filename for Content-Disposition header

        Returns:
            Presigned download URL or None if failed
        """
        import urllib.parse

        pkey = self._prefixed_key(key)
        client = await self._get_client()

        params = {
            "Bucket": self.bucket,
            "Key": pkey,
        }

        if filename:
            # RFC 5987: Use URL-encoded filename for non-ASCII characters
            # Format: filename="ascii_fallback"; filename*=UTF-8''url_encoded_name
            try:
                # Check if filename contains non-ASCII characters
                filename.encode("ascii")
                # Pure ASCII filename - use simple format
                params["ResponseContentDisposition"] = f'attachment; filename="{filename}"'
            except UnicodeEncodeError:
                # Non-ASCII filename - use RFC 5987 format with UTF-8 encoding
                # Create ASCII fallback (replace non-ASCII with underscore)
                ascii_fallback = (
                    filename.encode("ascii", "replace").decode("ascii").replace("?", "_")
                )
                # URL-encode the UTF-8 filename
                encoded_filename = urllib.parse.quote(filename, safe="")
                params["ResponseContentDisposition"] = (
                    f'attachment; filename="{ascii_fallback}"; '
                    f"filename*=UTF-8''{encoded_filename}"
                )

        try:
            presigned_url = await client.generate_presigned_url(
                "get_object",
                Params=params,
                ExpiresIn=expiry_seconds,
            )
            return presigned_url
        except Exception as e:
            logger.error(f"Failed to generate presigned download URL for {key}: {e}")
            return None


class OSSStorageBackend(BaseStorageBackend):
    """Alibaba Cloud OSS storage backend"""

    def __init__(
        self,
        bucket: str,
        endpoint: str,
        access_key: str,
        secret_key: str,
        key_prefix: str = "",
    ):
        super().__init__(key_prefix)
        self.bucket_name = bucket
        self.endpoint = endpoint
        self.access_key = access_key
        self.secret_key = secret_key
        self._bucket = None

    def _get_bucket(self):
        """Get OSS bucket instance"""
        if self._bucket is None:
            try:
                import oss2
            except ImportError:
                raise ImportError(
                    "oss2 is required for OSS storage. Install with: pip install oss2"
                )

            auth = oss2.Auth(self.access_key, self.secret_key)
            self._bucket = oss2.Bucket(auth, self.endpoint, self.bucket_name)
        return self._bucket

    async def upload(
        self,
        key: str,
        content: bytes,
        content_type: str,
        metadata: dict[str, str] | None = None,
    ) -> str:
        pkey = self._prefixed_key(key)
        bucket = self._get_bucket()

        headers = {"Content-Type": content_type}
        if metadata:
            for k, v in metadata.items():
                headers[f"x-oss-meta-{k}"] = v

        await asyncio.to_thread(bucket.put_object, pkey, content, headers=headers)

        logger.debug(f"Uploaded {len(content)} bytes to OSS: {pkey}")
        return self.get_url(key)

    async def download(self, key: str) -> bytes:
        pkey = self._prefixed_key(key)
        bucket = self._get_bucket()
        try:
            logger.debug(f"[OSS] Downloading key={pkey} from bucket={self.bucket_name}")
            result = await asyncio.to_thread(bucket.get_object, pkey)
            data = await asyncio.to_thread(result.read)
            logger.debug(f"[OSS] Downloaded {len(data)} bytes from key={pkey}")
            return data
        except Exception as e:
            error_str = str(e)
            if "NoSuchKey" in error_str or "404" in error_str:
                logger.error(f"[OSS] Key not found: bucket={self.bucket_name}, key={pkey}")
                raise FileNotFoundError(f"OSS object not found: {pkey}")
            logger.error(f"[OSS] Download failed: bucket={self.bucket_name}, key={pkey}, error={e}")
            raise

    async def download_to_path(self, key: str, target_path: str) -> str:
        pkey = self._prefixed_key(key)
        bucket = self._get_bucket()
        target = Path(target_path)
        target.parent.mkdir(parents=True, exist_ok=True)
        try:
            logger.debug(f"[OSS] Streaming download key={pkey} to {target}")
            await asyncio.to_thread(bucket.get_object_to_file, pkey, str(target))
            return str(target)
        except Exception as e:
            error_str = str(e)
            if "NoSuchKey" in error_str or "404" in error_str:
                logger.error(f"[OSS] Key not found: bucket={self.bucket_name}, key={pkey}")
                raise FileNotFoundError(f"OSS object not found: {pkey}")
            logger.error(
                f"[OSS] Streaming download failed: bucket={self.bucket_name}, key={pkey}, error={e}"
            )
            raise

    async def delete(self, key: str) -> bool:
        pkey = self._prefixed_key(key)
        bucket = self._get_bucket()
        try:
            await asyncio.to_thread(bucket.delete_object, pkey)
            return True
        except Exception as exc:
            record_internal_exception(
                logger,
                "assistant.storage.oss_delete_failed",
                exc,
                level=logging.WARNING,
            )
            logger.warning(
                "Storage object delete failed (provider=oss, operation=delete, key_hash=%s)",
                storage_key_log_hash(pkey),
            )
            return False

    async def delete_prefix(self, prefix: str) -> int:
        pprefix = self._prefixed_key(prefix)
        bucket = self._get_bucket()
        deleted = 0

        # Import oss2 here since it's lazy-loaded
        try:
            import oss2
        except ImportError:
            raise ImportError("oss2 is required for OSS storage. Install with: pip install oss2")

        def list_objects():
            return list(oss2.ObjectIterator(bucket, prefix=pprefix))

        for obj in await asyncio.to_thread(list_objects):
            await asyncio.to_thread(bucket.delete_object, obj.key)
            deleted += 1

        return deleted

    async def exists(self, key: str) -> bool:
        pkey = self._prefixed_key(key)
        bucket = self._get_bucket()
        return await asyncio.to_thread(bucket.object_exists, pkey)

    async def head(self, key: str) -> StorageObjectInfo | None:
        pkey = self._prefixed_key(key)
        bucket = self._get_bucket()
        try:
            result = await asyncio.to_thread(bucket.head_object, pkey)
        except Exception as exc:
            if "NoSuchKey" in str(exc) or "404" in str(exc):
                return None
            raise
        headers = getattr(result, "headers", {}) or {}
        raw_metadata = {
            str(name)[len("x-oss-meta-") :]: str(value)
            for name, value in headers.items()
            if str(name).lower().startswith("x-oss-meta-")
        }
        content_type = getattr(result, "content_type", None)
        return StorageObjectInfo(
            size_bytes=int(result.content_length or 0),
            content_type=str(content_type) if content_type else None,
            metadata=raw_metadata,
        )

    def get_url(self, key: str, expiry_seconds: int = 3600) -> str:
        """Get OSS URL"""
        del expiry_seconds
        pkey = self._prefixed_key(key)
        return f"https://{self.bucket_name}.{self.endpoint}/{quote(pkey)}"


class ImageStorageService:
    """
    High-level image storage service.

    Provides storage operations specifically for Confluence images.
    """

    def __init__(self, config: StorageConfig, signing_key: str = ""):
        """
        Initialize the image storage service.

        Args:
            config: Storage configuration
            signing_key: Optional key for signing local file URLs (HMAC-SHA256).
                        When provided, all file:// URLs will be signed with
                        expiry timestamps for security. Recommended for production.
        """
        self.config = config
        self._signing_key = signing_key
        self._backend = self._create_backend()

    def _sign_url_if_local(self, url: str) -> str:
        """
        Sign a URL if it's a local file URL and signing is enabled.

        Args:
            url: The URL to potentially sign

        Returns:
            Signed URL if local and signing_key is configured, otherwise original URL
        """
        if not url:
            return url

        if url.startswith("file://"):
            if not self._signing_key:
                raise RuntimeError(
                    "local image URLs require a configured HMAC signing key"
                )
            parsed = urlparse(url)
            expires = int(time.time()) + int(self.config.url_expiry_seconds)
            query_params = parse_qs(parsed.query)
            query_params["expires"] = [str(expires)]
            signature = hmac.new(
                self._signing_key.encode("utf-8"),
                f"{parsed.path}:{expires}".encode(),
                hashlib.sha256,
            ).hexdigest()[:32]
            query_params["sig"] = [signature]
            return urlunparse(
                (
                    parsed.scheme,
                    parsed.netloc,
                    parsed.path,
                    parsed.params,
                    urlencode(query_params, doseq=True),
                    parsed.fragment,
                )
            )

        return url

    def _create_backend(self) -> BaseStorageBackend:
        """Create storage backend based on configuration"""
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
    def _generate_key(
        tenant_id: str,
        document_id: str,
        attachment_id: str,
        filename: str,
    ) -> str:
        """
        Generate storage key for an image.

        Structure: knowledge/confluence/{tenant_id}/{document_id}/images/{attachment_id}_{filename}
        """
        # Sanitize filename
        safe_filename = "".join(c for c in filename if c.isalnum() or c in "._-")
        return (
            f"knowledge/confluence/{tenant_id}/{document_id}/images/{attachment_id}_{safe_filename}"
        )

    async def upload_image(
        self,
        tenant_id: str,
        document_id: str,
        attachment_id: str,
        filename: str,
        content: bytes,
        content_type: str,
        metadata: dict[str, str] | None = None,
    ) -> str:
        """
        Upload an image to storage.

        Args:
            tenant_id: Tenant ID
            document_id: Document ID
            attachment_id: Confluence attachment ID
            filename: Original filename
            content: Image binary content
            content_type: MIME type (e.g., "image/png")
            metadata: Optional metadata

        Returns:
            Storage URL
        """
        key = self._generate_key(tenant_id, document_id, attachment_id, filename)

        # Add image-specific metadata
        image_metadata = {
            "tenant_id": tenant_id,
            "document_id": document_id,
            "attachment_id": attachment_id,
            "original_filename": _sanitize_for_s3_metadata(filename),
            "content_hash": hashlib.sha256(content).hexdigest(),
        }
        if metadata:
            # Sanitize all incoming metadata values to ensure S3 compatibility
            sanitized_metadata = {
                k: _sanitize_for_s3_metadata(str(v)) if isinstance(v, str) else str(v)
                for k, v in metadata.items()
            }
            image_metadata.update(sanitized_metadata)

        url = await self._backend.upload(key, content, content_type, image_metadata)

        # Sign local file URLs for security
        url = self._sign_url_if_local(url)

        logger.info(f"Uploaded image {filename} ({len(content)} bytes) -> {key}")
        return url

    async def upload_original_file(
        self,
        tenant_id: str,
        document_id: str,
        filename: str,
        content: bytes,
        content_type: str,
    ) -> str:
        """
        Upload the original document file to storage for future re-extraction.

        Args:
            tenant_id: Tenant ID
            document_id: Document ID
            filename: Original filename
            content: Raw file bytes
            content_type: MIME type (e.g., "application/pdf")

        Returns:
            Logical storage key (without environment prefix). Retrieval MUST go
            through download_original_file() which applies the prefix internally.
        """
        import urllib.parse

        safe_filename = urllib.parse.quote(filename, safe="._-")
        if not safe_filename or safe_filename == ".":
            safe_filename = "document"
        storage_key = f"knowledge/documents/{tenant_id}/{document_id}/original/{safe_filename}"

        file_metadata = {
            "tenant_id": tenant_id,
            "document_id": document_id,
            "original_filename": _sanitize_for_s3_metadata(filename),
            "content_hash": hashlib.sha256(content).hexdigest(),
        }

        await self._backend.upload(storage_key, content, content_type, file_metadata)
        logger.info(f"Uploaded original file {filename} ({len(content)} bytes) -> {storage_key}")
        return storage_key

    async def download_original_file(self, storage_key: str) -> bytes:
        """
        Download an original document file by its storage key.

        Args:
            storage_key: The storage key returned by upload_original_file

        Returns:
            Raw file bytes
        """
        return await self._backend.download(storage_key)

    async def download_document_image(
        self,
        tenant_id: str,
        document_id: str,
        storage_key: str,
    ) -> bytes:
        """Download only an image object owned by the exact document scope."""

        normalized_tenant, normalized_document = self._normalize_document_asset_scope(
            tenant_id,
            document_id,
        )
        normalized_key = str(storage_key or "").strip()
        expected_prefix = (
            f"knowledge/confluence/{normalized_tenant}/{normalized_document}/images/"
        )
        suffix = normalized_key[len(expected_prefix) :] if normalized_key.startswith(
            expected_prefix
        ) else ""
        if (
            not normalized_key
            or normalized_key != str(storage_key)
            or not suffix
            or ".." in normalized_key
            or "\\" in normalized_key
            or "\x00" in normalized_key
        ):
            raise ValueError("image storage key is outside the document asset scope")
        return await self._backend.download(normalized_key)

    async def download_original_file_to_path(self, storage_key: str, target_path: str) -> str:
        """
        Download an original document file and write it to a local path.

        Args:
            storage_key: The storage key returned by upload_original_file
            target_path: Local file path to write to

        Returns:
            The local path written
        """
        return await self._backend.download_to_path(storage_key, target_path)

    async def upload_images_batch(
        self,
        images: list[ImageUploadParams],
        max_concurrent: int = 10,
    ) -> list[str]:
        """
        Upload multiple images concurrently with rate limiting.

        Args:
            images: List of ImageUploadParams for batch upload
            max_concurrent: Maximum concurrent uploads (default: 10)

        Returns:
            List of storage URLs for each uploaded image
        """
        if not images:
            return []

        semaphore = asyncio.Semaphore(max_concurrent)

        async def upload_with_limit(params: ImageUploadParams) -> str:
            """Upload a single image with semaphore-based rate limiting."""
            async with semaphore:
                return await self.upload_image(
                    tenant_id=params.tenant_id,
                    document_id=params.document_id,
                    attachment_id=params.attachment_id,
                    filename=params.filename,
                    content=params.content,
                    content_type=params.content_type,
                    metadata=params.metadata,
                )

        # Launch all uploads concurrently (limited by semaphore)
        tasks = [upload_with_limit(params) for params in images]
        urls = await asyncio.gather(*tasks)

        logger.info(f"Batch uploaded {len(urls)} images (max_concurrent={max_concurrent})")
        return list(urls)

    async def generate_presigned_upload_url(
        self,
        tenant_id: str,
        document_id: str,
        attachment_id: str,
        filename: str,
        content_type: str,
        expiry_seconds: int = 900,
        metadata: dict[str, str] | None = None,
    ) -> dict[str, Any] | None:
        """
        Generate a presigned URL for direct upload to storage.

        This allows frontend to upload directly to S3/OSS without going through
        the backend, reducing server load and enabling larger file uploads.

        Args:
            tenant_id: Tenant ID
            document_id: Document ID
            attachment_id: Attachment ID
            filename: Original filename
            content_type: MIME type of the file
            expiry_seconds: URL expiry time (default 15 minutes)
            metadata: Optional metadata to attach

        Returns:
            Dictionary with upload URL and instructions, or None if not supported
        """
        key = self._generate_key(tenant_id, document_id, attachment_id, filename)

        # Add standard metadata
        full_metadata = {
            "tenant-id": tenant_id,
            "document-id": document_id,
            "attachment-id": attachment_id,
            "original-filename": filename,
        }
        if metadata:
            full_metadata.update(metadata)

        result = await self._backend.generate_presigned_upload_url(
            key=key,
            content_type=content_type,
            expiry_seconds=expiry_seconds,
            metadata=full_metadata,
        )

        if result:
            result["storage_key"] = key
            result["filename"] = filename
            logger.info(f"Generated presigned upload URL for {key} (expires in {expiry_seconds}s)")

        return result

    def supports_presigned_urls(self) -> bool:
        """Check if the current backend supports presigned URLs.

        Note: OSS presigned URLs are not implemented yet.
        Only S3 backend supports presigned uploads currently.
        """
        # Only S3 has presigned upload implemented
        # OSS support can be added by implementing generate_presigned_upload_url in OSSBackend
        return self.config.backend == StorageBackend.S3

    async def download_image(
        self,
        tenant_id: str,
        document_id: str,
        attachment_id: str,
        filename: str,
    ) -> bytes:
        """
        Download an image from storage.

        Args:
            tenant_id: Tenant ID
            document_id: Document ID
            attachment_id: Confluence attachment ID
            filename: Original filename

        Returns:
            Image binary content
        """
        key = self._generate_key(tenant_id, document_id, attachment_id, filename)
        return await self._backend.download(key)

    async def delete_image(
        self,
        tenant_id: str,
        document_id: str,
        attachment_id: str,
        filename: str,
    ) -> bool:
        """
        Delete an image from storage.

        Args:
            tenant_id: Tenant ID
            document_id: Document ID
            attachment_id: Confluence attachment ID
            filename: Original filename

        Returns:
            True if deleted successfully
        """
        key = self._generate_key(tenant_id, document_id, attachment_id, filename)
        return await self._backend.delete(key)

    @staticmethod
    def _normalize_document_asset_scope(
        tenant_id: str,
        document_id: str,
    ) -> tuple[str, str]:
        normalized: list[str] = []
        for label, value in (
            ("tenant_id", tenant_id),
            ("document_id", document_id),
        ):
            item = str(value or "").strip()
            if not item or "/" in item or "\\" in item or ".." in item:
                raise ValueError(f"{label} is invalid for document asset deletion")
            normalized.append(item)
        return normalized[0], normalized[1]

    async def delete_document_images(
        self,
        tenant_id: str,
        document_id: str,
    ) -> int:
        """
        Delete all images for a document.

        Args:
            tenant_id: Tenant ID
            document_id: Document ID

        Returns:
            Number of deleted images
        """
        tenant_id, document_id = self._normalize_document_asset_scope(
            tenant_id,
            document_id,
        )
        prefix = f"knowledge/confluence/{tenant_id}/{document_id}/images/"
        deleted = await self._backend.delete_prefix(prefix)
        logger.info(f"Deleted {deleted} images for document {document_id}")
        return deleted

    async def delete_document_assets(
        self,
        tenant_id: str,
        document_id: str,
    ) -> int:
        """Delete every storage namespace owned by one exact document."""

        tenant_id, document_id = self._normalize_document_asset_scope(
            tenant_id,
            document_id,
        )

        prefixes = (
            f"knowledge/confluence/{tenant_id}/{document_id}/images/",
            f"knowledge/documents/{tenant_id}/{document_id}/original/",
        )
        deleted = 0
        for prefix in prefixes:
            deleted += int(await self._backend.delete_prefix(prefix) or 0)
        logger.info("Deleted %s stored assets for document %s", deleted, document_id)
        return deleted

    async def image_exists(
        self,
        tenant_id: str,
        document_id: str,
        attachment_id: str,
        filename: str,
    ) -> bool:
        """
        Check if an image exists in storage.

        Args:
            tenant_id: Tenant ID
            document_id: Document ID
            attachment_id: Confluence attachment ID
            filename: Original filename

        Returns:
            True if image exists
        """
        key = self._generate_key(tenant_id, document_id, attachment_id, filename)
        return await self._backend.exists(key)

    def get_image_url(
        self,
        tenant_id: str,
        document_id: str,
        attachment_id: str,
        filename: str,
        expiry_seconds: int = 3600,
    ) -> str:
        """
        Get URL for accessing an image.

        Args:
            tenant_id: Tenant ID
            document_id: Document ID
            attachment_id: Confluence attachment ID
            filename: Original filename
            expiry_seconds: URL expiry time

        Returns:
            Image URL (signed if local storage)
        """
        key = self._generate_key(tenant_id, document_id, attachment_id, filename)
        url = self._backend.get_url(key, expiry_seconds)
        return self._sign_url_if_local(url)

    async def exists_by_key(self, storage_key: str) -> bool:
        """
        Check if a file exists by its storage key.

        This is useful for verifying presigned URL uploads where
        we have the full storage key but not the individual components.

        Args:
            storage_key: Full storage key (path)

        Returns:
            True if file exists
        """
        return await self._backend.exists(storage_key)

    def get_url_by_key(self, storage_key: str, expiry_seconds: int = 3600) -> str:
        """
        Get URL for a file by its storage key.

        This is useful for presigned URL uploads where we have
        the full storage key but not the individual components.

        Args:
            storage_key: Full storage key (path)
            expiry_seconds: URL expiry time

        Returns:
            File URL (signed if local storage)
        """
        url = self._backend.get_url(storage_key, expiry_seconds)
        return self._sign_url_if_local(url)

    async def get_presigned_url(
        self,
        storage_url: str,
        expiry_seconds: int = 3600,
        segment_id: str | None = None,
    ) -> str | None:
        """
        Generate a presigned URL for accessing an image.

        Text-First RAG: Use this method to generate presigned URLs for image
        results in search responses.

        Args:
            storage_url: The stored URL (can be S3/OSS URL, file:// URL, or storage key)
            expiry_seconds: URL expiry time in seconds
            segment_id: Optional segment ID (used for local storage API fallback)

        Returns:
            Presigned URL for downloading the image, or API endpoint for local storage
        """
        if not storage_url:
            return None

        # Handle local file:// URLs - convert to API endpoint
        if storage_url.startswith("file://"):
            if segment_id:
                return f"/api/v1/knowledge/images/{segment_id}"
            # Try to extract segment info from URL if no segment_id provided
            logger.warning(f"Local file URL without segment_id: {storage_url}")
            return None

        # Handle S3 backend - generate presigned download URL
        if self.config.backend == StorageBackend.S3 and isinstance(self._backend, S3StorageBackend):
            # Extract key from URL
            key = self._extract_key_from_url(storage_url)
            if key:
                presigned = await self._backend.generate_presigned_download_url(
                    key=key,
                    expiry_seconds=expiry_seconds,
                )
                if presigned:
                    return presigned

            # Fall back to direct URL if presigned generation fails
            return storage_url

        # Handle OSS backend - for now, return the public URL
        # OSS presigned URLs can be added later if needed
        if self.config.backend == StorageBackend.OSS:
            return storage_url

        # Default: return the original URL
        return storage_url

    def _extract_key_from_url(self, url: str) -> str | None:
        """
        Extract storage key from a URL.

        Args:
            url: Storage URL (S3/OSS URL or key)

        Returns:
            Storage key or None
        """
        if not url:
            return None

        # If it's already a key (starts with knowledge/), return as-is
        if url.startswith("knowledge/"):
            return url

        # Handle S3 URLs
        # Format: https://{bucket}.s3.{region}.amazonaws.com/{key}
        # or: {endpoint}/{bucket}/{key}
        try:
            from urllib.parse import urlparse

            parsed = urlparse(url)

            if parsed.scheme in ("http", "https"):
                path = parsed.path.lstrip("/")
                # If URL contains bucket name in host, path is the key
                if self.config.s3_bucket and self.config.s3_bucket in parsed.netloc:
                    return path
                # If using endpoint URL, first segment might be bucket
                if self.config.s3_endpoint_url and path.startswith(f"{self.config.s3_bucket}/"):
                    return path[len(self.config.s3_bucket) + 1 :]
                # Otherwise, assume path is the key
                return path

            return url
        except Exception as e:
            logger.warning(f"Failed to extract key from URL {url}: {e}")
            return None

    async def close(self) -> None:
        """Close the storage service and release resources"""
        if self._backend is not None:
            await self._backend.close()

    async def __aenter__(self) -> ImageStorageService:
        """Async context manager entry"""
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb) -> None:
        """Async context manager exit - cleanup resources"""
        await self.close()
