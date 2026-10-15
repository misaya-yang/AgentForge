"""
LLM Provider Management API.

REST endpoints for managing LLM providers.
"""

from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Query, Request

from ...core.auth.permissions import Capability, build_permission_denied_detail, check_capability
from ...core.auth.rbac import RBAC
from ...core.auth.user_resolver import UserContext
from ...services.llm.model_catalog_sync import ModelCatalogSyncService
from ...services.llm.model_failover import has_secret_field
from ...services.llm.model_service import ModelService
from ...services.llm.provider_health import (
    begin_provider_probe,
    invalidate_provider_probe,
    record_provider_probe,
)
from ...services.llm.provider_service import ProviderService
from ...services.llm.provider_templates import (
    get_provider_template,
    list_provider_templates,
)
from ...services.metrics.audit_event_writer import record_config_change
from ..deps import AuthContext, get_user_context
from ..schemas.providers import (
    ProviderCreate,
    ProviderFromTemplateCreate,
    ProviderModelSyncResult,
    ProviderResponse,
    ProviderTemplateResponse,
    ProviderUpdate,
)
from .models import _publish_model_config_changed

router = APIRouter()
_USER_CONTEXT_RBAC = RBAC(role_permissions={"admin": ["admin:*"]})


def _auth_from_user(user: UserContext) -> AuthContext:
    return AuthContext(
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        roles=list(user.roles),
        permissions=[],
        is_authenticated=user.is_authenticated,
    )


def _require_user_gateway_capability(user: UserContext, capability: Capability) -> None:
    decision = check_capability(
        rbac=_USER_CONTEXT_RBAC,
        roles=user.roles,
        permissions=[],
        capability=capability,
    )
    if decision.allowed:
        return
    raise HTTPException(
        status_code=403,
        detail=build_permission_denied_detail(capability=capability),
    )


def _safe_provider_metadata(metadata: dict[str, Any] | None) -> dict[str, Any] | None:
    if metadata is None:
        return None
    if has_secret_field(metadata):
        raise HTTPException(status_code=422, detail="PROVIDER_METADATA_MUST_NOT_CONTAIN_SECRETS")
    return {
        str(key): value
        for key, value in metadata.items()
        if str(key).strip() and value not in (None, "")
    }


def get_provider_service(request: Request) -> ProviderService:
    """Get ProviderService from app state."""
    svc = getattr(request.app.state, "provider_service", None)
    if svc is None:
        raise HTTPException(
            status_code=503,
            detail="Provider service is not initialized.",
        )
    return svc


def get_model_service(request: Request) -> ModelService:
    """Get ModelService from app state."""
    svc = getattr(request.app.state, "model_service", None)
    if svc is None:
        raise HTTPException(
            status_code=503,
            detail="Model service is not initialized.",
        )
    return svc


@router.get("/provider-templates", response_model=list[ProviderTemplateResponse])
async def list_templates(
    user: UserContext = Depends(get_user_context),
):
    """List guided provider templates for admin onboarding."""
    _require_user_gateway_capability(user, Capability.GATEWAY_PROVIDER_CONFIG_READ)
    return [template.to_response() for template in list_provider_templates()]


@router.get("/providers", response_model=list[ProviderResponse])
async def list_providers(
    include_disabled: bool = Query(False, description="Include disabled providers"),
    provider_service: ProviderService = Depends(get_provider_service),
    user: UserContext = Depends(get_user_context),
):
    """List all providers for the tenant."""
    _require_user_gateway_capability(user, Capability.GATEWAY_PROVIDER_CONFIG_READ)
    # Only admin can see disabled providers
    if include_disabled and "admin" not in user.roles:
        include_disabled = False

    providers = await provider_service.list_providers(
        tenant_id=user.tenant_id or "default",
        include_disabled=include_disabled,
    )
    return providers


