"""Assistant catalog routes: models, datasets, config, tools, policies.

ARC-01 split of ``src/api/v1/assistant.py``.  All of these surfaces are
Gateway-owned reads (model catalogue, KB proxy, capability catalog projection);
no Runtime loop logic lives here.
"""

from __future__ import annotations

import inspect
import logging
from typing import Any

from ai_gateway_core.knowledge import is_multimodal_embedding_model
from ai_gateway_core.logging import record_internal_exception
from fastapi import APIRouter, Depends, HTTPException, Request

from ....core.assistant_capability_catalog import (
    AssistantCapabilityCatalogError,
    load_assistant_capability_catalog,
    load_gateway_assistant_policies,
    project_assistant_tools,
)
from ....core.auth.user_resolver import UserContext
from ....services.assistant_entry.model_access import (
    load_visible_assistant_models,
)
from ...deps import get_user_context
from ...schemas.assistant import (
    AssistantConfigResponse,
    DatasetsListResponse,
    ModelsListResponse,
)
from .schemas import (
    AssistantPoliciesResponse,
    ToolsListResponse,
)

router = APIRouter()
logger = logging.getLogger(__name__)


async def _load_assistant_tools(request: Request) -> list[str]:
    """Read an already-installed safe catalog getter without importing the runtime."""

    getter = getattr(request.app.state, "assistant_capability_catalog_getter", None)
    if getter is None:
        getter = getattr(request.app.state, "agent_runtime_capability_catalog", None)
    if getter is None:
        return []
    try:
        value = getter() if callable(getter) else getter
        if inspect.isawaitable(value):
            value = await value
        if isinstance(value, dict):
            value = value.get("tools", value.get("capabilities", []))
        if not isinstance(value, (list, tuple)):
            return []
        names: list[str] = []
        for item in value:
            name = item.get("name") if isinstance(item, dict) else item
            if isinstance(name, str) and name.strip():
                names.append(name.strip())
        return list(dict.fromkeys(names))
    except Exception as exc:
        record_internal_exception(logger, "assistant.gateway.config.catalog_failure", exc)
        return []


@router.get("/models", response_model=ModelsListResponse)
async def list_models(
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> ModelsListResponse:
    """Gateway-owned model catalogue used by the Agent Runtime."""

    # The implementation additionally filters by tenant and access level; keep
    # that implementation note out of the published operation description.
    return ModelsListResponse(models=await load_visible_assistant_models(request, user))


@router.get("/datasets", response_model=DatasetsListResponse)
async def list_datasets(
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> DatasetsListResponse:
    """Gateway knowledge catalogue resolved for Agent Runtime turns."""

    # The read is implemented through the Gateway-owned Knowledge proxy.
    kb_proxy = getattr(request.app.state, "kb_proxy", None)
    if kb_proxy is None or not callable(getattr(kb_proxy, "list_datasets", None)):
        raise HTTPException(status_code=503, detail="Knowledge service is unavailable")
    try:
        raw_datasets = await kb_proxy.list_datasets(user)
    except Exception as exc:
        record_internal_exception(logger, "assistant.gateway.datasets.internal_failure", exc)
        raise HTTPException(status_code=503, detail="Knowledge service is unavailable") from None
    if not isinstance(raw_datasets, list):
        raw_datasets = []
    datasets: list[dict[str, Any]] = []
    for raw in raw_datasets:
        if not isinstance(raw, dict):
            continue
        stats = raw.get("statistics") if isinstance(raw.get("statistics"), dict) else {}
        embedding_model = raw.get("embedding_model")
        datasets.append(
            {
                "dataset_id": str(raw.get("dataset_id") or ""),
                "name": str(raw.get("name") or ""),
                "description": raw.get("description"),
                "document_count": int(
                    stats.get("document_count", raw.get("document_count", 0)) or 0
                ),
                "chunk_count": int(
                    stats.get("segment_count", raw.get("segment_count", raw.get("chunk_count", 0)))
                    or 0
                ),
                "embedding_model": embedding_model,
                "is_multimodal": is_multimodal_embedding_model(str(embedding_model or "")),
            }
        )
    return DatasetsListResponse(datasets=datasets)


@router.get("/config", response_model=AssistantConfigResponse)
async def get_config(
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AssistantConfigResponse:
    """Gateway-owned provider configuration."""

    # This is a projection of Gateway-owned control-plane state, not a second
    # configuration authority.
    visible_models = await load_visible_assistant_models(request, user)
    settings = getattr(request.app.state, "settings", None)
    requested_default = str(getattr(settings, "default_model", "") or "").strip()
    default_model_id = next(
        (model["id"] for model in visible_models if model["id"] == requested_default),
        visible_models[0]["id"] if visible_models else "",
    )

    model_meta = getattr(request.app.state, "model_meta", None)
    available_providers: list[str] = []
    if model_meta is not None and callable(getattr(model_meta, "is_provider_configured", None)):
        for provider_id in dict.fromkeys(
            model["provider"] for model in visible_models if model["provider"]
        ):
            try:
                if await model_meta.is_provider_configured(
                    user.tenant_id or "default", provider_id
                ):
                    available_providers.append(provider_id)
            except Exception as exc:
                # Configuration must fail closed when provider state is unavailable.
                record_internal_exception(logger, "assistant.gateway.config.provider_failure", exc)

    return AssistantConfigResponse(
        default_model_id=default_model_id,
        available_providers=available_providers,
        kb_enabled=getattr(request.app.state, "kb_proxy", None) is not None,
        web_search_enabled=True,
        tools_available=await _load_assistant_tools(request),
    )


@router.get("/tools", response_model=ToolsListResponse)
async def list_tools(
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> ToolsListResponse:
    """Gateway-owned tool registry exposed to the Agent Runtime."""

    # Authorization and tenant policy still filter the declarative catalogue.
    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="authentication required")
    try:
        _, records = load_assistant_capability_catalog()
        policies = await load_gateway_assistant_policies(request, user, records)
        tools = project_assistant_tools(user, tenant_policy=policies)
    except HTTPException:
        raise
    except AssistantCapabilityCatalogError as exc:
        record_internal_exception(logger, "assistant.gateway.tools.catalog_failure", exc)
        raise HTTPException(
            status_code=503, detail="Assistant tool catalog is unavailable"
        ) from None
    return ToolsListResponse(tools=tools)


@router.get("/policies", response_model=AssistantPoliciesResponse)
async def get_policies(
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AssistantPoliciesResponse:
    """Gateway policy snapshot used by the Agent Runtime."""

    # The snapshot is resolved for the authenticated tenant below.
    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="authentication required")
    try:
        _, records = load_assistant_capability_catalog()
        policies = await load_gateway_assistant_policies(request, user, records)
    except HTTPException:
        raise
    except AssistantCapabilityCatalogError as exc:
        record_internal_exception(logger, "assistant.gateway.policies.catalog_failure", exc)
        raise HTTPException(status_code=503, detail="Assistant policy is unavailable") from None
    return AssistantPoliciesResponse(policies=policies)
