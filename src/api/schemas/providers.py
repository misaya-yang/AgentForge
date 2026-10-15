"""
Provider and Model API Schemas.

Pydantic models for LLM provider and model management endpoints.
"""

from datetime import datetime
from decimal import Decimal
from typing import Any

from pydantic import BaseModel, Field

# ============================================================================
# Provider Schemas
# ============================================================================


class ProviderBase(BaseModel):
    """Base provider fields."""

    provider_id: str = Field(
        ..., min_length=1, max_length=50, description="Unique provider identifier"
    )
    display_name: str = Field(..., min_length=1, max_length=100, description="Display name")
    api_type: str = Field(
        default="openai",
        description="API type: openai, anthropic, google, or google-vertex",
    )
    base_url: str | None = Field(None, max_length=500, description="Custom API base URL")
    metadata: dict[str, Any] = Field(default_factory=dict, description="Safe provider metadata")
    is_enabled: bool = Field(default=True, description="Whether the provider is enabled")


class ProviderCreate(ProviderBase):
    """Request to create a provider."""

    api_key: str | None = Field(None, description="API key (will be encrypted)")


class ProviderUpdate(BaseModel):
    """Request to update a provider."""

    display_name: str | None = Field(None, min_length=1, max_length=100)
    api_type: str | None = None
    base_url: str | None = Field(None, max_length=500)
    metadata: dict[str, Any] | None = Field(None, description="Safe provider metadata")
    api_key: str | None = Field(None, description="New API key (will be encrypted)")
    is_enabled: bool | None = None


class ProviderResponse(ProviderBase):
    """Provider response (without API key)."""

    tenant_id: str
    has_api_key: bool = Field(description="Whether API key is configured")
    allow_environment_credentials: bool = Field(
        default=False,
        description="Whether the provider can use server-side environment credentials",
    )
    created_at: datetime
    updated_at: datetime

    class Config:
        from_attributes = True


class ProviderListResponse(BaseModel):
    """List of providers response."""

    providers: list[ProviderResponse]
    total: int


class ProviderTestResult(BaseModel):
    """Result of testing provider connection."""

    success: bool
    message: str
    latency_ms: int | None = None


class ProviderTemplateCredentialField(BaseModel):
    """Credential field exposed by a provider template."""

    name: str
    label: str
    field_type: str = "password"
    required: bool = True
    placeholder: str | None = None


class ProviderTemplateModel(BaseModel):
    """Trusted model metadata exposed by a provider template."""

    model_id: str
    display_name: str
    context_window: int
    max_output_tokens: int
    supports_vision: bool
    supports_tools: bool
    input_price_per_1k: float
    output_price_per_1k: float
    access_level: str
    sort_order: int


class ProviderTemplateResponse(BaseModel):
    """Provider template response for guided onboarding."""

    template_id: str
    display_name: str
    description: str
    default_provider_id: str
    api_type: str
    default_base_url: str
    credential_fields: list[ProviderTemplateCredentialField]
    discovery_strategy: str
    default_models: list[ProviderTemplateModel]
    default_metadata: dict[str, Any] = Field(default_factory=dict)
    advanced: bool = False


class ProviderFromTemplateCreate(BaseModel):
    """Request to create or update a provider from a template."""

    template_id: str = Field(..., min_length=1)
    provider_id: str | None = Field(None, min_length=1, max_length=50)
    display_name: str | None = Field(None, min_length=1, max_length=100)
    base_url: str | None = Field(None, max_length=500)
    metadata: dict[str, Any] | None = Field(None, description="Safe provider metadata")
    api_key: str | None = Field(None, description="API key (will be encrypted)")
    is_enabled: bool = True


class ProviderModelSyncSkipped(BaseModel):
    """A discovered model that was not enabled by sync."""

    model_id: str
    reason: str


class ProviderModelSyncItem(BaseModel):
    """A model created or updated by provider model sync."""

    model_id: str
    provider_id: str
    display_name: str
    is_enabled: bool


class ProviderModelSyncResult(BaseModel):
    """Provider model sync response."""

    provider_id: str
    template_id: str | None = None
    created_models: list[ProviderModelSyncItem]
    updated_models: list[ProviderModelSyncItem]
    skipped_models: list[ProviderModelSyncSkipped]
    discovery_warnings: list[str]


# ============================================================================
# Model Schemas
# ============================================================================


class ModelBase(BaseModel):
    """Base model fields."""

    model_id: str = Field(
        ..., min_length=1, max_length=100, description="Model identifier for API calls"
    )
    provider_id: str = Field(
        ..., min_length=1, max_length=50, description="Provider this model belongs to"
    )
    display_name: str = Field(..., min_length=1, max_length=100, description="Display name")
    context_window: int = Field(default=128000, ge=1, description="Context window size in tokens")
    max_output_tokens: int = Field(default=4096, ge=1, description="Maximum output tokens")
    supports_vision: bool = Field(
        default=False, description="Whether the model supports vision/images"
    )
    supports_tools: bool = Field(
        default=True, description="Whether the model supports tool calling"
    )
    input_price_per_1k: Decimal = Field(
        default=Decimal("0"), ge=0, description="Input price per 1K tokens in USD"
    )
    output_price_per_1k: Decimal = Field(
        default=Decimal("0"), ge=0, description="Output price per 1K tokens in USD"
    )
    access_level: str = Field(
        default="public", description="Access level: public, premium, or admin"
    )
    is_enabled: bool = Field(default=True, description="Whether the model is enabled")
    sort_order: int = Field(default=0, description="Sort order for UI display")
    capability_overrides: dict[str, Any] = Field(
        default_factory=dict,
        description="Tenant capability profile overrides merged over the provider catalog",
    )


class ModelCreate(ModelBase):
    """Request to create a model."""

    pass


class ModelUpdate(BaseModel):
    """Request to update a model.

    ``model_id`` is renameable — the frontend now exposes it in the edit
    dialog so a provider's model rename (e.g. ``gemini-3-flash-preview`` →
    ``gemini-3.1-flash``) doesn't require delete+recreate. Server-side
    rename pushes an UPDATE on the primary key; the pricing-table sync
    also follows the new id.
    """

    model_id: str | None = Field(None, min_length=1, max_length=100)
    display_name: str | None = Field(None, min_length=1, max_length=100)
    context_window: int | None = Field(None, ge=1)
    max_output_tokens: int | None = Field(None, ge=1)
    supports_vision: bool | None = None
    supports_tools: bool | None = None
    input_price_per_1k: Decimal | None = Field(None, ge=0)
    output_price_per_1k: Decimal | None = Field(None, ge=0)
    access_level: str | None = None
    is_enabled: bool | None = None
    sort_order: int | None = None
    capability_overrides: dict[str, Any] | None = None
    expected_capability_revision: int | None = Field(default=None, ge=1)


class ModelResponse(ModelBase):
    """Model response."""

    tenant_id: str
    catalog_capabilities: dict[str, Any] = Field(default_factory=dict)
    effective_capabilities: dict[str, Any] = Field(default_factory=dict)
    capability_revision: int = Field(default=1, ge=1)
    created_at: datetime
    updated_at: datetime

    class Config:
        from_attributes = True


class ModelListResponse(BaseModel):
    """List of models response."""

    models: list[ModelResponse]
    total: int


class ModelToggleRequest(BaseModel):
    """Request to toggle model enabled state."""

    is_enabled: bool
