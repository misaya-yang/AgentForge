"""Assistant artifact routes: list, read, create, delete, download.

ARC-01 split of ``src/api/v1/assistant.py``.  Error sanitization (ARC-01
deliverable 6): 500 responses carry a stable public detail only; the raw
exception is recorded through ``record_internal_exception`` (type +
fingerprint, no message) instead of being echoed to the caller.
"""

from __future__ import annotations

import logging

from ai_gateway_core.logging import record_internal_exception
from ai_gateway_core.storage import get_artifact_storage
from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import StreamingResponse

from ....core.auth.user_resolver import UserContext
from ....services.assistant_entry.session_binding import get_session_manager
from ....services.assistant_entry.source_access import require_artifact_source_access
from ...deps import get_user_context
from ...schemas.artifacts import ArtifactCreateRequest, ArtifactInfo, ArtifactListResponse
from .._artifact_headers import attachment_content_disposition

router = APIRouter()
logger = logging.getLogger(__name__)


def _browser_artifact_download_url(raw_url: str | None, artifact_id: str) -> str:
    """Every browser read rechecks ownership and current source rights."""
    del raw_url
    return f"/api/v1/assistant/artifacts/{artifact_id}/download"


def _is_missing_artifact_schema_error(exc: Exception) -> bool:
    """Treat uninitialized artifact storage as an empty artifact list during restore."""
    exc_type = type(exc)
    if exc_type.__module__.startswith("asyncpg") and exc_type.__name__ in {
        "InvalidSchemaNameError",
        "UndefinedTableError",
    }:
        return True

    message = str(exc).lower()
    return (
        'relation "assistant.artifacts" does not exist' in message
        or 'schema "assistant" does not exist' in message
    )


def _raise_artifact_not_found_if_schema_missing(exc: Exception) -> None:
    """Hide uninitialized artifact schema details behind the public 404 contract."""
    if _is_missing_artifact_schema_error(exc):
        logger.warning("Artifact storage schema is not initialized; treating artifact as not found")
        raise HTTPException(status_code=404, detail="Artifact not found") from None


