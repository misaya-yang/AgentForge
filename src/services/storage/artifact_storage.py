# Back-compat shim — moved to ai_gateway_core.storage.artifact_storage in Phase 5f.
# Tests and external callers may still import via this path during transition.
from ai_gateway_core.storage.artifact_storage import *  # noqa: F401,F403
from ai_gateway_core.storage.artifact_storage import (  # noqa: F401
    ArtifactInfo,
    ArtifactStorageService,
    get_artifact_storage,
    init_artifact_storage,
)
