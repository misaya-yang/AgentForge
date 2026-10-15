"""Provider-scoped model catalog synchronization."""

from __future__ import annotations

from typing import Any

import httpx
from ai_gateway_core.logging import get_logger
from ai_gateway_core.models import ModelCapabilityError

from .model_service import ModelService
from .provider_service import ProviderService
from .provider_templates import (
    CatalogModel,
    ProviderTemplate,
    find_provider_template_for_config,
)

logger = get_logger(__name__)


class ModelCatalogSyncService:
    """Sync trusted provider catalog entries into ``llm_models``."""

    def __init__(
        self,
        provider_service: ProviderService,
        model_service: ModelService,
        *,
        http_client: httpx.AsyncClient | None = None,
    ):
        self.provider_service = provider_service
        self.model_service = model_service
        self._http_client = http_client

    async def sync_provider_models(
        self,
        *,
        tenant_id: str,
        provider_id: str,
        discover: bool = True,
    ) -> dict[str, Any]:
        provider = await self.provider_service.get_provider(tenant_id, provider_id)
        if not provider:
            raise ValueError("PROVIDER_NOT_FOUND")

        template = find_provider_template_for_config(
            provider_id=provider_id,
            api_type=provider.get("api_type"),
            base_url=provider.get("base_url"),
        )
        if template is None:
            return {
                "provider_id": provider_id,
                "template_id": None,
                "created_models": [],
                "updated_models": [],
                "skipped_models": [],
                "discovery_warnings": [
                    f"No trusted provider template is available for {provider_id}."
                ],
            }

        catalog_by_id = {entry.model_id: entry for entry in template.default_models}
        skipped_models: list[dict[str, str]] = []
        discovery_warnings: list[str] = []

        discovered: list[dict[str, Any]] = []
        if discover:
            discovered = await self._discover_models(
                tenant_id=tenant_id,
                provider=provider,
                template=template,
            )
        for item in discovered:
            model_id = item.get("model_id")
            if not model_id:
                continue
            if model_id not in catalog_by_id:
                skipped_models.append(
                    {
                        "model_id": model_id,
                        "reason": "discovered_model_not_in_trusted_catalog",
                    }
                )
                continue
            catalog_by_id[model_id] = self._merge_discovered_metadata(
                catalog_by_id[model_id],
                item,
            )

        if discover and template.discovery_strategy in {
            "google_ai_studio_models_list",
            "vertex_best_effort",
        } and not discovered:
            discovery_warnings.append(
                f"{template.display_name} discovery returned no usable catalog models; used trusted catalog defaults."
            )

        created_models: list[dict[str, Any]] = []
        updated_models: list[dict[str, Any]] = []
        capability_changed_models: list[dict[str, Any]] = []

        for entry in sorted(catalog_by_id.values(), key=lambda item: (-item.sort_order, item.model_id)):
            try:
                status, model, capability_changed = await self.model_service.upsert_model_from_catalog(
                    tenant_id=tenant_id,
                    provider_id=provider_id,
                    **entry.to_model_kwargs(provider_id=provider_id),
                )
            except ModelCapabilityError as exc:
                # A single model whose existing tenant overrides conflict with
                # the refreshed catalog must not wedge the whole provider sync.
                # Skip it and continue with the remaining models.
                skipped_models.append(
                    {
                        "model_id": entry.model_id,
                        "reason": f"capability_override_conflict: {exc}",
                    }
                )
                logger.warning(
                    "Skipped catalog sync for model_id=%s provider_id=%s due to capability conflict: %s",
                    entry.model_id,
                    provider_id,
                    exc,
                )
                continue
            payload = {
                "model_id": model["model_id"],
                "provider_id": model["provider_id"],
                "display_name": model["display_name"],
                "is_enabled": model["is_enabled"],
            }
            if status == "created":
                created_models.append(payload)
            else:
                updated_models.append(payload)
            if capability_changed:
                capability_changed_models.append(payload)

        logger.info(
            "Synced model catalog provider_id=%s template_id=%s created=%s updated=%s skipped=%s capability_changed=%s",
            provider_id,
            template.template_id,
            len(created_models),
            len(updated_models),
            len(skipped_models),
            len(capability_changed_models),
        )

        return {
            "provider_id": provider_id,
            "template_id": template.template_id,
            "created_models": created_models,
            "updated_models": updated_models,
            "skipped_models": skipped_models,
            "discovery_warnings": discovery_warnings,
            # Internal-only: models whose effective capability profile changed.
            # Not part of the public ``ProviderModelSyncResult`` schema; the
            # sync endpoint reads it to publish exact invalidations and FastAPI
            # drops it when serializing the response.
            "capability_changed_models": capability_changed_models,
        }

    async def _discover_models(
        self,
        *,
        tenant_id: str,
        provider: dict[str, Any],
        template: ProviderTemplate,
    ) -> list[dict[str, Any]]:
        if template.discovery_strategy == "google_ai_studio_models_list":
            return await self._discover_google_ai_studio_models(tenant_id, provider)
        if template.discovery_strategy == "vertex_best_effort":
            return await self._discover_vertex_models(tenant_id, provider)
        return []

    async def _discover_google_ai_studio_models(
        self,
        tenant_id: str,
        provider: dict[str, Any],
    ) -> list[dict[str, Any]]:
        api_key = await self._get_runtime_api_key(tenant_id, provider["provider_id"])
        if not api_key:
            return []

        base_url = str(provider.get("base_url") or "https://generativelanguage.googleapis.com")
        url = f"{base_url.rstrip('/')}/v1beta/models"
        try:
            response = await self._get(url, params={"key": api_key})
            if response.status_code != 200:
                logger.warning(
                    "Google AI Studio model discovery failed provider_id=%s status=%s",
                    provider["provider_id"],
                    response.status_code,
                )
                return []
            return self._parse_google_models(response.json())
        except Exception as exc:
            logger.warning(
                "Google AI Studio model discovery error provider_id=%s error=%s",
                provider["provider_id"],
                exc,
            )
            return []

    async def _discover_vertex_models(
        self,
        tenant_id: str,
        provider: dict[str, Any],
    ) -> list[dict[str, Any]]:
        api_key = await self._get_runtime_api_key(tenant_id, provider["provider_id"])
        if not api_key or api_key.lstrip().startswith("{"):
            return []

        base_url = str(provider.get("base_url") or "https://aiplatform.googleapis.com")
        url = f"{base_url.rstrip('/')}/v1/publishers/google/models"
        try:
            response = await self._get(url, params={"key": api_key})
            if response.status_code != 200:
                logger.warning(
                    "Vertex model discovery failed provider_id=%s status=%s",
                    provider["provider_id"],
                    response.status_code,
                )
                return []
            return self._parse_google_models(response.json())
        except Exception as exc:
            logger.warning(
                "Vertex model discovery error provider_id=%s error=%s",
                provider["provider_id"],
                exc,
            )
            return []

    async def _get(self, url: str, *, params: dict[str, str]) -> httpx.Response:
        if self._http_client is not None:
            return await self._http_client.get(url, params=params, timeout=10.0)
        async with httpx.AsyncClient(timeout=10.0) as client:
            return await client.get(url, params=params)

    async def _get_runtime_api_key(self, tenant_id: str, provider_id: str) -> str | None:
        runtime_config = await self.provider_service.get_runtime_provider_config(
            tenant_id,
            provider_id,
        )
        api_key = runtime_config.get("api_key")
        return str(api_key) if api_key else None

    @staticmethod
    def _parse_google_models(payload: dict[str, Any]) -> list[dict[str, Any]]:
        items: list[dict[str, Any]] = []
        for row in payload.get("models", []):
            name = str(row.get("name") or "").strip()
            model_id = name.split("/")[-1] if name else ""
            if not model_id:
                continue
            supported_methods = row.get("supportedGenerationMethods") or []
            if supported_methods and "generateContent" not in supported_methods:
                continue
            items.append(
                {
                    "model_id": model_id,
                    "display_name": row.get("displayName") or model_id,
                    "context_window": row.get("inputTokenLimit"),
                    "max_output_tokens": row.get("outputTokenLimit"),
                }
            )
        return items

    @staticmethod
    def _merge_discovered_metadata(
        base: CatalogModel,
        discovered: dict[str, Any],
    ) -> CatalogModel:
        return CatalogModel(
            model_id=base.model_id,
            display_name=str(discovered.get("display_name") or base.display_name),
            context_window=int(discovered.get("context_window") or base.context_window),
            max_output_tokens=int(discovered.get("max_output_tokens") or base.max_output_tokens),
            supports_vision=base.supports_vision,
            supports_tools=base.supports_tools,
            input_price_per_1k=base.input_price_per_1k,
            output_price_per_1k=base.output_price_per_1k,
            access_level=base.access_level,
            sort_order=base.sort_order,
        )
