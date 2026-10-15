# Back-compat shim — moved to ai_gateway_core.storage in Phase 5f.
from ai_gateway_core.storage.artifact_storage import (
    ArtifactInfo,
    ArtifactStorageService,
    get_artifact_storage,
    init_artifact_storage,
)
from ai_gateway_core.storage.file_storage import (
    FileInfo,
    FileStorageService,
    get_file_storage,
    init_file_storage,
    shutdown_file_storage,
)

__all__ = [
    "ArtifactInfo",
    "ArtifactStorageService",
    "FileInfo",
    "FileStorageService",
    "get_artifact_storage",
    "get_file_storage",
    "init_artifact_storage",
    "init_file_storage",
    "shutdown_file_storage",
]
