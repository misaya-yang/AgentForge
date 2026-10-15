"""Typed public API contracts for Agent Studio lifecycle management."""

from __future__ import annotations

import uuid
from datetime import datetime, timezone
from typing import Any, Literal
from urllib.parse import urlsplit

from ai_gateway_core.persistence.repositories.agent_repository import (
    redact_agent_spec_for_read,
    unsafe_agent_spec_paths,
)
from ai_gateway_core.security.redaction import redact_trace_text
from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator

AgentRole = Literal["owner", "editor", "viewer"]
AgentStatus = Literal["draft", "active", "archived", "deleted"]
CapabilityType = Literal["native", "model_native", "mcp", "skill", "connector", "knowledge"]
AgentChannel = Literal["hosted", "embed", "api"]
AgentAuthMode = Literal["private", "tenant", "public", "token"]
AgentReleaseStatus = Literal["queued", "running", "passed", "failed", "cancelled", "stale"]


class AgentIdentitySpec(BaseModel):
    model_config = ConfigDict(extra="forbid")

    icon_url: str | None = Field(None, max_length=2048)
    theme_color: str | None = Field(None, pattern=r"^#[0-9A-Fa-f]{6}$")
    welcome_message: str = Field("", max_length=4000)
    suggested_prompts: list[str] = Field(default_factory=list, max_length=12)

    @field_validator("suggested_prompts")
    @classmethod
    def _validate_suggested_prompts(cls, value: list[str]) -> list[str]:
        if any(not prompt.strip() or len(prompt) > 500 for prompt in value):
            raise ValueError("suggested prompts must be 1..500 characters")
        return value


class AgentModelSpec(BaseModel):
    model_config = ConfigDict(extra="forbid")

    model_id: str = Field("", max_length=255)
    provider_id: str | None = Field(None, max_length=128)
    temperature: float | None = Field(None, ge=0, le=2)
    max_tokens: int | None = Field(None, ge=1, le=1_000_000)
    thinking_mode: str | None = Field(None, max_length=64)


class AgentOutcomeSpec(BaseModel):
    model_config = ConfigDict(extra="forbid")

    goal: str = Field("", max_length=4000)
    triggers: list[str] = Field(default_factory=list, max_length=16)
    inputs: list[str] = Field(default_factory=list, max_length=32)
    human_boundaries: list[str] = Field(default_factory=list, max_length=32)
    success_criteria: list[str] = Field(default_factory=list, max_length=32)

    @field_validator("triggers", "inputs", "human_boundaries", "success_criteria")
    @classmethod
    def _validate_lines(cls, value: list[str]) -> list[str]:
        if any(not item.strip() or len(item) > 500 for item in value):
            raise ValueError("outcome list items must be 1..500 characters")
        return value


class AgentCapabilityBinding(BaseModel):
    model_config = ConfigDict(extra="forbid")

    type: CapabilityType
    resource_id: str = Field("", max_length=255)
    resource_version: str | None = Field(None, max_length=255)
    schema_hash: str | None = Field(None, pattern=r"^[0-9a-f]{64}$")
    config: dict[str, Any] = Field(default_factory=dict)


class AgentKnowledgeBinding(BaseModel):
    model_config = ConfigDict(extra="forbid")

    dataset_id: str = Field(..., min_length=1, max_length=255)
    retrieval_config: dict[str, Any] = Field(default_factory=dict)


class AgentSpec(BaseModel):
    """Versioned editing shape; runtime resolution is owned by later Phases."""

    model_config = ConfigDict(extra="forbid")

    schema_version: Literal["agent-spec/v1"] = "agent-spec/v1"
    identity: AgentIdentitySpec = Field(default_factory=AgentIdentitySpec)
    outcome: AgentOutcomeSpec = Field(default_factory=AgentOutcomeSpec)
    instructions: str = Field("", max_length=100_000)
    model: AgentModelSpec = Field(default_factory=AgentModelSpec)
    capabilities: list[AgentCapabilityBinding] = Field(default_factory=list, max_length=256)
    knowledge: list[AgentKnowledgeBinding] = Field(default_factory=list, max_length=128)
    memory: dict[str, Any] = Field(default_factory=dict)

    @model_validator(mode="after")
    def _no_sensitive_values(self) -> AgentSpec:
        unsafe = unsafe_agent_spec_paths(self.model_dump(mode="python"))
        if unsafe:
            raise ValueError(f"{unsafe[0]} must not contain credentials or secrets")
        return self


class AgentCreateRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    name: str = Field(..., min_length=1, max_length=255)
    slug: str | None = Field(None, min_length=1, max_length=128)
    description: str = Field("", max_length=4000)
    spec: AgentSpec = Field(default_factory=AgentSpec)


class AgentUpdateRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    name: str | None = Field(None, min_length=1, max_length=255)
    slug: str | None = Field(None, min_length=1, max_length=128)
    description: str | None = Field(None, max_length=4000)

    @model_validator(mode="after")
    def _at_least_one_field(self) -> AgentUpdateRequest:
        if not self.model_fields_set:
            raise ValueError("at least one mutable Agent field is required")
        return self


class AgentDraftUpdateRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    spec: AgentSpec
    name: str | None = Field(None, min_length=1, max_length=255)
    description: str | None = Field(None, max_length=4000)


class AgentCopyRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    name: str | None = Field(None, min_length=1, max_length=255)
    slug: str | None = Field(None, min_length=1, max_length=128)


class AgentArchiveRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    disable_publications: bool = False


class AgentChannelPolicy(BaseModel):
    model_config = ConfigDict(extra="forbid")

    attachments: bool = False
    high_risk_tools: bool = False
    allowed_origins: list[str] = Field(default_factory=list, max_length=64)
    expires_at: datetime | None = None
    requests_per_minute: int = Field(default=30, ge=1, le=10_000)
    requests_per_day: int = Field(default=1000, ge=1, le=10_000_000)
    ip_requests_per_minute: int = Field(default=60, ge=1, le=10_000)
    ip_requests_per_day: int = Field(default=2000, ge=1, le=10_000_000)
    publication_requests_per_minute: int = Field(default=300, ge=1, le=100_000)
    publication_requests_per_day: int = Field(default=10_000, ge=1, le=100_000_000)

    @field_validator("allowed_origins")
    @classmethod
    def _validate_origins(cls, value: list[str]) -> list[str]:
        normalized: list[str] = []
        for raw in value:
            origin = raw.strip().rstrip("/")
            parsed = urlsplit(origin)
            if (
                "*" in origin
                or origin.lower() == "null"
                or parsed.scheme not in {"http", "https"}
                or not parsed.netloc
                or parsed.username
                or parsed.password
                or parsed.path
                or parsed.query
                or parsed.fragment
            ):
                raise ValueError("allowed_origins must contain exact http(s) origins")
            normalized.append(origin)
        return sorted(set(normalized))

    @field_validator("expires_at")
    @classmethod
    def _expiry_is_timezone_aware(cls, value: datetime | None) -> datetime | None:
        if value is not None and (value.tzinfo is None or value.utcoffset() is None):
            raise ValueError("expires_at must include a timezone")
        return value


class AgentReleaseEvaluationRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    draft_revision: int = Field(..., ge=1)
    dataset_id: uuid.UUID | None = None
    channel: AgentChannel = "hosted"
    auth_mode: AgentAuthMode = "private"
    channel_policy: AgentChannelPolicy = Field(default_factory=AgentChannelPolicy)

    @model_validator(mode="after")
    def _public_channel_is_conservative(self) -> AgentReleaseEvaluationRequest:
        if self.auth_mode == "public" and self.channel_policy.high_risk_tools:
            raise ValueError("public channels cannot enable high-risk tools")
        if self.auth_mode == "public" and (
            self.channel_policy.expires_at is None
            or self.channel_policy.expires_at <= datetime.now(timezone.utc)
        ):
            raise ValueError("public channels require a future expires_at")
        if self.auth_mode != "public" and self.channel_policy.expires_at is not None:
            raise ValueError("expires_at is only supported for public channels")
        return self


class AgentPublishRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    evaluation_id: uuid.UUID
    experiment_run_id: uuid.UUID | None = None
    reason: str = Field("", max_length=1000)

    @field_validator("reason")
    @classmethod
    def _reason_contains_no_secret_material(cls, value: str) -> str:
        if redact_trace_text(value) != value:
            raise ValueError("release reason must not contain credentials or secrets")
        return value


class AgentRollbackRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    target_version_id: uuid.UUID
    reason: str = Field(..., min_length=1, max_length=1000)

    @field_validator("reason")
    @classmethod
    def _reason_contains_no_secret_material(cls, value: str) -> str:
        if redact_trace_text(value) != value:
            raise ValueError("rollback reason must not contain credentials or secrets")
        return value


class AgentMemberRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    role: AgentRole


class AgentSummary(BaseModel):
    model_config = ConfigDict(extra="ignore")

    tenant_id: str
    agent_id: str
    slug: str
    name: str
    description: str = ""
    owner_id: str
    status: AgentStatus
    caller_role: AgentRole
    draft_revision: int | None = None
    published_channels: list[AgentChannel] = Field(default_factory=list)
    created_at: datetime | str
    updated_at: datetime | str


class AgentDraftMetadata(BaseModel):
    model_config = ConfigDict(extra="ignore")

    revision: int
    schema_version: str
    spec_hash: str
    updated_at: datetime | str


class AgentDetail(AgentSummary):
    draft: AgentDraftMetadata | None = None
    archived_at: datetime | str | None = None
    deleted_at: datetime | str | None = None


class AgentDraftResponse(BaseModel):
    model_config = ConfigDict(extra="ignore")

    tenant_id: str
    draft_id: str
    agent_id: str
    revision: int
    schema_version: str
    spec: dict[str, Any]
    spec_hash: str
    updated_by: str
    created_at: datetime | str
    updated_at: datetime | str

    @field_validator("spec", mode="before")
    @classmethod
    def _redact_legacy_spec(cls, value: Any) -> dict[str, Any]:
        return redact_agent_spec_for_read(value if isinstance(value, dict) else {})


class AgentVersionResponse(BaseModel):
    model_config = ConfigDict(extra="ignore")

    tenant_id: str
    agent_version_id: str
    agent_id: str
    version_number: int
    schema_version: str
    spec: AgentSpec
    spec_hash: str
    source_draft_id: str
    source_draft_revision: int
    release_evaluation_id: str | None = None
    release_identity_hash: str | None = None
    created_by: str
    created_at: datetime | str

    @model_validator(mode="before")
    @classmethod
    def _normalize_resolved_spec(cls, value: Any) -> Any:
        if not isinstance(value, dict):
            return value
        normalized = dict(value)
        raw_spec = normalized.get("spec", normalized.get("resolved_spec"))
        normalized["spec"] = redact_agent_spec_for_read(raw_spec)
        return normalized


class AgentReleaseEvaluationEventResponse(BaseModel):
    model_config = ConfigDict(extra="ignore")

    event_id: str
    evaluation_id: str
    sequence: int
    status: AgentReleaseStatus
    summary: dict[str, Any] = Field(default_factory=dict)
    created_at: datetime | str


class AgentReleaseEvaluationResponse(BaseModel):
    model_config = ConfigDict(extra="ignore")

    tenant_id: str
    evaluation_id: str
    agent_id: str
    draft_id: str
    draft_revision: int
    spec_hash: str
    runtime_fingerprint: dict[str, Any]
    runtime_fingerprint_hash: str
    release_identity_hash: str
    evaluation_identity_hash: str | None = None
    profile_id: str
    profile_version: str
    dataset_id: str | None = None
    dataset_version: str | None = None
    dataset_manifest_hash: str | None = None
    experiment_run_id: str | None = None
    channel: AgentChannel
    auth_mode: AgentAuthMode
    channel_policy: AgentChannelPolicy
    channel_policy_hash: str
    status: AgentReleaseStatus
    stale: bool = False
    stale_reasons: list[str] = Field(default_factory=list)
    validation_snapshot: dict[str, Any] = Field(default_factory=dict)
    gate_snapshot: dict[str, Any] = Field(default_factory=dict)
    events: list[AgentReleaseEvaluationEventResponse] = Field(default_factory=list)
    created_by: str
    created_at: datetime | str
    started_at: datetime | str | None = None
    completed_at: datetime | str | None = None


class AgentReleaseEvaluationListResponse(BaseModel):
    evaluations: list[AgentReleaseEvaluationResponse] = Field(default_factory=list)


