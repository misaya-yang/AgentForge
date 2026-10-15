from __future__ import annotations

from fastapi import APIRouter, Depends, Request

from ...core.auth.user_resolver import UserContext
from ...core.gateway.dispatcher import GatewayDispatcher
from ..deps import get_dispatcher, get_user_context
from ..schemas.request import UnifiedRequestSchema
from ..schemas.response import UnifiedResponseSchema

router = APIRouter()


@router.post("/invoke", response_model=UnifiedResponseSchema)
async def invoke(
    body: UnifiedRequestSchema,
    request: Request,
    dispatcher: GatewayDispatcher = Depends(get_dispatcher),
    user: UserContext = Depends(get_user_context),
):
    domain_req = body.to_domain(default_user_id=user.user_id, default_tenant_id=user.tenant_id)
    client_ip = request.client.host if request.client else None
    resp = await dispatcher.invoke(domain_req, roles=user.roles, client_ip=client_ip)
    return UnifiedResponseSchema.from_domain(resp)