@router.post("/providers", response_model=ProviderResponse)
async def create_provider(
    body: ProviderCreate,
    request: Request = None,
    provider_service: ProviderService = Depends(get_provider_service),
    user: UserContext = Depends(get_user_context),
):
    """Create a new provider."""
    _require_user_gateway_capability(user, Capability.GATEWAY_PROVIDER_CONFIG_WRITE)

    try:
        provider = await provider_service.create_provider(
            tenant_id=user.tenant_id or "default",
            provider_id=body.provider_id,
            display_name=body.display_name,
            api_type=body.api_type,
            base_url=body.base_url,
            api_key=body.api_key,
            metadata=_safe_provider_metadata(body.metadata),
            is_enabled=body.is_enabled,
        )
        if request is not None:
            invalidate_provider_probe(request, user.tenant_id or "default", body.provider_id)
            await record_config_change(
                request=request,
                auth=_auth_from_user(user),
                resource_type="provider",
                resource_id=body.provider_id,
                action="create",
                before=None,
                after=body.model_dump(),
            )
        return provider
    except Exception as e:
        if "duplicate key" in str(e).lower():
            raise HTTPException(status_code=409, detail="Provider already exists")
        raise HTTPException(status_code=500, detail=str(e))


@router.post("/providers/from-template", response_model=ProviderResponse)
async def create_provider_from_template(
    body: ProviderFromTemplateCreate,
    request: Request = None,
    provider_service: ProviderService = Depends(get_provider_service),
    user: UserContext = Depends(get_user_context),
):
    """Create or update a provider from a guided template."""
    _require_user_gateway_capability(user, Capability.GATEWAY_PROVIDER_CONFIG_WRITE)

    template = get_provider_template(body.template_id)
    if not template:
        raise HTTPException(status_code=404, detail="Provider template not found")

    provider_id = (body.provider_id or template.default_provider_id).strip()
    display_name = (body.display_name or template.display_name).strip()
    base_url = (body.base_url or template.default_base_url).strip() or None

    if template.advanced:
        if not provider_id or not display_name or not base_url:
            raise HTTPException(
                status_code=422,
                detail="ADVANCED_TEMPLATE_REQUIRES_PROVIDER_ID_DISPLAY_NAME_BASE_URL",
            )
    elif body.provider_id and body.provider_id != template.default_provider_id:
        raise HTTPException(
            status_code=422,
            detail="TEMPLATE_PROVIDER_ID_IS_FIXED_FOR_MAINSTREAM_PROVIDERS",
        )

    tenant_id = user.tenant_id or "default"
    metadata = {
        **dict(template.default_metadata),
        **(_safe_provider_metadata(body.metadata) or {}),
    }
    existing = await provider_service.get_provider(tenant_id, provider_id)
    if existing:
        provider = await provider_service.update_provider(
            tenant_id=tenant_id,
            provider_id=provider_id,
            display_name=display_name,
            api_type=template.api_type,
            base_url=base_url,
            api_key=body.api_key,
            metadata=metadata,
            is_enabled=body.is_enabled,
        )
        if not provider:
            raise HTTPException(status_code=404, detail="Provider not found")
        if request is not None:
            await record_config_change(
                request=request,
                auth=_auth_from_user(user),
                resource_type="provider",
                resource_id=provider_id,
                action="update_from_template",
                before={"template_id": body.template_id},
                after=body.model_dump(),
            )
        return provider

    try:
        provider = await provider_service.create_provider(
            tenant_id=tenant_id,
            provider_id=provider_id,
            display_name=display_name,
            api_type=template.api_type,
            base_url=base_url,
            api_key=body.api_key,
            metadata=metadata,
            is_enabled=body.is_enabled,
        )
        if request is not None:
            invalidate_provider_probe(request, user.tenant_id or "default", provider_id)
            await record_config_change(
                request=request,
                auth=_auth_from_user(user),
                resource_type="provider",
                resource_id=provider_id,
                action="create_from_template",
                before={"template_id": body.template_id},
                after=body.model_dump(),
            )
        return provider
    except Exception as e:
        if "duplicate key" in str(e).lower():
            raise HTTPException(status_code=409, detail="Provider already exists")
        raise HTTPException(status_code=500, detail=str(e))


@router.get("/providers/{provider_id}", response_model=ProviderResponse)
async def get_provider(
    provider_id: str,
    provider_service: ProviderService = Depends(get_provider_service),
    user: UserContext = Depends(get_user_context),
):
    """Get a specific provider."""
    _require_user_gateway_capability(user, Capability.GATEWAY_PROVIDER_CONFIG_READ)
    provider = await provider_service.get_provider(
        tenant_id=user.tenant_id or "default",
        provider_id=provider_id,
    )
    if not provider:
        raise HTTPException(status_code=404, detail="Provider not found")
    return provider


