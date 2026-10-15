"""Gateway-side model-metadata facade.

Gateway no longer runs an in-process ``ModelRegistry`` (those live only
in a second execution service). For the remaining Gateway-side
uses — chat-stream permission check, health-provider enumeration,
and admin CRUD cache-refresh triggers — gateway queries the DB
directly via the existing ``ModelService`` / ``ProviderService``.

This class is the narrow read-only interface those routes depend on.
Everything here is async + DB-backed; no caching (per-request DB hit
is cheaper than the ~6 provider HTTP client allocations that the old
``ModelRegistry`` constructor did at every lifespan boot).
"""

from __future__ import annotations

from collections.abc import Collection
from typing import Any

from ai_gateway_core.logging import get_logger

logger = get_logger(__name__)


class GatewayModelMeta:
    """Narrow read-only facade over ModelService + ProviderService.

    Used by gateway routes that need LLM metadata (access levels,
    configured providers) without dialling the providers themselves.
    """

    def __init__(
        self,
        model_service: Any,
        provider_service: Any,
        *,
        runtime_configured_providers: Collection[str] = (),
    ) -> None:
        self.model_service = model_service
        self.provider_service = provider_service
        # This set reflects providers configured in the separate Assistant
        # process, not merely enabled administration rows in the Gateway DB.
        self._runtime_configured_providers = frozenset(
            str(provider_id).strip()
            for provider_id in runtime_configured_providers
            if str(provider_id).strip()
        )

    async def get_access_level(
        self, tenant_id: str, model_id: str, provider_id: str | None = None,
    ) -> str | None:
        """Return the model's access_level (public / premium / admin)
        or ``None`` if the model isn't in the DB. Used by
        ``_check_model_permission`` to authorize chat requests at the
        edge before starting a Runtime turn."""
        row = (
            await self.model_service.get_model(tenant_id, model_id, provider_id=provider_id)
            if provider_id else await self.model_service.get_model(tenant_id, model_id)
        )
        if not row:
            return None
        return row.get("access_level")

    async def is_provider_configured(
        self, tenant_id: str, provider_id: str
    ) -> bool:
        """Return whether an enabled provider has a runtime credential path.

        Providers may be configured either through the shared startup
        environment or by an encrypted tenant key resolved by Assistant for the
        current request.
        """
        row = await self.provider_service.get_provider(tenant_id, provider_id)
        if not row:
            return False
        return bool(
            row.get("is_enabled")
            and (
                row.get("has_api_key")
                or provider_id in self._runtime_configured_providers
            )
        )

    async def list_enabled_providers(self, tenant_id: str) -> list[str]:
        """Return the set of enabled provider_ids for a tenant."""
        rows = await self.provider_service.list_providers(
            tenant_id, include_disabled=False
        )
        return [r["provider_id"] for r in rows]

    async def count_enabled_models_by_provider(
        self, tenant_id: str
    ) -> dict[str, int]:
        """Return ``{provider_id: enabled_model_count}`` for health
        dashboards. Uses the models table directly — cheaper than
        round-tripping through another execution service."""
        rows = await self.model_service.list_models(
            tenant_id, include_disabled=False
        )
        counts: dict[str, int] = {}
        for r in rows:
            pid = r.get("provider_id")
            if pid:
                counts[pid] = counts.get(pid, 0) + 1
        return counts
