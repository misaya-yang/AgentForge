# Back-compat shim — moved to ai_gateway_core.storage.file_storage in Phase 5f.
# Tests and external callers may still import via this path during transition.
from ai_gateway_core.storage.file_storage import *  # noqa: F401,F403
from ai_gateway_core.storage.file_storage import (  # noqa: F401
    FileInfo,
    FileStorageService,
    get_file_storage,
    init_file_storage,
    shutdown_file_storage,
)
