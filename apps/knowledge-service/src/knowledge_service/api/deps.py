"""FastAPI dependency injection helpers.

These functions retrieve shared resources that were stored on
``app.state`` during the lifespan startup phase, and expose them as
FastAPI ``Depends()`` callables.
"""

from __future__ import annotations

from fastapi import Request

from ..auth.user_context import UserContext
from ..auth.user_context import get_user_context as _get_user_context
from ..config import Settings
from ..persistence.database import DatabaseStorage


def get_settings(request: Request) -> Settings:
    """Return the application settings singleton."""
    return request.app.state.settings


def get_db(request: Request) -> DatabaseStorage:
    """Return the asyncpg database pool."""
    return request.app.state.db


def get_qdrant(request: Request):
    """Return the Qdrant async client."""
    return request.app.state.qdrant


async def get_user_context(request: Request) -> UserContext:
    """Return the resolved user context from gateway headers."""
    return await _get_user_context(request)


def get_knowledge_service(request: Request):
    """Return the KnowledgeService instance."""
    return getattr(request.app.state, "knowledge_service", None)


def get_knowledge_worker(request: Request):
    """Return the KnowledgeWorker instance."""
    return getattr(request.app.state, "knowledge_worker", None)
