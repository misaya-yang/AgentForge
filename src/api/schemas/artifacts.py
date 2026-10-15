"""
Artifact schemas for assistant API.

Artifacts represent outputs from code execution, document generation,
image generation, or other tool invocations.
"""

from typing import Any

from pydantic import BaseModel


class ArtifactInfo(BaseModel):
    """Artifact metadata for API responses."""

    artifact_id: str
    session_id: str
    type: str  # image, document, chart, code, file
    format: str  # png, pdf, docx, md, csv, json, etc.
    title: str
    filename: str
    size_bytes: int
    ready: bool = True
    variant: str = "raw"
    parent_artifact_id: str | None = None
    mime_type: str | None = None
    source: str = "ai"  # ai | user | code_execution
    message_id: str | None = None
    download_url: str | None = None  # Presigned URL for download
    metadata: dict[str, Any] | None = None
    created_at: str | None = None


class ArtifactListResponse(BaseModel):
    """Response with list of artifacts."""

    artifacts: list[ArtifactInfo]
    total: int


class ArtifactCreateRequest(BaseModel):
    """Request to create an artifact."""

    session_id: str
    type: str
    format: str
    title: str
    filename: str
    content_base64: str  # Base64 encoded content
    source: str = "ai"
    message_id: str | None = None
    metadata: dict[str, Any] | None = None
