"""Provider template catalog for guided LLM provider onboarding.

The catalog describes provider capabilities and mainstream model metadata.
It does not choose business/runtime models for services; services still select
from persisted ``llm_providers`` / ``llm_models`` rows.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass
from decimal import Decimal
from typing import Any

from ai_gateway_core.models import get_builtin_model_capabilities


@dataclass(frozen=True)
class CredentialField:
    """Credential or region field required by a provider template."""

    name: str
    label: str
    field_type: str = "password"
    required: bool = True
    placeholder: str | None = None


@dataclass(frozen=True)
class CatalogModel:
    """Trusted model metadata for one provider template."""

    model_id: str
    display_name: str
    context_window: int = 128000
    max_output_tokens: int = 4096
    supports_vision: bool = False
    supports_tools: bool = True
    input_price_per_1k: Decimal = Decimal("0")
    output_price_per_1k: Decimal = Decimal("0")
    access_level: str = "public"
    sort_order: int = 0

    def to_model_kwargs(self, *, provider_id: str | None = None) -> dict[str, Any]:
        result = {
            "model_id": self.model_id,
            "display_name": self.display_name,
            "context_window": self.context_window,
            "max_output_tokens": self.max_output_tokens,
            "supports_vision": self.supports_vision,
            "supports_tools": self.supports_tools,
            "input_price_per_1k": self.input_price_per_1k,
            "output_price_per_1k": self.output_price_per_1k,
            "access_level": self.access_level,
            "sort_order": self.sort_order,
        }
        if provider_id:
            result["catalog_capabilities"] = get_builtin_model_capabilities(
                provider_id,
                self.model_id,
            ) or {}
        return result

    def to_response(self, *, provider_id: str | None = None) -> dict[str, Any]:
        result = self.to_model_kwargs(provider_id=provider_id)
        result["input_price_per_1k"] = float(self.input_price_per_1k)
        result["output_price_per_1k"] = float(self.output_price_per_1k)
        return result


@dataclass(frozen=True)
class ProviderTemplate:
    """Template metadata used by the guided provider wizard."""

    template_id: str
    display_name: str
    default_provider_id: str
    api_type: str
    default_base_url: str
    credential_fields: tuple[CredentialField, ...]
    discovery_strategy: str
    default_models: tuple[CatalogModel, ...]
    default_metadata: tuple[tuple[str, Any], ...] = ()
    description: str = ""
    advanced: bool = False

    def to_response(self) -> dict[str, Any]:
        return {
            "template_id": self.template_id,
            "display_name": self.display_name,
            "description": self.description,
            "default_provider_id": self.default_provider_id,
            "api_type": self.api_type,
            "default_base_url": self.default_base_url,
            "credential_fields": [asdict(field) for field in self.credential_fields],
            "discovery_strategy": self.discovery_strategy,
            "default_models": [
                model.to_response(provider_id=self.default_provider_id)
                for model in self.default_models
            ],
            "default_metadata": dict(self.default_metadata),
            "advanced": self.advanced,
        }


DASHSCOPE_MODELS: tuple[CatalogModel, ...] = (
    CatalogModel(
        model_id="qwen3.8-flash",
        display_name="Qwen 3.8 Flash",
        context_window=1000000,
        max_output_tokens=131072,
        supports_tools=True,
        sort_order=140,
    ),
    CatalogModel(
        model_id="qwen3.7-plus",
        display_name="Qwen 3.7 Plus",
        context_window=1000000,
        max_output_tokens=65536,
        supports_tools=True,
        sort_order=130,
    ),
    CatalogModel(
        model_id="qwen3.7-max",
        display_name="Qwen 3.7 Max",
        context_window=1000000,
        max_output_tokens=8192,
        supports_tools=True,
        sort_order=120,
    ),
    CatalogModel(
        model_id="qwen3.6-plus",
        display_name="Qwen 3.6 Plus",
        context_window=128000,
        max_output_tokens=8192,
        supports_tools=True,
        sort_order=100,
    ),
    CatalogModel(
        model_id="qwen-plus",
        display_name="Qwen Plus",
        context_window=128000,
        max_output_tokens=8192,
        supports_tools=True,
        sort_order=90,
    ),
    CatalogModel(
        model_id="qwen-max",
        display_name="Qwen Max",
        context_window=32768,
        max_output_tokens=8192,
        supports_tools=True,
        sort_order=80,
    ),
    CatalogModel(
        model_id="qwen-turbo",
        display_name="Qwen Turbo",
        context_window=1000000,
        max_output_tokens=8192,
        supports_tools=True,
        sort_order=70,
    ),
)


GOOGLE_GEMINI_MODELS: tuple[CatalogModel, ...] = (
    CatalogModel(
        model_id="gemini-3.5-flash",
        display_name="Gemini3.5-flash",
        context_window=1000000,
        max_output_tokens=8192,
        supports_vision=True,
        supports_tools=True,
        sort_order=120,
    ),
    CatalogModel(
        model_id="gemini-3.1-pro-preview",
        display_name="Gemini 3.1 Pro",
        context_window=1000000,
        max_output_tokens=8192,
        supports_vision=True,
        supports_tools=True,
        sort_order=110,
    ),
    CatalogModel(
        model_id="gemini-3-flash-preview",
        display_name="Gemini 3 Flash",
        context_window=1000000,
        max_output_tokens=8192,
        supports_vision=True,
        supports_tools=True,
        sort_order=100,
    ),
    CatalogModel(
        model_id="gemini-2.5-flash",
        display_name="Gemini 2.5 Flash",
        context_window=1000000,
        max_output_tokens=8192,
        supports_vision=True,
        supports_tools=True,
        sort_order=90,
    ),
    CatalogModel(
        model_id="gemini-2.5-pro",
        display_name="Gemini 2.5 Pro",
        context_window=2000000,
        max_output_tokens=8192,
        supports_vision=True,
        supports_tools=True,
        sort_order=80,
    ),
)


VERTEX_GEMINI_MODELS: tuple[CatalogModel, ...] = (
    CatalogModel(
        model_id="gemini-3.5-flash",
        display_name="Gemini3.5-flash",
        context_window=1000000,
        max_output_tokens=8192,
        supports_vision=True,
        supports_tools=True,
        sort_order=120,
    ),
    CatalogModel(
        model_id="gemini-3.1-pro-preview",
        display_name="Gemini 3.1 Pro",
        context_window=1000000,
        max_output_tokens=8192,
        supports_vision=True,
        supports_tools=True,
        sort_order=110,
    ),
    CatalogModel(
        model_id="gemini-3-flash-preview",
        display_name="Gemini 3 Flash",
        context_window=1000000,
        max_output_tokens=8192,
        supports_vision=True,
        supports_tools=True,
        sort_order=100,
    ),
)


PROVIDER_TEMPLATES: tuple[ProviderTemplate, ...] = (
    ProviderTemplate(
        template_id="dashscope-cn",
        display_name="Qwen/DashScope China",
        description="Alibaba Cloud Model Studio China endpoint.",
        default_provider_id="dashscope-cn",
        api_type="openai",
        default_base_url="https://dashscope.aliyuncs.com/compatible-mode",
        credential_fields=(
            CredentialField(
                name="api_key",
                label="API Key",
                placeholder="sk-...",
            ),
        ),
        discovery_strategy="catalog",
        default_models=DASHSCOPE_MODELS,
        default_metadata=(("wire_protocol", "responses_v1"),),
    ),
    ProviderTemplate(
        template_id="dashscope-intl",
        display_name="Qwen/DashScope Intl",
        description="Alibaba Cloud Model Studio international endpoint.",
        default_provider_id="dashscope-intl",
        api_type="openai",
        default_base_url="https://dashscope-intl.aliyuncs.com/compatible-mode",
        credential_fields=(
            CredentialField(
                name="api_key",
                label="API Key",
                placeholder="sk-...",
            ),
        ),
        discovery_strategy="catalog",
        default_models=DASHSCOPE_MODELS,
        default_metadata=(("wire_protocol", "responses_v1"),),
    ),
    ProviderTemplate(
        template_id="google-ai-studio",
        display_name="Google Gemini",
        description="Google AI Studio Gemini API.",
        default_provider_id="google",
        api_type="google",
        default_base_url="https://generativelanguage.googleapis.com",
        credential_fields=(
            CredentialField(
                name="api_key",
                label="API Key",
                placeholder="AIzaSy...",
            ),
        ),
        discovery_strategy="google_ai_studio_models_list",
        default_models=GOOGLE_GEMINI_MODELS,
    ),
    ProviderTemplate(
        template_id="google-vertex",
        display_name="Google Vertex AI",
        description="Google Vertex AI Gemini via official Google auth.",
        default_provider_id="google-vertex",
        api_type="google-vertex",
        default_base_url="https://aiplatform.googleapis.com",
        credential_fields=(
            CredentialField(
                name="project",
                label="Project ID",
                field_type="text",
                placeholder="my-gcp-project",
            ),
            CredentialField(
                name="location",
                label="Location",
                field_type="text",
                placeholder="us-central1",
            ),
            CredentialField(
                name="api_key",
                label="Service account JSON",
                required=False,
                placeholder="Leave blank to use server ADC",
            ),
        ),
        discovery_strategy="vertex_best_effort",
        default_models=VERTEX_GEMINI_MODELS,
    ),
    ProviderTemplate(
        template_id="custom-openai-compatible",
        display_name="Custom OpenAI-Compatible",
        description="Advanced custom provider path.",
        default_provider_id="",
        api_type="openai",
        default_base_url="",
        credential_fields=(
            CredentialField(name="provider_id", label="Provider ID", field_type="text"),
            CredentialField(name="display_name", label="Display Name", field_type="text"),
            CredentialField(name="base_url", label="Base URL", field_type="text"),
            CredentialField(name="api_key", label="API Key", placeholder="sk-..."),
        ),
        discovery_strategy="openai_compatible_best_effort",
        default_models=(),
        advanced=True,
    ),
)


def list_provider_templates() -> list[ProviderTemplate]:
    """Return templates in UI display order."""
    return list(PROVIDER_TEMPLATES)


def get_provider_template(template_id: str) -> ProviderTemplate | None:
    """Find a provider template by template id."""
    normalized = template_id.strip().lower()
    return next((tpl for tpl in PROVIDER_TEMPLATES if tpl.template_id == normalized), None)


def find_provider_template_for_config(
    *,
    provider_id: str,
    api_type: str | None = None,
    base_url: str | None = None,
) -> ProviderTemplate | None:
    """Infer the best template for an existing provider row."""
    normalized_provider = provider_id.strip().lower()
    normalized_api_type = (api_type or "").strip().lower()
    normalized_base_url = (base_url or "").strip().lower()

    exact = next(
        (tpl for tpl in PROVIDER_TEMPLATES if tpl.default_provider_id == normalized_provider),
        None,
    )
    if exact:
        return exact

    if "dashscope-intl.aliyuncs.com" in normalized_base_url:
        return get_provider_template("dashscope-intl")
    if "dashscope.aliyuncs.com" in normalized_base_url or normalized_provider.startswith(
        "dashscope"
    ):
        return get_provider_template("dashscope-cn")
    if normalized_api_type in {"google", "google-ai-studio"}:
        return get_provider_template("google-ai-studio")
    if normalized_api_type in {"google-vertex", "vertex"}:
        return get_provider_template("google-vertex")
    return None


def known_catalog_model_provider_ids(model_id: str) -> set[str]:
    """Return provider ids whose templates know this model id."""
    normalized = model_id.strip().lower()
    provider_ids: set[str] = set()
    for template in PROVIDER_TEMPLATES:
        if any(model.model_id.lower() == normalized for model in template.default_models):
            provider_ids.add(template.default_provider_id)
    return provider_ids
