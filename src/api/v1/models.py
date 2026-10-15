"""
LLM Model Management API — admin CRUD.

Phase 5e: gateway no longer owns an in-process ``ModelRegistry``.
Admin writes (create/update/delete/toggle) persist to the
``llm_models`` table via ``ModelService``; Runtime request construction
ModelRegistry refreshes on demand on the next request (loads
lazily from the DB), so the old gateway-side
``load_models_from_database`` cache-refresh calls are no-ops here.
"""

import json
from decimal import Decimal

from ai_gateway_core.models import (
    ModelCapabilityError,
    get_model_capability_adapter,
    list_model_capability_adapters,
)
from fastapi import APIRouter, Depends, HTTPException, Query, Request

from ...core.auth.permissions import Capability, build_permission_denied_detail, check_capability
from ...core.auth.rbac import RBAC
from ...core.auth.user_resolver import UserContext
from ...services.llm.model_service import ModelCapabilityRevisionConflict, ModelService
from ...services.metrics.audit_event_writer import record_config_change
from ..deps import AuthContext, get_user_context
from ..schemas.providers import (
    ModelCreate,
    ModelResponse,
    ModelUpdate,
)

router = APIRouter()
_USER_CONTEXT_RBAC = RBAC(role_permissions={"admin": ["admin:*"]})
_MODEL_CONFIG_CHANNEL = "gateway:model-config:changed:v1"


async def _publish_model_config_changed(
    request: Request | None,
    *,
    tenant_id: str,
    model_id: str,
    provider_id: str | None,
) -> None:
    if request is None:
        return
    storage = getattr(request.app.state, "redis", None)
    getter = getattr(storage, "get_native_client", None)
    client = getter() if callable(getter) else storage
    publish = getattr(client, "publish", None)
    if not callable(publish):
        return
    try:
        await publish(
            _MODEL_CONFIG_CHANNEL,
            json.dumps(
                {
                    "tenant_id": tenant_id,
                    "model_id": model_id,
                    "provider_id": provider_id,
                },
                separators=(",", ":"),
            ),
        )
    except Exception:
        # The Assistant has a bounded TTL fallback; config writes must not fail
        # merely because the optional invalidation channel is unavailable.
        return


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


def _redact_capability_layers_for_non_admin(
    models: list[dict], user: UserContext
) -> list[dict]:
    """Hide the catalog/override config layers from non-admin callers.

    ADR-005 keeps capability profile editing admin-only; the raw layers are
    admin configuration, not runtime data.  Non-admin readers keep
    ``effective_capabilities`` (what the model actually offers) but never see
    the tenant override layer or the catalog snapshot it merges over.
    """
    if "admin" in user.roles:
        return models
    for model in models:
        model.pop("catalog_capabilities", None)
        model.pop("capability_overrides", None)
    return models


def get_model_service(request: Request) -> ModelService:
    """Get ModelService from app state."""
    svc = getattr(request.app.state, "model_service", None)
    if svc is None:
        raise HTTPException(
            status_code=503,
            detail="Model service is not initialized.",
        )
    return svc


@router.get("/model-capability-adapters")
async def list_capability_adapters(
    user: UserContext = Depends(get_user_context),
):
    """Return the typed adapter catalog used by the model management form."""

    del user
    return list_model_capability_adapters()


@router.get("/model-capability-adapters/{adapter_id:path}/schema")
async def get_capability_adapter_schema(
    adapter_id: str,
    user: UserContext = Depends(get_user_context),
):
    del user
    adapter = get_model_capability_adapter(adapter_id)
    if adapter is None:
        raise HTTPException(status_code=404, detail="Capability adapter not found")
    return adapter


@router.get("/models", response_model=list[ModelResponse])
async def list_models(
    provider_id: str | None = Query(None, description="Filter by provider"),
    include_disabled: bool = Query(False, description="Include disabled models"),
    model_service: ModelService = Depends(get_model_service),
    user: UserContext = Depends(get_user_context),
):
    """List all models for the tenant."""
    if "admin" in user.roles:
        access_level = "admin"
    elif "premium" in user.roles or "vip" in user.roles:
        access_level = "premium"
    else:
        access_level = "public"

    if include_disabled and "admin" not in user.roles:
        include_disabled = False

    models = await model_service.list_models(
        tenant_id=user.tenant_id or "default",
        provider_id=provider_id,
        include_disabled=include_disabled,
        access_level=access_level if not include_disabled else None,
    )
    return _redact_capability_layers_for_non_admin(models, user)


