"""Closed browser-facing schemas for Agent Preview and published runtime."""

from __future__ import annotations

from datetime import datetime
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field, model_validator


class _ClosedRuntimeModel(BaseModel):
    model_config = ConfigDict(extra="forbid")


class _ResumeApprovalRuntimeModel(_ClosedRuntimeModel):
    resume_run_id: str | None = Field(default=None, min_length=1, max_length=255)
    resume_approval_id: str | None = Field(default=None, min_length=1, max_length=255)

    @model_validator(mode="after")
    def require_complete_resume_identity(self) -> _ResumeApprovalRuntimeModel:
        if (self.resume_run_id is None) != (self.resume_approval_id is None):
            raise ValueError(
                "resume_run_id and resume_approval_id must be provided together"
            )
        return self


class AgentRuntimeAttachment(_ClosedRuntimeModel):
    artifact_id: str = Field(min_length=1, max_length=255)
    filename: str | None = Field(default=None, max_length=255)
    mime_type: str | None = Field(default=None, max_length=255)


class AgentRuntimeAttachmentUploadResponse(_ClosedRuntimeModel):
    artifact_id: str
    filename: str
    mime_type: str
    size_bytes: int
    expires_at: datetime | str
    request_id: str


class AgentPreviewSessionRequest(_ClosedRuntimeModel):
    draft_revision: int = Field(ge=1)


class AgentVersionPreviewSessionRequest(_ClosedRuntimeModel):
    """Create an isolated Preview session pinned to one immutable Version."""


class AgentRuntimeEffectiveCapability(_ClosedRuntimeModel):
    name: str
    schema_hash: str
    risk: Literal["low", "medium", "high", "critical"]
    requires_confirmation: bool


class AgentRuntimeSessionResponse(_ClosedRuntimeModel):
    session_id: str
    agent_id: str
    agent_version_id: str | None = None
    draft_revision: int | None = None
    publication_id: str | None = None
    channel: str
    runtime_fingerprint: str
    effective_capabilities: list[AgentRuntimeEffectiveCapability] = Field(
        default_factory=list,
        max_length=128,
    )
    request_id: str


class AgentPreviewChatRequest(_ResumeApprovalRuntimeModel):
    message: str = Field(min_length=1, max_length=200_000)
    session_id: str | None = Field(default=None, min_length=1, max_length=255)
    draft_revision: int = Field(ge=1)
    attachments: list[AgentRuntimeAttachment] = Field(default_factory=list, max_length=20)


class AgentVersionPreviewChatRequest(_ResumeApprovalRuntimeModel):
    message: str = Field(min_length=1, max_length=200_000)
    session_id: str | None = Field(default=None, min_length=1, max_length=255)
    attachments: list[AgentRuntimeAttachment] = Field(default_factory=list, max_length=20)


class AgentPublishedChatRequest(_ResumeApprovalRuntimeModel):
    message: str = Field(min_length=1, max_length=200_000)
    session_id: str | None = Field(default=None, min_length=1, max_length=255)
    attachments: list[AgentRuntimeAttachment] = Field(default_factory=list, max_length=20)


class AgentPublicSessionRequest(_ClosedRuntimeModel):
    channel: Literal["hosted", "embed"]
    embed_token: str | None = Field(default=None, max_length=2048)


class AgentPublicChatRequest(AgentPublishedChatRequest):
    channel: Literal["hosted", "embed"]
    embed_token: str | None = Field(default=None, max_length=2048)


class AgentRuntimeFeedbackRequest(_ClosedRuntimeModel):
    session_id: str = Field(min_length=1, max_length=255)
    rating: Literal[-1, 1]
    comment: str = Field(default="", max_length=2000)
    channel: Literal["hosted", "embed", "api"] = "api"
    embed_token: str | None = Field(default=None, max_length=2048)


class AgentPublicConfigResponse(_ClosedRuntimeModel):
    public_id: str
    publication_id: str
    channel: Literal["hosted", "embed"]
    auth_mode: Literal["private", "tenant", "public", "token"]
    name: str
    description: str = ""
    identity: dict[str, Any] = Field(default_factory=dict)
    attachments: bool = False
    version_number: int = Field(default=1, ge=1)
    capability_count: int = Field(default=0, ge=0)
    knowledge_count: int = Field(default=0, ge=0)
    release_gate_verified: bool = False
    published_at: datetime | None = None
    request_id: str


class AgentApiTokenCreateRequest(_ClosedRuntimeModel):
    name: str = Field(min_length=1, max_length=255)
    scopes: list[Literal[
        "chat:write",
        "sessions:write",
        "attachments:write",
        "feedback:write",
    ]] = Field(min_length=1, max_length=4)
    expires_at: datetime | None = None


class AgentApiTokenRotateRequest(_ClosedRuntimeModel):
    name: str | None = Field(default=None, min_length=1, max_length=255)
    scopes: list[Literal[
        "chat:write",
        "sessions:write",
        "attachments:write",
        "feedback:write",
    ]] | None = Field(default=None, min_length=1, max_length=4)
    expires_at: datetime | None = None


class AgentApiTokenMetadata(_ClosedRuntimeModel):
    tenant_id: str
    token_id: str
    publication_id: str
    name: str
    scopes: list[str]
    expires_at: datetime | str | None = None
    revoked_at: datetime | str | None = None
    last_used_at: datetime | str | None = None
    rotated_from_token_id: str | None = None
    created_by: str
    created_at: datetime | str


class AgentApiTokenIssueResponse(_ClosedRuntimeModel):
    token: str
    token_metadata: AgentApiTokenMetadata
    request_id: str


class AgentApiTokenListResponse(_ClosedRuntimeModel):
    tokens: list[AgentApiTokenMetadata]
    request_id: str


class AgentRuntimeFeedbackResponse(_ClosedRuntimeModel):
    feedback_id: str
    session_id: str
    rating: Literal[-1, 1]
    request_id: str


class InternalAgentRuntimeChatRequest(_ResumeApprovalRuntimeModel):
    """Gateway-authored body accepted only by the Assistant internal route."""

    message: str = Field(min_length=1, max_length=200_000)
    session_id: str = Field(min_length=1, max_length=255)
    history: list[dict[str, Any]] | None = Field(default=None, max_length=200)
    attachments: list[dict[str, Any]] = Field(default_factory=list, max_length=20)
    runtime_envelope: dict[str, Any]

    def verification_body(self) -> dict[str, Any]:
        body = {
            "message": self.message,
            "session_id": self.session_id,
            "history": self.history,
            "attachments": self.attachments,
        }
        if self.resume_run_id is not None:
            body.update(
                {
                    "resume_run_id": self.resume_run_id,
                    "resume_approval_id": self.resume_approval_id,
                }
            )
        return body