@router.get("/sessions/{session_id}/artifacts", response_model=ArtifactListResponse)
async def list_session_artifacts(
    session_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> ArtifactListResponse:
    """
    List all artifacts for a session.

    Returns artifacts created during the conversation session.
    Artifacts are loaded when switching back to a conversation.

    Args:
        session_id: Session ID to get artifacts for.

    Returns:
        ArtifactListResponse with list of artifacts.
    """
    # Verify session ownership
    session_manager = get_session_manager(request)
    try:
        session = await session_manager.get(session_id)
        if not session:
            raise HTTPException(status_code=404, detail="Session not found")
        if session.user_id != user.user_id or session.tenant_id != user.tenant_id:
            raise HTTPException(status_code=404, detail="Session not found")
    except HTTPException:
        raise
    except Exception as exc:
        record_internal_exception(logger, "assistant.gateway.artifacts.session_check_failure", exc)
        raise HTTPException(status_code=500, detail="Failed to verify session") from None

    # Get artifacts
    artifact_storage = get_artifact_storage()
    if not artifact_storage:
        return ArtifactListResponse(artifacts=[], total=0)

    try:
        artifacts = await artifact_storage.get_session_artifacts(session_id, user.tenant_id)

        # Browser downloads always return through current gateway authorization.
        artifact_list = []
        for art in artifacts:
            if art.source == "user" or getattr(art, "variant", "raw") != "raw":
                continue
            try:
                await require_artifact_source_access(request, user, art)
            except HTTPException as exc:
                if exc.status_code == 403:
                    continue  # Restricted messages explain the unavailable source.
                raise
            download_url = _browser_artifact_download_url(
                None,
                art.artifact_id,
            )
            artifact_list.append(
                ArtifactInfo(
                    artifact_id=art.artifact_id,
                    session_id=art.session_id,
                    type=art.type,
                    format=art.format,
                    title=art.title,
                    filename=art.filename,
                    size_bytes=art.size_bytes,
                    ready=art.size_bytes > 0,
                    variant=getattr(art, "variant", "raw"),
                    parent_artifact_id=getattr(art, "parent_artifact_id", None),
                    mime_type=art.mime_type,
                    source=art.source,
                    message_id=art.message_id,
                    download_url=download_url,
                    metadata=art.metadata,
                    created_at=art.created_at.isoformat() if art.created_at else None,
                )
            )

        return ArtifactListResponse(artifacts=artifact_list, total=len(artifact_list))
    except HTTPException:
        raise
    except Exception as exc:
        if _is_missing_artifact_schema_error(exc):
            logger.warning(
                "Artifact storage schema is not initialized; returning empty artifact list"
            )
            return ArtifactListResponse(artifacts=[], total=0)
        record_internal_exception(logger, "assistant.gateway.artifacts.list_failure", exc)
        raise HTTPException(status_code=500, detail="Failed to list artifacts") from None


@router.get("/artifacts/{artifact_id}", response_model=ArtifactInfo)
async def get_artifact(
    artifact_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> ArtifactInfo:
    """
    Get artifact metadata with fresh download URL.

    Returns metadata for an artifact including a fresh presigned download URL.

    Args:
        artifact_id: Unique identifier for the artifact.

    Returns:
        ArtifactInfo with artifact metadata and download URL.

    Raises:
        404: Artifact not found.
    """
    artifact_storage = get_artifact_storage()
    if not artifact_storage:
        raise HTTPException(status_code=503, detail="Artifact storage not initialized")

    try:
        artifact = await artifact_storage.get_artifact(artifact_id)
        if not artifact:
            raise HTTPException(status_code=404, detail="Artifact not found")

        # Verify ownership
        if artifact.tenant_id != user.tenant_id or artifact.user_id != user.user_id:
            raise HTTPException(status_code=404, detail="Artifact not found")

        await require_artifact_source_access(request, user, artifact)

        # Metadata reads do not depend on generating an unused storage URL.
        download_url = _browser_artifact_download_url(
            None,
            artifact.artifact_id,
        )

        return ArtifactInfo(
            artifact_id=artifact.artifact_id,
            session_id=artifact.session_id,
            type=artifact.type,
            format=artifact.format,
            title=artifact.title,
            filename=artifact.filename,
            size_bytes=artifact.size_bytes,
            ready=artifact.size_bytes > 0,
            mime_type=artifact.mime_type,
            source=artifact.source,
            message_id=artifact.message_id,
            download_url=download_url,
            metadata=artifact.metadata,
            created_at=artifact.created_at.isoformat() if artifact.created_at else None,
        )
    except HTTPException:
        raise
    except Exception as exc:
        _raise_artifact_not_found_if_schema_missing(exc)
        record_internal_exception(logger, "assistant.gateway.artifacts.get_failure", exc)
        raise HTTPException(status_code=500, detail="Failed to get artifact") from None


@router.post("/artifacts", response_model=ArtifactInfo)
async def create_artifact(
    body: ArtifactCreateRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """
    Create an artifact from base64 encoded content.

    Used for saving generated images, documents, etc. to the artifact storage.

    Args:
        body: Artifact creation request with base64 encoded content.

    Returns:
        Created artifact metadata with download URL.
    """
    import base64

    artifact_storage = get_artifact_storage()
    if not artifact_storage:
        raise HTTPException(status_code=503, detail="Artifact storage not initialized")

    try:
        session_manager = get_session_manager(request)
        try:
            session = await session_manager.get(body.session_id)
        except Exception as exc:
            record_internal_exception(
                logger, "assistant.gateway.artifacts.session_check_failure", exc
            )
            raise HTTPException(status_code=500, detail="Failed to verify session") from None
        if not session or session.user_id != user.user_id or session.tenant_id != user.tenant_id:
            raise HTTPException(status_code=404, detail="Session not found")

        # Decode base64 content
        try:
            content = base64.b64decode(body.content_base64)
        except Exception as e:
            raise HTTPException(status_code=400, detail=f"Invalid base64 content: {e}")

        # Determine MIME type
        mime_type_map = {
            "png": "image/png",
            "jpg": "image/jpeg",
            "jpeg": "image/jpeg",
            "gif": "image/gif",
            "webp": "image/webp",
            "pdf": "application/pdf",
            "json": "application/json",
            "csv": "text/csv",
            "md": "text/markdown",
            "txt": "text/plain",
        }
        mime_type_map.get(body.format.lower(), "application/octet-stream")

        # Create artifact
        artifact = await artifact_storage.create_artifact(
            session_id=body.session_id,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            type=body.type,
            format=body.format,
            title=body.title,
            filename=body.filename,
            content=content,
            source=body.source,
            message_id=body.message_id,
            metadata=body.metadata,
        )

        # Creation is complete; do not fail it on unused storage URL signing.
        download_url = _browser_artifact_download_url(
            None,
            artifact.artifact_id,
        )

        return ArtifactInfo(
            artifact_id=artifact.artifact_id,
            session_id=artifact.session_id,
            type=artifact.type,
            format=artifact.format,
            title=artifact.title,
            filename=artifact.filename,
            size_bytes=artifact.size_bytes,
            ready=artifact.size_bytes > 0,
            mime_type=artifact.mime_type,
            source=artifact.source,
            message_id=artifact.message_id,
            download_url=download_url,
            metadata=artifact.metadata,
            created_at=artifact.created_at.isoformat() if artifact.created_at else None,
        )
    except HTTPException:
        raise
    except Exception as exc:
        record_internal_exception(logger, "assistant.gateway.artifacts.create_failure", exc)
        raise HTTPException(status_code=500, detail="Failed to create artifact") from None


@router.delete("/artifacts/{artifact_id}")
async def delete_artifact(
    artifact_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """
    Delete an artifact.

    Removes the artifact file from storage and metadata from database.

    Args:
        artifact_id: Unique identifier for the artifact.

    Returns:
        Confirmation of deletion.

    Raises:
        404: Artifact not found.
    """
    artifact_storage = get_artifact_storage()
    if not artifact_storage:
        raise HTTPException(status_code=503, detail="Artifact storage not initialized")

    try:
        artifact = await artifact_storage.get_artifact(artifact_id)
        if not artifact:
            raise HTTPException(status_code=404, detail="Artifact not found")

        # Verify ownership
        if artifact.tenant_id != user.tenant_id or artifact.user_id != user.user_id:
            raise HTTPException(status_code=404, detail="Artifact not found")

        await artifact_storage.delete_artifact(artifact_id)
        return {"artifact_id": artifact_id, "status": "deleted"}
    except HTTPException:
        raise
    except Exception as exc:
        _raise_artifact_not_found_if_schema_missing(exc)
        record_internal_exception(logger, "assistant.gateway.artifacts.delete_failure", exc)
        raise HTTPException(status_code=500, detail="Failed to delete artifact") from None


@router.get("/artifacts/{artifact_id}/download")
async def download_artifact(
    artifact_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """
    Download an artifact file.

    Redirects to presigned URL or streams content directly.

    Args:
        artifact_id: Unique identifier for the artifact.

    Returns:
        Redirect to download URL or streaming response.

    Raises:
        404: Artifact not found.
    """
    artifact_storage = get_artifact_storage()
    if not artifact_storage:
        raise HTTPException(status_code=503, detail="Artifact storage not initialized")

    try:
        artifact = await artifact_storage.get_artifact(artifact_id)
        if not artifact:
            raise HTTPException(status_code=404, detail="Artifact not found")

        # Verify ownership
        if artifact.tenant_id != user.tenant_id or artifact.user_id != user.user_id:
            raise HTTPException(status_code=404, detail="Artifact not found")
        await require_artifact_source_access(request, user, artifact)

        # Do not issue a storage URL that outlives a later source revocation.
        content = await artifact_storage.download_artifact(artifact_id)
        if content is None:
            raise HTTPException(status_code=404, detail="Artifact content not found")
        if not content:
            raise HTTPException(status_code=409, detail="Artifact is empty or unfinished")

        return StreamingResponse(
            iter([content]),
            media_type=artifact.mime_type or "application/octet-stream",
            headers={
                "Content-Disposition": attachment_content_disposition(artifact.filename),
                "Content-Length": str(len(content)),
            },
        )
    except HTTPException:
        raise
    except Exception as exc:
        _raise_artifact_not_found_if_schema_missing(exc)
        record_internal_exception(logger, "assistant.gateway.artifacts.download_failure", exc)
        raise HTTPException(status_code=500, detail="Failed to download artifact") from None