@router.post("/models", response_model=ModelResponse)
async def create_model(
    body: ModelCreate,
    request: Request = None,
    model_service: ModelService = Depends(get_model_service),
    user: UserContext = Depends(get_user_context),
):
    """Create a new model."""
    _require_user_gateway_capability(user, Capability.GATEWAY_MODEL_CONFIG_WRITE)

    try:
        model = await model_service.create_model(
            tenant_id=user.tenant_id or "default",
            model_id=body.model_id,
            provider_id=body.provider_id,
            display_name=body.display_name,
            context_window=body.context_window,
            max_output_tokens=body.max_output_tokens,
            supports_vision=body.supports_vision,
            supports_tools=body.supports_tools,
            input_price_per_1k=Decimal(str(body.input_price_per_1k)),
            output_price_per_1k=Decimal(str(body.output_price_per_1k)),
            access_level=body.access_level,
            is_enabled=body.is_enabled,
            sort_order=body.sort_order,
            capability_overrides=body.capability_overrides,
        )
        if request is not None:
            await record_config_change(
                request=request,
                auth=_auth_from_user(user),
                resource_type="model",
                resource_id=body.model_id,
                action="create",
                before=None,
                after=body.model_dump(),
            )
            await _publish_model_config_changed(
                request,
                tenant_id=user.tenant_id or "default",
                model_id=body.model_id,
                provider_id=body.provider_id,
            )
        return model
    except ModelCapabilityError as e:
        raise HTTPException(status_code=422, detail=str(e)) from e
    except Exception as e:
        if "duplicate key" in str(e).lower():
            raise HTTPException(status_code=409, detail="Model already exists")
        raise HTTPException(status_code=500, detail=str(e))


@router.get("/models/{model_id}", response_model=ModelResponse)
async def get_model(
    model_id: str,
    model_service: ModelService = Depends(get_model_service),
    user: UserContext = Depends(get_user_context),
):
    """Get a specific model."""
    model = await model_service.get_model(
        tenant_id=user.tenant_id or "default",
        model_id=model_id,
    )
    if not model:
        raise HTTPException(status_code=404, detail="Model not found")
    return _redact_capability_layers_for_non_admin([model], user)[0]


@router.put("/models/{model_id}", response_model=ModelResponse)
async def update_model(
    model_id: str,
    body: ModelUpdate,
    request: Request = None,
    provider_id: str | None = None,
    model_service: ModelService = Depends(get_model_service),
    user: UserContext = Depends(get_user_context),
):
    """Update a model.

    ``provider_id`` is an optional query param that disambiguates when
    the same model_id exists under multiple providers (introduced by
    migration 055). Without it we default to pre-migration behaviour —
    first row by sort_order+provider_id wins — which works for the
    common case of a single provider per model_id.
    """
    _require_user_gateway_capability(user, Capability.GATEWAY_MODEL_CONFIG_WRITE)

    input_price = (
        Decimal(str(body.input_price_per_1k)) if body.input_price_per_1k is not None else None
    )
    output_price = (
        Decimal(str(body.output_price_per_1k)) if body.output_price_per_1k is not None else None
    )

    new_mid = body.model_id if body.model_id and body.model_id != model_id else None

    try:
        model = await model_service.update_model(
            tenant_id=user.tenant_id or "default",
            model_id=model_id,
            new_model_id=new_mid,
            display_name=body.display_name,
            context_window=body.context_window,
            max_output_tokens=body.max_output_tokens,
            supports_vision=body.supports_vision,
            supports_tools=body.supports_tools,
            input_price_per_1k=input_price,
            output_price_per_1k=output_price,
            access_level=body.access_level,
            is_enabled=body.is_enabled,
            sort_order=body.sort_order,
            provider_id=provider_id,
            capability_overrides=body.capability_overrides,
            expected_capability_revision=body.expected_capability_revision,
        )
    except ModelCapabilityRevisionConflict as exc:
        raise HTTPException(status_code=409, detail="Model capability profile was updated") from exc
    except ModelCapabilityError as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc
    if not model:
        raise HTTPException(status_code=404, detail="Model not found")
    if request is not None:
        await record_config_change(
            request=request,
            auth=_auth_from_user(user),
            resource_type="model",
            resource_id=model_id,
            action="update",
            before={"model_id": model_id, "provider_id": provider_id},
            after=body.model_dump(exclude_none=True),
        )
        await _publish_model_config_changed(
            request,
            tenant_id=user.tenant_id or "default",
            model_id=str(model["model_id"]),
            provider_id=provider_id or str(model.get("provider_id") or "") or None,
        )
    return model


