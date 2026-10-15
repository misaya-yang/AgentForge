"""Storage contract.

File and artifact storage are provider-specific (S3, OSS, local disk).
These Protocols describe the operations assistant-owned code actually
calls on storage instances.

NoOp reference impls are provided so an un-injected AssistantService
degrades to silent no-op behavior instead of NoneType-crashing.

Concrete implementations (moved here from gateway src/services/storage/
in Phase 5f Batch B) are also re-exported below.
"""

from __future__ import annotations

from typing import Any, Protocol, runtime_checkable

from .artifact_storage import (
    ArtifactInfo,
    ArtifactStorageService,
    get_artifact_storage,
    init_artifact_storage,
)
from .file_storage import (
    FileInfo,
    FileStorageService,
    get_file_storage,
    init_file_storage,
    shutdown_file_storage,
)
from .image_storage import (
    BaseStorageBackend,
    ImageStorageService,
    LocalStorageBackend,
    OSSStorageBackend,
    S3StorageBackend,
    StorageBackend,
    StorageConfig,
)


@runtime_checkable
class FileStorageLike(Protocol):
    """Contract for uploading/retrieving user-uploaded files."""

    # ``config`` attribute access (``.config.backend``, ``.config.local_base_path``)
    # leaks from two diagnostic-logging lines in the assistant. Typed as Any so
    # Protocol-satisfying null impls can expose a lightweight stand-in without
    # dragging the concrete StorageConfig class into ai-gateway-core.
    config: Any

    async def download_file(self, path: str) -> bytes: ...


@runtime_checkable
class ArtifactStorageLike(Protocol):
    """Contract for persisting agent-generated artifacts (images, PPT, reports)."""

    async def create_artifact(self, **fields: Any) -> Any: ...

    async def get_presigned_download_url(
        self, artifact: Any, expiry_seconds: int = 3600
    ) -> str: ...

    def supports_scoped_artifact_reads(self) -> bool: ...

    async def read_artifact_scoped(
        self,
        artifact_id: str,
        *,
        tenant_id: str,
        session_id: str,
        user_id: str,
        max_bytes: int,
        expected_source: str = "tool_output_spill",
    ) -> tuple[Any, bytes] | None: ...


class _NoOpConfig:
    """Minimal stand-in for StorageConfig used by NoOpFileStorage."""

    class _NoOpBackend:
        value = "noop"

    backend = _NoOpBackend()
    local_base_path = "/tmp"


class NoOpFileStorage:
    """Protocol-satisfying null FileStorage. download_file returns empty bytes.

    ``__bool__`` returns False so legacy ``if self.file_storage:`` truthy
    checks continue to skip the "real storage configured" branch when the
    service is un-injected — matching the pre-DI semantics where the
    attribute was ``None`` on initialisation failure.
    """

    config: Any = _NoOpConfig()

    def __bool__(self) -> bool:
        return False

    async def download_file(self, path: str) -> bytes:
        del path
        return b""


class NoOpArtifactStorage:
    """Protocol-satisfying null ArtifactStorage. Persist calls return None-like stubs.

    See ``NoOpFileStorage.__bool__`` — same rationale.
    """

    def __bool__(self) -> bool:
        return False

    async def create_artifact(self, **fields: Any) -> Any:
        del fields
        return None

    async def get_presigned_download_url(self, artifact: Any, expiry_seconds: int = 3600) -> str:
        del artifact, expiry_seconds
        return ""

    def supports_scoped_artifact_reads(self) -> bool:
        return False

    async def read_artifact_scoped(
        self,
        artifact_id: str,
        *,
        tenant_id: str,
        session_id: str,
        user_id: str,
        max_bytes: int,
        expected_source: str = "tool_output_spill",
    ) -> tuple[Any, bytes] | None:
        del artifact_id, tenant_id, session_id, user_id, max_bytes, expected_source
        return None


__all__ = [
    "ArtifactStorageLike",
    "FileStorageLike",
    "NoOpArtifactStorage",
    "NoOpFileStorage",
    # Concrete image-storage stack (moved in Phase 5f Batch B)
    "BaseStorageBackend",
    "ImageStorageService",
    "LocalStorageBackend",
    "OSSStorageBackend",
    "S3StorageBackend",
    "StorageBackend",
    "StorageConfig",
    # Artifact storage (moved in Phase 5f Batch B)
    "ArtifactInfo",
    "ArtifactStorageService",
    "get_artifact_storage",
    "init_artifact_storage",
    # File storage (moved in Phase 5f Batch B)
    "FileInfo",
    "FileStorageService",
    "get_file_storage",
    "init_file_storage",
    "shutdown_file_storage",
]
