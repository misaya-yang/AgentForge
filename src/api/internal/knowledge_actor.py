"""Gateway-owned current actor resolution for proof-verified KB capabilities."""
from __future__ import annotations

import os
from functools import lru_cache

from ai_gateway_core.auth.gateway_secret import GatewaySecret, InvalidGatewaySecret
from fastapi import HTTPException, Request
from pydantic import BaseModel, ConfigDict, Field


class KnowledgeActorRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    tenant_id: str = Field(min_length=1, max_length=255)
    user_id: str = Field(min_length=1, max_length=255)


@lru_cache(maxsize=1)
def _verifier() -> GatewaySecret:
    secret = os.getenv("AI_PLATFORM_INTERNAL_TOKEN", "")
    if len(secret) < 16:
        raise HTTPException(503, detail={"code": "KB_ACTOR_UNAVAILABLE"})
    return GatewaySecret(secret=secret, caller_service="knowledge-service", audience="gateway",
                         allowed_path_prefixes=("/internal/v2/agent-capabilities/knowledge-actor",))


async def resolve_knowledge_actor(payload: KnowledgeActorRequest, request: Request):
    body = await request.body()
    if len(body) > 4096:
        raise HTTPException(413, detail={"code": "KB_ACTOR_REQUEST_TOO_LARGE"})
    try:
        _verifier().verify(request.headers.get("X-Gateway-Secret", ""), method=request.method,
                           path=request.url.path, query=request.url.query, body=body,
                           identity_headers=request.headers)
    except InvalidGatewaySecret:
        raise HTTPException(403, detail={"code": "KB_ACTOR_AUTH_DENIED"}) from None
    db = getattr(request.app.state, "database", None)
    if db is None:
        raise HTTPException(503, detail={"code": "KB_ACTOR_UNAVAILABLE"})
    account = await db.get_user_for_tenant(payload.user_id, payload.tenant_id)
    if not account or account.get("status") != "active":
        raise HTTPException(403, detail={"code": "KB_ACTOR_DENIED"})
    # user_roles is current (including expiry/revocation). An empty result is
    # meaningful; never resurrect stale roles from users.roles or a JWT.
    roles = await db.get_user_roles(payload.user_id)
    return {"user_id": payload.user_id, "tenant_id": payload.tenant_id,
            "roles": roles, "tier": account.get("tier") or "normal", "status": "active"}