@router.post("/models/{model_id}/capabilities/reset", response_model=ModelResponse)
async def reset_model_capabilities(
    model_id: str,
    request: Request,
    provider_id: str | None = None,
    expected_capability_revision: int | None = Query(None, ge=1),
    model_service: ModelService = Depends(get_model_service),
    user: UserContext = Depends(get_user_context),
):
    """Clear only tenant overrides, preserving provider catalog defaults."""

    _require_user_gateway_capability(user, Capability.GATEWAY_MODEL_CONFIG_WRITE)
    try:
        model = await model_service.reset_model_capabilities(
            tenant_id=user.tenant_id or "default",
            model_id=model_id,
            provider_id=provider_id,
            expected_capability_revision=expected_capability_revision,
        )
    except ModelCapabilityRevisionConflict as exc:
        raise HTTPException(status_code=409, detail="Model capability profile was updated") from exc
    if model is None:
        raise HTTPException(status_code=404, detail="Model not found")
    await record_config_change(
        request=request,
        auth=_auth_from_user(user),
        resource_type="model",
        resource_id=model_id,
        action="capabilities_reset",
        before={"capability_revision": expected_capability_revision},
        after={"capability_revision": model["capability_revision"]},
    )
    await _publish_model_config_changed(
        request,
        tenant_id=user.tenant_id or "default",
        model_id=model_id,
        provider_id=provider_id or str(model.get("provider_id") or "") or None,
    )
    return model


@router.delete("/models/{model_id}")
async def delete_model(
    model_id: str,
    request: Request = None,
    provider_id: str | None = None,
    model_service: ModelService = Depends(get_model_service),
    user: UserContext = Depends(get_user_context),
):
    """Delete a model."""
    _require_user_gateway_capability(user, Capability.GATEWAY_MODEL_CONFIG_WRITE)

    deleted = await model_service.delete_model(
        tenant_id=user.tenant_id or "default",
        model_id=model_id,
        provider_id=provider_id,
    )
    if not deleted:
        raise HTTPException(status_code=404, detail="Model not found")

    if request is not None:
        await record_config_change(
            request=request,
            auth=_auth_from_user(user),
            resource_type="model",
            resource_id=model_id,
            action="delete",
            before={"model_id": model_id, "provider_id": provider_id},
            after=None,
        )
        await _publish_model_config_changed(
            request,
            tenant_id=user.tenant_id or "default",
            model_id=model_id,
            provider_id=provider_id,
        )
    return {"model_id": model_id, "status": "deleted"}


@router.patch("/models/{model_id}/toggle")
async def toggle_model(
    model_id: str,
    request: Request = None,
    is_enabled: bool = Query(..., description="Enable or disable the model"),
    provider_id: str | None = None,
    model_service: ModelService = Depends(get_model_service),
    user: UserContext = Depends(get_user_context),
):
    """Toggle model enabled state."""
    _require_user_gateway_capability(user, Capability.GATEWAY_MODEL_CONFIG_WRITE)

    model = await model_service.toggle_model(
        tenant_id=user.tenant_id or "default",
        model_id=model_id,
        is_enabled=is_enabled,
        provider_id=provider_id,
    )
    if not model:
        raise HTTPException(status_code=404, detail="Model not found")
    if request is not None:
        await record_config_change(
            request=request,
            auth=_auth_from_user(user),
            resource_type="model",
            resource_id=model_id,
            action="toggle",
            before={"model_id": model_id, "provider_id": provider_id},
            after={"is_enabled": is_enabled},
        )
        await _publish_model_config_changed(
            request,
            tenant_id=user.tenant_id or "default",
            model_id=model_id,
            provider_id=provider_id or str(model.get("provider_id") or "") or None,
        )
    return model
