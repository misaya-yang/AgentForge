"""Provider-setup derivation shared by health and first-run onboarding.

Both ``/health/providers`` and ``/setup/state`` need the same answer to
"which providers are configured for this tenant" — enabled in the DB and
with a runtime credential path. Keeping that in one place means the health
dashboard and the console first-run flow never disagree about what counts
as configured.
"""

from __future__ import annotations


async def configured_providers(model_meta, tenant_id: str) -> list[str]:
    """Return provider ids configured for ``tenant_id``.

    A provider counts as configured when it is enabled for the tenant and
    ``model_meta`` reports a runtime credential path (startup environment or
    an encrypted tenant key). ``model_meta`` is the gateway's
    ``GatewayModelMeta`` facade (``request.app.state.model_meta``).
    """
    enabled = await model_meta.list_enabled_providers(tenant_id)
    configured: list[str] = []
    seen: set[str] = set()
    for raw_provider_id in enabled:
        provider_id = str(raw_provider_id or "").strip()
        if not provider_id or provider_id in seen:
            continue
        seen.add(provider_id)
        if await model_meta.is_provider_configured(
            tenant_id,
            provider_id,
        ):
            configured.append(provider_id)
    return configured
