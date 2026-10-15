"""First-run onboarding state for the console setup flow.

``GET /setup/state`` answers one question for the frontend: has this
deployment got a usable model provider yet? The console uses it to show
the setup banner and the dashboard checklist until the operator configures
a provider in Services.

Auth: JWT **and** API-key (the CLI consumes this endpoint). Callers need the
Services-view capability so provider details are not exposed to ordinary users.
"""

from __future__ import annotations

from ai_gateway_core.enums import ModelProvider
from fastapi import APIRouter, Depends, HTTPException, Request

from ...config.settings import Settings
from ...core.auth.permissions import Capability
from ...services.llm.provider_setup import configured_providers
from ..deps import (
    AuthContext,
    get_auth_context,
    get_settings,
    require_gateway_capability,
)

router = APIRouter()


@router.get("/setup/state")
async def setup_state(
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
    settings: Settings = Depends(get_settings),
) -> dict:
    """Report whether a model provider is configured for the tenant.

    Returns:
        ``{"configured": bool, "missing": [provider_id, ...], "mode": str,
        "default_model": str}``. ``configured`` is true when at
        least one provider has a runtime credential path; ``missing`` lists
        the providers that are not configured; ``mode`` is the
        ``model_setup_mode`` setting (``ui`` | ``environment``);
        ``default_model`` is the effective deployment default — the
        ``DEFAULT_MODEL`` env var, or the platform code default when unset.
    """
    if not auth.is_authenticated:
        raise HTTPException(status_code=401, detail="Authentication required")
    require_gateway_capability(request, auth, Capability.SERVICE_LIST_READ)

    model_meta = getattr(request.app.state, "model_meta", None)
    tenant_id = auth.tenant_id or "default"
    configured = (
        await configured_providers(model_meta, tenant_id) if model_meta is not None else []
    )
    known = [provider.value for provider in ModelProvider]
    return {
        "configured": bool(configured),
        "missing": [provider_id for provider_id in known if provider_id not in configured],
        "mode": settings.model_setup_mode,
        "default_model": settings.default_model,
    }