class AgentReleaseDiffResponse(BaseModel):
    model_config = ConfigDict(extra="ignore")

    evaluation_id: str
    draft_revision: int
    publication_id: str | None = None
    current_version_id: str | None = None
    current_version_number: int | None = None
    diff: dict[str, Any]


class AgentPublicationResponse(BaseModel):
    model_config = ConfigDict(extra="ignore")

    tenant_id: str
    publication_id: str
    agent_id: str
    channel: AgentChannel
    public_id: str
    version_id: str | None = None
    version_number: int | None = None
    version_spec_hash: str | None = None
    auth_mode: AgentAuthMode
    policy: dict[str, Any] = Field(default_factory=dict)
    expires_at: datetime | str | None = None
    status: Literal["draft", "active", "disabled", "degraded"]
    created_by: str
    updated_by: str
    created_at: datetime | str
    updated_at: datetime | str


class AgentPublishEventResponse(BaseModel):
    model_config = ConfigDict(extra="ignore")

    event_id: str
    publication_id: str
    agent_id: str
    from_version_id: str | None = None
    to_version_id: str
    actor_id: str
    reason: str = ""
    validation_snapshot: dict[str, Any] = Field(default_factory=dict)
    operation: Literal["promote", "rollback"] = "promote"
    release_evaluation_id: str | None = None
    request_hash: str | None = None
    created_at: datetime | str


class AgentReleaseMutationResponse(BaseModel):
    request_id: str
    version: AgentVersionResponse
    publication: AgentPublicationResponse
    event: AgentPublishEventResponse
    idempotent_replay: bool = False


class AgentMemberResponse(BaseModel):
    model_config = ConfigDict(extra="ignore")

    tenant_id: str
    agent_id: str
    principal_type: Literal["user", "group"]
    principal_id: str
    role: AgentRole
    created_by: str
    created_at: datetime | str
    updated_at: datetime | str


class AgentPageResponse(BaseModel):
    items: list[AgentSummary]
    next_cursor: str | None = None


class AgentValidationIssue(BaseModel):
    field: str
    code: str
    message: str


class AgentValidationResponse(BaseModel):
    valid: bool
    revision: int
    spec_hash: str
    errors: list[AgentValidationIssue]


class AgentMutationResponse(BaseModel):
    request_id: str
    agent: AgentSummary | AgentDetail


class AgentDraftMutationResponse(BaseModel):
    request_id: str
    draft: AgentDraftResponse


class AgentVersionMutationResponse(BaseModel):
    request_id: str
    version: AgentVersionResponse


class AgentMemberMutationResponse(BaseModel):
    request_id: str
    member: AgentMemberResponse


class AgentStatusResponse(BaseModel):
    request_id: str
    status: str


class AgentGovernancePolicyUpdate(BaseModel):
    model_config = ConfigDict(extra="forbid")

    trace_retention_days: int | None = Field(None, ge=1, le=3650)
    runtime_retention_days: int | None = Field(None, ge=1, le=3650)
    attachment_retention_days: int | None = Field(None, ge=1, le=3650)
    legal_hold: bool | None = None
    principal_requests_per_minute: int | None = Field(None, ge=1, le=1_000_000)
    principal_requests_per_day: int | None = Field(None, ge=1, le=100_000_000)
    ip_requests_per_minute: int | None = Field(None, ge=1, le=1_000_000)
    ip_requests_per_day: int | None = Field(None, ge=1, le=100_000_000)
    publication_requests_per_minute: int | None = Field(None, ge=1, le=1_000_000)
    publication_requests_per_day: int | None = Field(None, ge=1, le=100_000_000)
    max_agents_per_tenant: int | None = Field(None, ge=1, le=100_000)
    max_active_publications: int | None = Field(None, ge=1, le=100_000)
    max_concurrent_runs: int | None = Field(None, ge=1, le=1_000_000)
    max_daily_tokens: int | None = Field(None, ge=1, le=10_000_000_000_000)
    max_daily_mcp_calls: int | None = Field(None, ge=1, le=10_000_000_000)
    max_storage_bytes: int | None = Field(None, ge=1, le=10_000_000_000_000)
    alert_threshold_percent: int | None = Field(None, ge=1, le=100)


