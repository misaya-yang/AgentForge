"""
Artifact module — create, retrieve, download, and manage artifacts.

Artifacts are files generated during a conversation (images, documents,
charts, code snippets, etc.) that persist on the server and can be
downloaded through the owner-authorized Gateway endpoint.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from ai_assistant.models.request import CreateArtifactRequest
from ai_assistant.models.response import Artifact

if TYPE_CHECKING:
    from ai_assistant.transport.http import HTTPTransport


class ArtifactModule:
    """High-level artifact operations backed by ``/api/v1/assistant/artifacts``."""

    _BASE = "/api/v1/assistant/artifacts"
    _SESSION_ARTIFACTS = "/api/v1/assistant/sessions/{session_id}/artifacts"

    def __init__(self, transport: HTTPTransport) -> None:
        self._transport = transport

    async def create(
        self,
        *,
        session_id: str,
        content_base64: str,
        filename: str,
        title: str,
        type: str = "file",  # noqa: A002 - public API field name
        format: str = "",  # noqa: A002 - public API field name
        mime_type: str | None = None,
        source: str | None = None,
        message_id: str | None = None,
        metadata: dict[str, Any] | None = None,
    ) -> Artifact:
        """Upload a new artifact from base64-encoded content.

        Args:
            session_id: Session the artifact belongs to.
            content_base64: Base64-encoded file bytes.
            filename: Target filename.
            title: Human-readable title.
            type: Artifact type (``image``, ``document``, ``chart``, ``code``, ``file``).
            format: File format / extension (``png``, ``pdf``, ``json``, etc.).
            mime_type: Optional MIME type override.
            source: Origin (``ai``, ``user``, ``code_execution``, ``image_generation``).
            message_id: Optional link to the generating message.
            metadata: Arbitrary key-value metadata.

        Returns:
            The created ``Artifact`` with server-assigned ID and download URL.
        """
        req = CreateArtifactRequest(
            session_id=session_id,
            type=type,
            format=format,
            title=title,
            filename=filename,
            content_base64=content_base64,
            source=source,
            message_id=message_id,
            metadata=metadata,
        )
        resp = await self._transport.request("POST", self._BASE, json=req.to_dict())
        return Artifact.from_dict(resp.json())

    async def get(self, artifact_id: str) -> Artifact:
        """Retrieve artifact metadata (including a fresh download URL).

        Raises:
            NotFoundError: If the artifact does not exist.
        """
        resp = await self._transport.request("GET", f"{self._BASE}/{artifact_id}")
        return Artifact.from_dict(resp.json())

    async def download(self, artifact_id: str) -> bytes:
        """Download the raw bytes of an artifact.

        The Gateway streams bytes after current owner/source-access checks.
        Redirect responses are refused so credentials cannot leave the Gateway.
        """
        resp = await self._transport.request("GET", f"{self._BASE}/{artifact_id}/download")
        return resp.content

    def download_url(self, artifact_id: str) -> str:
        """Return the authenticated Gateway download URL.

        This does **not** make a network call — it only constructs the path.
        Consumers still need Gateway authentication; this is not a signed grant.
        """
        base = self._transport._auth.base_url
        return f"{base}{self._BASE}/{artifact_id}/download"

    async def list_by_session(self, session_id: str) -> list[Artifact]:
        """List all artifacts attached to a session.

        Args:
            session_id: Target session.

        Returns:
            List of ``Artifact`` objects, newest first.
        """
        path = self._SESSION_ARTIFACTS.format(session_id=session_id)
        resp = await self._transport.request("GET", path)
        data = resp.json()
        artifacts_raw = data.get("artifacts", data) if isinstance(data, dict) else data
        return [Artifact.from_dict(a) for a in artifacts_raw]

    async def delete(self, artifact_id: str) -> None:
        """Permanently delete an artifact.

        Raises:
            NotFoundError: If the artifact does not exist.
        """
        await self._transport.request("DELETE", f"{self._BASE}/{artifact_id}")
