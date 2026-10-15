"""Back-compat shim — repositories moved to ai_gateway_core in Phase 5f Batch C.

The repository implementations now live at
``packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/``.
This re-export keeps any external import of ``src.persistence.repositories``
working while the canonical home is in ai_gateway_core.
"""

from ai_gateway_core.persistence.repositories import (
    APIKeyRepository,
    BaseRepository,
    DatabaseAPIKeyRepository,
    DatabaseServiceRepository,
    DatabaseSessionRepository,
    DatabaseTaskRepository,
    DatabaseUserRepository,
    ServiceRepository,
    SessionRepository,
    TaskRepository,
    UserRepository,
)

__all__ = [
    "APIKeyRepository",
    "BaseRepository",
    "DatabaseAPIKeyRepository",
    "DatabaseServiceRepository",
    "DatabaseSessionRepository",
    "DatabaseTaskRepository",
    "DatabaseUserRepository",
    "ServiceRepository",
    "SessionRepository",
    "TaskRepository",
    "UserRepository",
]