@router.put("/providers/{provider_id}", response_model=ProviderResponse)
async def update_provider(
    provider_id: str,
    body: ProviderUpdate,
    request: Request = None,
    provider_service: ProviderService = Depends(get_provider_service),
    user: UserContext = Depends(get_user_context),
):
    """Update a provider."""
    _require_user_gateway_capability(user, Capability.GATEWAY_PROVIDER_CONFIG_WRITE)

    provider = await provider_service.update_provider(
        tenant_id=user.tenant_id or "default",
        provider_id=provider_id,
        display_name=body.display_name,
        api_type=body.api_type,
        base_url=body.base_url,
        api_key=body.api_key,
        metadata=_safe_provider_metadata(body.metadata),
        is_enabled=body.is_enabled,
    )
    if not provider:
        raise HTTPException(status_code=404, detail="Provider not found")
    if request is not None:
        invalidate_provider_probe(request, user.tenant_id or "default", provider_id)
        await record_config_change(
            request=request,
            auth=_auth_from_user(user),
            resource_type="provider",
            resource_id=provider_id,
            action="update",
            before=None,
            after=body.model_dump(exclude_none=True),
        )
    return provider


@router.delete("/providers/{provider_id}")
async def delete_provider(
    provider_id: str,
    request: Request = None,
    provider_service: ProviderService = Depends(get_provider_service),
    user: UserContext = Depends(get_user_context),
):
    """Delete a provider and all its models."""
    _require_user_gateway_capability(user, Capability.GATEWAY_PROVIDER_CONFIG_WRITE)

    deleted = await provider_service.delete_provider(
        tenant_id=user.tenant_id or "default",
        provider_id=provider_id,
    )
    if not deleted:
        raise HTTPException(status_code=404, detail="Provider not found")
    if request is not None:
        invalidate_provider_probe(request, user.tenant_id or "default", provider_id)
        await record_config_change(
            request=request,
            auth=_auth_from_user(user),
            resource_type="provider",
            resource_id=provider_id,
            action="delete",
            before={"provider_id": provider_id},
            after=None,
        )
    return {"provider_id": provider_id, "status": "deleted"}


@router.post("/providers/{provider_id}/test")
async def test_provider_connection(
    provider_id: str,
    request: Request = None,
    provider_service: ProviderService = Depends(get_provider_service),
    user: UserContext = Depends(get_user_context),
):
    """Test API connection for a provider."""
    _require_user_gateway_capability(user, Capability.GATEWAY_PROVIDER_CONFIG_WRITE)

    tenant_id = user.tenant_id or "default"
    probe_id = begin_provider_probe(request, tenant_id, provider_id)
    result = await provider_service.test_connection(
        tenant_id=tenant_id,
        provider_id=provider_id,
    )
    record_provider_probe(request, tenant_id, provider_id, result, probe_id=probe_id)
    return result


@router.post("/providers/{provider_id}/models/sync", response_model=ProviderModelSyncResult)
async def sync_provider_models(
    provider_id: str,
    request: Request = None,
    provider_service: ProviderService = Depends(get_provider_service),
    model_service: ModelService = Depends(get_model_service),
    user: UserContext = Depends(get_user_context),
):
    """Sync provider-supported models into llm_models/model_pricing."""
    _require_user_gateway_capability(user, Capability.GATEWAY_PROVIDER_CONFIG_WRITE)

    sync_service = ModelCatalogSyncService(provider_service, model_service)
    try:
        result = await sync_service.sync_provider_models(
            tenant_id=user.tenant_id or "default",
            provider_id=provider_id,
        )
    except ValueError as e:
        if str(e) == "PROVIDER_NOT_FOUND":
            raise HTTPException(status_code=404, detail="Provider not found")
        raise HTTPException(status_code=400, detail=str(e))

    # Catalog sync can change effective capability profiles (new models or a
    # capability_revision bump).  Publish exact invalidations so the Assistant
    # does not serve stale profiles until its bounded TTL expires.
    for model in result.get("capability_changed_models", []):
        await _publish_model_config_changed(
            request,
            tenant_id=user.tenant_id or "default",
            model_id=str(model.get("model_id") or ""),
            provider_id=str(model.get("provider_id") or provider_id),
        )
    return result
