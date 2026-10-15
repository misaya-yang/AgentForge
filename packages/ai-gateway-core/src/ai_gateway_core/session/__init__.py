"""Session-manager contract + shared concrete implementation.

Both services need to read/write chat-session state. The asyncpg-backed
``DatabaseSessionManager`` was historically per-service (lived at
``src/services/session/database_session_manager.py`` in gateway). Phase
5f Batch C moved it here so the AS container no longer needs gateway
src/ for session persistence.

- ``SessionManagerLike`` — minimal Protocol used in caller type hints.
- ``DatabaseSessionManager`` — full asyncpg + Redis-cached concrete.
- ``Session`` / ``SessionMessage`` — domain dataclasses.
"""

from __future__ import annotations

from typing import Any, Protocol, runtime_checkable

from .database_manager import DatabaseSessionManager
from .models import Session, SessionMessage


@runtime_checkable
class SessionManagerLike(Protocol):
    """Minimal async contract for session CRUD used by assistant callers."""

    async def get_session(self, session_id: str) -> dict[str, Any] | None: ...
    async def create_session(self, **kwargs: Any) -> str: ...
    async def update_session(self, session_id: str, **kwargs: Any) -> None: ...
    async def append_message(self, session_id: str, message: dict[str, Any]) -> None: ...


__all__ = [
    "DatabaseSessionManager",
    "Session",
    "SessionManagerLike",
    "SessionMessage",
]
