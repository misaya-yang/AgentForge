"""Gateway model-access policy shared by Assistant, Responses and Agent routes.

Moved verbatim from ``src/api/v1/assistant.py`` by ARC-01 so that every public
entry point authorizes models through one implementation.  Behaviour, status
codes and error details are contract-frozen; change them only with the public
API contract.
"""

from __future__ import annotations

import json
import logging
from typing import Any

from ai_gateway_core.logging import record_internal_exception
from fastapi import HTTPException, Request

from ...core.auth.user_resolver import UserContext

logger = logging.getLogger(__name__)


def user_can_access_model(user: UserContext, access_level: str) -> bool:
    """
    Check if a user can access a model based on access level.

    Access levels:
    - public: All authenticated users
    - premium: Users with tier=premium/enterprise/admin or role=admin
    - admin: Only users with tier=admin or role=admin
    """
    from ai_gateway_core.enums import ModelAccessLevel

    try:
        required_access_level = ModelAccessLevel(access_level)
    except (TypeError, ValueError):
        # Dirty metadata must not turn a restricted model into a public one.
        return False

    # Admin users can access every *known* access level.
    if user.tier == "admin" or "admin" in user.roles:
        return True

    if required_access_level is ModelAccessLevel.PUBLIC:
        return True
    elif required_access_level is ModelAccessLevel.PREMIUM:
        return user.tier in ("premium", "enterprise", "admin")
    elif required_access_level is ModelAccessLevel.ADMIN:
        return False  # Only admins, checked above

    return False


async def check_model_permission(
    user: UserContext, model_id: str, model_meta: Any, *, provider_id: str | None = None,
) -> None:
    """Check if the user has permission to invoke ``model_id``.

    DB-backed via ``GatewayModelMeta``. Unknown model → 400; caller's
    tier/role insufficient → 403. The old ModelRegistry in-memory
    lookup was sync; swapping to a single DB query per chat request
    is cheap (well under 1 ms).
    """
    access_level = (
        await model_meta.get_access_level(user.tenant_id, model_id, provider_id=provider_id)
        if provider_id else await model_meta.get_access_level(user.tenant_id, model_id)
    )
    if access_level is None:
        raise HTTPException(status_code=400, detail=f"Unknown model: {model_id}")

    if not user_can_access_model(user, access_level):
        raise HTTPException(
            status_code=403,
            detail=f"Access denied: Model '{model_id}' requires {access_level} access level",
        )


def effective_chat_model_id(request: Request, requested_model_id: str | None) -> str:
    """Resolve the exact model that Gateway authorizes and proxies downstream."""

    requested = str(requested_model_id or "").strip()
    if requested:
        return requested

    settings = getattr(request.app.state, "settings", None)
    default_model = str(getattr(settings, "default_model", "") or "").strip()
    if not default_model:
        raise HTTPException(status_code=503, detail="Default model is not configured")
    return default_model


def chat_body_with_model(raw_body: Any, model_id: str) -> bytes:
    """Return the validated client body with the server-resolved model pinned."""

    payload = dict(raw_body) if isinstance(raw_body, dict) else {}
    payload["model_id"] = model_id
    return json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def assistant_model_service(request: Request) -> Any:
    """Return the gateway-owned model service for Assistant read routes."""

    model_service = getattr(request.app.state, "model_service", None)
    if model_service is not None:
        return model_service
    # Keep lightweight app fixtures and early-startup callers compatible with
    # the model facade while the canonical application state remains
    # ``model_service``.
    model_meta = getattr(request.app.state, "model_meta", None)
    return getattr(model_meta, "model_service", None)


def visible_assistant_models(user: UserContext, rows: Any) -> list[dict[str, Any]]:
    """Project enabled, tenant-scoped model rows into the public Assistant shape."""

    if not isinstance(rows, list):
        rows = list(rows or [])
    visible: list[dict[str, Any]] = []
    for row in rows:
        if not isinstance(row, dict) or row.get("is_enabled") is not True:
            continue
        row_tenant_id = row.get("tenant_id")
        if row_tenant_id is not None and str(row_tenant_id) != (user.tenant_id or "default"):
            continue
        access_level = row.get("access_level")
        # ``user_can_access_model`` deliberately rejects malformed access
        # levels, including for administrators.
        if not isinstance(access_level, str) or not user_can_access_model(user, access_level):
            continue
        model_id = str(row.get("model_id") or "").strip()
        provider_id = str(row.get("provider_id") or "").strip()
        effective_capabilities = row.get("effective_capabilities")
        if effective_capabilities is None:
            effective_capabilities = {}
        if not model_id or not provider_id or not isinstance(effective_capabilities, dict):
            continue
        try:
            context_window = int(row.get("context_window") or 0)
            max_output_tokens = int(row.get("max_output_tokens") or 0)
            capability_revision = int(row.get("capability_revision") or 0)
            sort_order = int(row.get("sort_order") or 0)
            input_price = float(row.get("input_price_per_1k") or 0)
            output_price = float(row.get("output_price_per_1k") or 0)
        except (TypeError, ValueError, OverflowError):
            continue
        if context_window <= 0 or max_output_tokens <= 0 or capability_revision < 1:
            continue
        visible.append(
            {
                "id": model_id,
                "name": str(row.get("display_name") or model_id),
                "provider": provider_id,
                "context_window": context_window,
                "max_output_tokens": max_output_tokens,
                "supports_vision": bool(row.get("supports_vision", False)),
                "supports_tools": bool(row.get("supports_tools", False)),
                "access_level": access_level,
                "input_price_per_1k": input_price,
                "output_price_per_1k": output_price,
                "effective_capabilities": dict(effective_capabilities),
                "capability_revision": capability_revision,
                "_sort_order": sort_order,
            }
        )
    visible.sort(key=lambda item: (item["provider"], item["_sort_order"], item["name"], item["id"]))
    for item in visible:
        item.pop("_sort_order", None)
    return visible


async def load_visible_assistant_models(
    request: Request, user: UserContext
) -> list[dict[str, Any]]:
    model_service = assistant_model_service(request)
    if model_service is None or not callable(getattr(model_service, "list_models", None)):
        raise HTTPException(status_code=503, detail="Model service is unavailable")
    try:
        rows = await model_service.list_models(
            tenant_id=user.tenant_id or "default",
            include_disabled=False,
        )
    except Exception as exc:
        record_internal_exception(logger, "assistant.gateway.models.internal_failure", exc)
        raise HTTPException(status_code=503, detail="Model service is unavailable") from None
    return visible_assistant_models(user, rows)
