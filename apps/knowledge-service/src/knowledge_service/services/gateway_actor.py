"""Resolve current user authority through Gateway without reading its tables."""
from __future__ import annotations

import json
import os

import httpx
from ai_gateway_core.auth.gateway_secret import GatewaySecret
from fastapi import HTTPException

from ..core.auth.user_resolver import UserContext

ACTOR_PATH = "/internal/v2/agent-capabilities/knowledge-actor"


async def resolve_runtime_actor(*, tenant_id: str, user_id: str) -> UserContext:
    secret = os.getenv("AI_PLATFORM_INTERNAL_TOKEN", "")
    if len(secret) < 16:
        raise HTTPException(503, detail="capability identity unavailable")
    body = json.dumps({"tenant_id": tenant_id, "user_id": user_id}).encode()
    signer = GatewaySecret(secret=secret, caller_service="knowledge-service", audience="gateway",
                           allowed_path_prefixes=(ACTOR_PATH,))
    headers = {"Content-Type": "application/json", "X-Gateway-Secret": signer.sign(
        method="POST", path=ACTOR_PATH, body=body,
    )}
    try:
        async with httpx.AsyncClient(base_url=os.getenv("GATEWAY_URL", "http://gateway:8080"),
                                     timeout=5, trust_env=False, follow_redirects=False) as client:
            response = await client.post(ACTOR_PATH, content=body, headers=headers)
        if response.status_code != 200:
            raise HTTPException(403 if response.status_code == 403 else 503,
                                detail="capability identity unavailable")
        actor = response.json()
    except (httpx.HTTPError, ValueError):
        raise HTTPException(503, detail="capability identity unavailable") from None
    if (not isinstance(actor, dict) or actor.get("user_id") != user_id
            or actor.get("tenant_id") != tenant_id or actor.get("status") != "active"
            or not isinstance(actor.get("roles"), list)
            or not all(isinstance(role, str) and role for role in actor["roles"])
            or not isinstance(actor.get("tier"), str)):
        raise HTTPException(403, detail="capability identity invalid")
    return UserContext(user_id=user_id, tenant_id=tenant_id, user_tier=actor["tier"],
                       user_type="runtime", roles=actor["roles"])
