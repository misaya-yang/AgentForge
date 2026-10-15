"""Response/request models private to the ``/api/v1/assistant`` surface.

Public, cross-surface schemas stay in ``src/api/schemas/assistant.py``; only
models used exclusively by the Assistant V1 routes live here (ARC-01).
"""

from __future__ import annotations

from pydantic import BaseModel


class SessionCreateRequest(BaseModel):
    """Request to create a new assistant session."""

    metadata: dict | None = None  # Optional metadata like title


class SessionResponse(BaseModel):
    """Response with session info."""

    session_id: str
    user_id: str
    tenant_id: str
    service_id: str | None = None
    created_at: str | None = None
    updated_at: str | None = None
    metadata: dict | None = None
    message_count: int = 0


class SessionListResponse(BaseModel):
    """Response with list of sessions."""

    sessions: list[SessionResponse]
    total: int


class SessionHistoryMessage(BaseModel):
    """A message in session history."""

    role: str
    content: str
    timestamp: str | None = None
    metadata: dict | None = None


class SessionHistoryResponse(BaseModel):
    """Response with session history."""

    session_id: str
    messages: list[SessionHistoryMessage]
    total: int


class ToolInfoResponse(BaseModel):
    """Tool information response."""

    name: str
    description: str
    category: str
    risk_level: str
    when_to_use: str | None = None
    when_not_to_use: str | None = None


class ToolsListResponse(BaseModel):
    """Response for listing available tools."""

    tools: list[ToolInfoResponse]


class AssistantPoliciesResponse(BaseModel):
    """Assistant gateway policy snapshot."""

    policies: dict


class ApprovalRequest(BaseModel):
    """Approve or reject a pending tool call."""

    approved: bool
    reason: str | None = None


class ApprovalResponse(BaseModel):
    """Approval mutation result."""

    approval: dict


class RunStatusResponse(BaseModel):
    """Assistant run status response."""

    run: dict


class ResumeRequest(BaseModel):
    """Optional approval binding for resume preparation."""

    approval_id: str | None = None
    session_id: str | None = None


class ResumeResponse(BaseModel):
    """Non-executing resume plan from the latest safe checkpoint."""

    resume: dict


class TaskCancelRequest(BaseModel):
    """Request to cancel a running task."""

    reason: str | None = None


class TaskCancelResponse(BaseModel):
    """Response for task cancellation."""

    task_id: str
    session_id: str
    cancelled: bool
    message: str


class ContextMetricsResponse(BaseModel):
    """Response with context metrics for a session."""

    session_id: str
    request_count: int
    avg_tokens: int
    avg_utilization: float
    avg_compression_ratio: float
    avg_cache_hit_rate: float
    total_tokens_used: int | None = None


class TenantMetricsResponse(BaseModel):
    """Response with aggregated tenant metrics."""

    tenant_id: str
    hours: int
    request_count: int
    unique_sessions: int
    total_tokens: int
    avg_tokens_per_request: int | None = None
    avg_utilization: float | None = None
