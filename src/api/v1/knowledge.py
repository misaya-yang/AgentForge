"""Knowledge Base API — streaming proxy to KB Service with rate limiting."""
from __future__ import annotations

from typing import Any

from ai_gateway_core.logging import get_logger
from fastapi import APIRouter, Depends, HTTPException, Request
from starlette.responses import Response

from ...core.auth.user_resolver import UserContext
from ...core.gateway.multi_dimension_rate_limiter import MultiDimensionRateLimiter, RateLimitContext
from ...services.knowledge_authz import (
    DatasetImpactAuthorityError,
    KnowledgeServiceAgentKnowledgeResolver,
)
from ..deps import get_rate_limiter, get_user_context
from ._proxy_utils import enforce_knowledge_scope, proxy_to_kb_service
from .agents import _get_repository, _is_tenant_admin

router = APIRouter(prefix="/knowledge", tags=["Knowledge Base"])
logger = get_logger(__name__)


@router.get("/datasets/{dataset_id}/impact", summary="Preview Agent references before Dataset deletion")
async def get_dataset_impact(
    dataset_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
    rate_limiter: MultiDimensionRateLimiter | None = Depends(get_rate_limiter),
) -> dict[str, Any]:
    path = f"datasets/{dataset_id}/impact"
    enforce_knowledge_scope(request, path=path)
    if not user.is_authenticated or not user.user_id:
        raise HTTPException(401, "Authentication required")
    if not user.tenant_id or user.tenant_id == "public":
        raise HTTPException(403, "Tenant identity required")
    if rate_limiter is not None:
        ctx = RateLimitContext.from_user_context(user)
        result = await rate_limiter.check(ctx)
        if not result.allowed:
            raise HTTPException(429, "Rate limit exceeded", headers={
                "X-RateLimit-Limit": str(result.limit),
                "X-RateLimit-Remaining": str(result.remaining),
                "Retry-After": str(result.retry_after),
            })

    authority = getattr(request.app.state, "agent_runtime_knowledge_resolver", None)
    temporary_authority = authority is None
    if temporary_authority:
        authority = KnowledgeServiceAgentKnowledgeResolver()
    try:
        await authority.require_dataset_owner(
            dataset_id=dataset_id,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            roles=user.roles,
        )
    except DatasetImpactAuthorityError as exc:
        detail = {
            403: "Dataset owner access required",
            404: "Dataset not found",
        }.get(exc.status_code, "Dataset impact authority unavailable")
        raise HTTPException(exc.status_code, detail) from exc
    except Exception as exc:
        logger.error("dataset impact authority failed: %s", type(exc).__name__)
        raise HTTPException(503, "Dataset impact authority unavailable") from exc
    finally:
        if temporary_authority:
            await authority.close()

    try:
        return await _get_repository(request).get_dataset_impact(
            tenant_id=user.tenant_id,
            dataset_id=dataset_id,
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        logger.error("dataset Agent impact lookup failed: %s", type(exc).__name__)
        raise HTTPException(503, "Dataset impact unavailable") from exc

@router.api_route("/{path:path}", methods=["GET", "POST", "PUT", "PATCH", "DELETE"],
                   summary="Proxy to Knowledge Base Service")
async def proxy_knowledge(
    path: str, request: Request,
    user: UserContext = Depends(get_user_context),
    rate_limiter: MultiDimensionRateLimiter | None = Depends(get_rate_limiter),
) -> Response:
    enforce_knowledge_scope(request, path=path)
    if rate_limiter is not None:
        ctx = RateLimitContext.from_user_context(user)
        result = await rate_limiter.check(ctx)
        if not result.allowed:
            raise HTTPException(429, "Rate limit exceeded", headers={
                "X-RateLimit-Limit": str(result.limit),
                "X-RateLimit-Remaining": str(result.remaining),
                "Retry-After": str(result.retry_after),
            })
    return await proxy_to_kb_service(request, user, path=path)
