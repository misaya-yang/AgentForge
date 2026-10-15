"""Gateway startup synchronization for provider and model metadata."""

from __future__ import annotations

import os
from collections.abc import Collection
from dataclasses import dataclass
from typing import Any

from .model_service import ModelService
from .provider_service import ProviderService


@dataclass(frozen=True, slots=True)
class ProviderSeedResult:
    """Provider sets discovered while seeding gateway startup state."""

    configured_providers: tuple[str, ...]
    runtime_configured_providers: frozenset[str]


# Providers whose endpoint selection is straightforward use the configured
# environment key and optional base URL. DashScope and Google are resolved via
# their per-domain helpers in ``seed_startup_providers``.
_DEFAULT_PROVIDER_CONFIGS: dict[str, dict[str, str | None]] = {
    "openai": {
        "display_name": "OpenAI",
        "api_type": "openai",
        "base_url": "https://api.openai.com",
        "env_key": "OPENAI_API_KEY",
        "env_base_url": "OPENAI_BASE_URL",
    },
    "anthropic": {
        "display_name": "Anthropic",
        "api_type": "anthropic",
        "base_url": "https://api.anthropic.com",
        "env_key": "ANTHROPIC_API_KEY",
        "env_base_url": "ANTHROPIC_BASE_URL",
    },
    "deepseek": {
        "display_name": "DeepSeek",
        "api_type": "openai",
        "base_url": "https://api.deepseek.com",
        "env_key": "DEEPSEEK_API_KEY",
        "env_base_url": "DEEPSEEK_BASE_URL",
    },
    "dashscope": {
        "display_name": "Qwen/DashScope",
        "api_type": "openai",
        "base_url": "https://dashscope.aliyuncs.com/compatible-mode",
        "env_key": "DASHSCOPE_API_KEY",
        "env_base_url": "DASHSCOPE_CHAT_BASE_URL",
    },
    "google": {
        "display_name": "Google Gemini",
        "api_type": "google",
        "base_url": "https://generativelanguage.googleapis.com",
        "env_key": "GEMINI_API_KEY",
        "env_base_url": None,
    },
    "google-vertex": {
        "display_name": "Google Vertex AI",
        "api_type": "google-vertex",
        "base_url": "https://aiplatform.googleapis.com",
        "env_key": "VERTEX_API_KEY",
        "env_base_url": None,
    },
}