class AgentGovernancePolicyResponse(BaseModel):
    model_config = ConfigDict(extra="ignore")

    tenant_id: str
    agent_id: str
    trace_retention_days: int
    runtime_retention_days: int
    attachment_retention_days: int
    legal_hold: bool
    principal_requests_per_minute: int
    principal_requests_per_day: int
    ip_requests_per_minute: int
    ip_requests_per_day: int
    publication_requests_per_minute: int
    publication_requests_per_day: int
    max_agents_per_tenant: int
    max_active_publications: int
    max_concurrent_runs: int
    max_daily_tokens: int
    max_daily_mcp_calls: int
    max_storage_bytes: int
    alert_threshold_percent: int
    cache_epoch: int = 0
    updated_by: str
    created_at: datetime | str | None = None
    updated_at: datetime | str | None = None


class AgentOperationsTrace(BaseModel):
    model_config = ConfigDict(extra="ignore")

    trace_id: str
    agent_id: str
    agent_version_id: str | None = None
    publication_id: str | None = None
    channel: Literal["preview", "hosted", "embed", "api", "builtin"]
    session_id: str | None = None
    status: str
    model_id: str | None = None
    total_latency_ms: int = 0
    total_tokens: int = 0
    total_cost_cents: int = 0
    input_preview: str = ""
    output_preview: str = ""
    redaction_state: dict[str, Any] = Field(default_factory=dict)
    started_at: datetime | str | None = None
    created_at: datetime | str | None = None


class AgentAnalyticsResponse(BaseModel):
    model_config = ConfigDict(extra="ignore")

    agent_id: str
    caller_role: AgentRole
    metrics: dict[str, Any]
    traces: list[AgentOperationsTrace]
    total: int
    limit: int
    offset: int
    filters: dict[str, Any] = Field(default_factory=dict)


class AgentAuditEventResponse(BaseModel):
    model_config = ConfigDict(extra="ignore")

    id: int
    user_id: str | None = None
    action: str
    status: str
    agent_id: str
    agent_version_id: str | None = None
    publication_id: str | None = None
    channel: str | None = None
    request_summary: dict[str, Any] = Field(default_factory=dict)
    response_summary: dict[str, Any] = Field(default_factory=dict)
    redaction_state: dict[str, Any] = Field(default_factory=dict)
    created_at: datetime | str


class AgentAuditPageResponse(BaseModel):
    events: list[AgentAuditEventResponse]
    total: int
    limit: int
    offset: int


class AgentDataDeletionRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    scope: Literal["retention", "user", "tenant"]
    subject_user_id: str | None = Field(None, min_length=1, max_length=255)
    idempotency_key: str = Field(..., min_length=8, max_length=255)

    @model_validator(mode="after")
    def _validate_subject(self) -> AgentDataDeletionRequest:
        if (self.scope == "user") != bool(self.subject_user_id):
            raise ValueError("subject_user_id is required only for user scope")
        return self


class AgentDataDeletionResponse(BaseModel):
    model_config = ConfigDict(extra="ignore")

    deletion_id: str
    tenant_id: str
    agent_id: str
    scope: Literal["retention", "user", "tenant"]
    subject_user_id: str | None = None
    status: Literal["pending", "completed", "failed", "blocked"]
    deleted_counts: dict[str, Any] = Field(default_factory=dict)
    error_code: str | None = None
    requested_by: str
    requested_at: datetime | str
    attempt_count: int = 0
    last_attempt_at: datetime | str | None = None
    completed_at: datetime | str | None = None
    retryable: bool = False

    @model_validator(mode="after")
    def _derive_retryable(self) -> AgentDataDeletionResponse:
        self.retryable = self.status == "failed" and self.completed_at is None
        return self


class AgentCredentialRevocationResponse(BaseModel):
    request_id: str
    revoked: dict[str, int]


class AgentCacheInvalidationResponse(BaseModel):
    request_id: str
    cache_epoch: int
    deleted_cache_rows: int


class AgentErrorDetail(BaseModel):
    code: str
    message: str
    request_id: str
    current_revision: int | None = None
    errors: list[AgentValidationIssue] | None = None
    findings: list[dict[str, Any]] | None = None


class AgentErrorResponse(BaseModel):
    detail: AgentErrorDetail


__all__ = [name for name in globals() if name.startswith("Agent")]
