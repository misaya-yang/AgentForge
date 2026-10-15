from __future__ import annotations

from fastapi import APIRouter, Depends, Request

from ...core.auth.user_resolver import UserContext
from ...core.gateway.dispatcher import GatewayDispatcher
from ...models.response import UnifiedResponse
from ..deps import get_dispatcher, get_user_context
from ..schemas.request import UnifiedRequestSchema
from ..schemas.response import UnifiedResponseSchema

router = APIRouter()


@router.post("/submit", response_model=UnifiedResponseSchema)
async def submit(
    body: UnifiedRequestSchema,
    request: Request,
    dispatcher: GatewayDispatcher = Depends(get_dispatcher),
    user: UserContext = Depends(get_user_context),
):
    domain_req = body.to_domain(default_user_id=user.user_id, default_tenant_id=user.tenant_id)
    client_ip = request.client.host if request.client else None
    task_id = await dispatcher.submit(domain_req, roles=user.roles, client_ip=client_ip)
    pending = UnifiedResponse(
        request_id=domain_req.request_id,
        status="pending",
        outputs=[],
        session_id=domain_req.session_id,
        task_id=task_id,
    )
    return UnifiedResponseSchema.from_domain(pending)