async def seed_startup_providers(
    *,
    provider_service: ProviderService | None,
    legacy_dashscope_api_key: str,
    tenant_id: str = "default",
    log: Any,
) -> ProviderSeedResult:
    """Seed environment-derived providers and load configured database rows."""
    import ai_gateway_core.config as endpoint_config
    from ai_gateway_core.enums import ModelProvider

    configured_providers: list[str] = []
    # DB-only provider keys are not available to the separate Assistant process.
    runtime_configured_providers: set[str] = set()

    # Keep the legacy Vertex environment switches functional for one release.
    legacy_backend = os.environ.get("GOOGLE_API_BACKEND", "").strip().lower()
    legacy_models = os.environ.get("GOOGLE_VERTEX_MODELS", "").strip()
    if legacy_backend == "vertex":
        log.warning(
            "GOOGLE_API_BACKEND=vertex is deprecated. Add a 'google-vertex' "
            "provider in the Service Management UI (or set VERTEX_API_KEY "
            "so the default google-vertex provider is seeded at startup) "
            "and remove GOOGLE_API_BACKEND from your environment."
        )
    if legacy_models:
        log.warning(
            "GOOGLE_VERTEX_MODELS is deprecated. Use the 'google-vertex' "
            "provider instead — models configured under that provider route "
            "to Vertex without needing per-model env overrides."
        )

    for provider_id, config in _DEFAULT_PROVIDER_CONFIGS.items():
        env_key = config["env_key"]
        api_key = os.environ.get(env_key, "") if env_key else ""
        # Gateway-only legacy fallbacks may seed admin metadata, but cannot
        # prove execution readiness in the separate Assistant process.
        assistant_runtime_key = api_key
        env_base_url = config.get("env_base_url")
        base_url = os.environ.get(env_base_url) if env_base_url else None
        if not base_url:
            base_url = config["base_url"]
        google_backend = "ai_studio"

        if provider_id == "dashscope":
            resolved_key, resolved_url = endpoint_config.resolve_dashscope("chat")
            api_key = resolved_key
            assistant_runtime_key = resolved_key
            base_url = resolved_url
            if not api_key:
                api_key = legacy_dashscope_api_key

        if provider_id == "google":
            resolved_key, _resolved_url, google_backend = endpoint_config.resolve_google("chat")
            api_key = resolved_key
            assistant_runtime_key = resolved_key
            if google_backend == "vertex":
                base_url = None

        if api_key:
            try:
                ModelProvider(provider_id)
                configured_providers.append(provider_id)
                if assistant_runtime_key:
                    runtime_configured_providers.add(provider_id)
                if provider_id == "google" and google_backend == "vertex":
                    log.info("Google provider routed to Vertex (env-seeded)")
            except ValueError:
                log.warning(f"Unknown provider enum: {provider_id}")

        if provider_service:
            try:
                existing = await provider_service.get_provider(tenant_id, provider_id)
                if not existing:
                    await provider_service.create_provider(
                        tenant_id=tenant_id,
                        provider_id=provider_id,
                        display_name=config["display_name"],
                        api_type=config["api_type"],
                        base_url=base_url,
                        api_key=api_key if api_key else None,
                        is_enabled=True,
                    )
                    log.info(f"Created provider {provider_id} in database")
                elif api_key:
                    if (
                        existing.get("has_api_key")
                        and existing.get("api_type") == config["api_type"]
                        and (base_url is None or existing.get("base_url") == base_url)
                    ):
                        runtime_config = await provider_service.get_runtime_provider_config(
                            tenant_id, provider_id,
                        )
                        if runtime_config.get("api_key") == api_key:
                            # Encryption is nondeterministic. Re-encrypting an
                            # unchanged credential advances updated_at and
                            # invalidates model leases for still-running turns.
                            continue
                    await provider_service.update_provider(
                        tenant_id=tenant_id,
                        provider_id=provider_id,
                        api_key=api_key,
                        api_type=config["api_type"],
                        base_url=base_url,
                    )
                    log.info(f"Updated runtime configuration for provider {provider_id}")
            except Exception as exc:
                log.warning(f"Failed to sync provider {provider_id} to database: {exc}")

    if provider_service:
        try:
            db_providers = await provider_service.list_providers(
                tenant_id,
                include_disabled=False,
            )
            for provider in db_providers:
                provider_id = provider.get("provider_id", "")
                if provider_id in configured_providers:
                    continue
                if not provider.get("has_api_key"):
                    continue

                try:
                    ModelProvider(provider_id)
                    configured_providers.append(provider_id)
                    log.info(f"Provider {provider_id} loaded from database")
                except ValueError:
                    log.debug(f"Custom provider {provider_id} not in enum, skipping")
        except Exception as exc:
            log.warning(f"Failed to load providers from database: {exc}")

    return ProviderSeedResult(
        configured_providers=tuple(configured_providers),
        runtime_configured_providers=frozenset(runtime_configured_providers),
    )


async def sync_startup_model_catalog(
    *,
    provider_service: ProviderService | None,
    model_service: ModelService | None,
    configured_providers: Collection[str],
    tenant_id: str = "default",
    log: Any,
) -> None:
    """Synchronize trusted model catalog entries for startup providers."""
    if not (provider_service and model_service and configured_providers):
        return

    from . import model_catalog_sync

    sync_service = model_catalog_sync.ModelCatalogSyncService(provider_service, model_service)
    for provider_id in sorted(set(configured_providers)):
        try:
            result = await sync_service.sync_provider_models(
                tenant_id=tenant_id,
                provider_id=provider_id,
                discover=False,
            )
            created = len(result.get("created_models", []))
            updated = len(result.get("updated_models", []))
            if created or updated:
                log.info(
                    "Synced startup model catalog provider_id=%s created=%s updated=%s",
                    provider_id,
                    created,
                    updated,
                )
        except Exception as exc:
            log.warning(
                "Failed to sync startup model catalog for provider %s: %s",
                provider_id,
                exc,
            )
