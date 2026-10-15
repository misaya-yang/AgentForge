"""
Session management module.

Provides CRUD operations on assistant conversation sessions and access
to message history within a session.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from ai_assistant.models.request import SessionCreateRequest
from ai_assistant.models.response import Message, Session

if TYPE_CHECKING:
    from ai_assistant.transport.http import HTTPTransport


class SessionModule:
    """High-level session operations backed by ``/api/v1/assistant/sessions``."""

    _BASE = "/api/v1/assistant/sessions"

    def __init__(self, transport: HTTPTransport) -> None:
        self._transport = transport

    async def create(
        self,
        *,
        title: str | None = None,
        metadata: dict[str, Any] | None = None,
    ) -> Session:
        """Create a new conversation session.

        Args:
            title: Optional human-readable title stored in metadata.
            metadata: Arbitrary key-value metadata.

        Returns:
            The newly created ``Session``.
        """
        req = SessionCreateRequest(title=title, metadata=metadata)
        resp = await self._transport.request("POST", self._BASE, json=req.to_dict())
        return Session.from_dict(resp.json())

    async def list(self, *, limit: int = 50, offset: int = 0) -> list[Session]:
        """List the current user's sessions, newest first.

        Args:
            limit: Maximum number of sessions to return.
            offset: Pagination offset.

        Returns:
            List of ``Session`` objects.
        """
        params: dict[str, Any] = {"limit": limit}
        if offset > 0:
            params["offset"] = offset
        resp = await self._transport.request("GET", self._BASE, params=params)
        data = resp.json()
        sessions_raw = data.get("sessions", data) if isinstance(data, dict) else data
        return [Session.from_dict(s) for s in sessions_raw]

    async def get(self, session_id: str) -> Session:
        """Retrieve a single session by ID.

        Raises:
            NotFoundError: If the session does not exist.
        """
        resp = await self._transport.request("GET", f"{self._BASE}/{session_id}")
        return Session.from_dict(resp.json())

    async def delete(self, session_id: str) -> None:
        """Delete a session and all associated data.

        Raises:
            NotFoundError: If the session does not exist.
        """
        await self._transport.request("DELETE", f"{self._BASE}/{session_id}")

    async def history(
        self,
        session_id: str,
        *,
        limit: int = 100,
    ) -> list[Message]:
        """Retrieve the message history for a session.

        Args:
            session_id: Target session.
            limit: Maximum number of messages to return.

        Returns:
            List of ``Message`` objects in chronological order.
        """
        resp = await self._transport.request(
            "GET",
            f"{self._BASE}/{session_id}/history",
            params={"limit": limit},
        )
        data = resp.json()
        messages_raw = data.get("messages", data) if isinstance(data, dict) else data
        return [Message.from_dict(m) for m in messages_raw]
