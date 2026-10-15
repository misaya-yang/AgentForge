"""Gateway-owned account confirmation before a signed Knowledge delete."""

from __future__ import annotations

import json
import time

from fastapi import HTTPException, Request

from src.core.auth.password import verify_password
from src.core.auth.user_resolver import UserContext

CONFIRMATION_FIELD = "_gateway_delete_confirmation"


async def prepare_knowledge_body(
    request: Request, user: UserContext, *, path: str, body: bytes | None,
) -> bytes | None:
    """Never let a public caller obtain a signature for a forged confirmation."""
    try:
        payload = json.loads(body or b"null")
    except RecursionError:
        raise HTTPException(400, detail={"code": "KB_JSON_NESTING_LIMIT"}) from None
    except (ValueError, UnicodeDecodeError):
        payload = None
    if isinstance(payload, dict) and CONFIRMATION_FIELD in payload:
        raise HTTPException(400, detail={"code": "KB_RESERVED_CONFIRMATION_FIELD"})
    parts = path.strip("/").split("/")
    if request.method != "DELETE" or len(parts) != 2 or parts[0] != "datasets":
        return body
    if not user.is_authenticated or not user.tenant_id or user.tenant_id == "public":
        raise HTTPException(403, detail={"code": "KB_ACCOUNT_CONFIRMATION_REQUIRED"})
    password = payload.get("password") if isinstance(payload, dict) else None
    if not isinstance(password, str) or not password or len(password) > 128:
        raise HTTPException(400, detail={"code": "KB_PASSWORD_CONFIRMATION_INVALID"})
    database = getattr(request.app.state, "database", None)
    if database is None:
        raise HTTPException(503, detail={"code": "KB_CONFIRMATION_UNAVAILABLE"})
    account = await database.get_user_for_tenant(user.user_id, user.tenant_id)
    password_hash = (account or {}).get("password_hash")
    if not password_hash or not verify_password(password, password_hash):
        raise HTTPException(403, detail={"code": "KB_PASSWORD_CONFIRMATION_INVALID"})
    # The existing v2 envelope binds these claims, identity, method, path and
    # body to a one-use request ID. Passwords never cross the service boundary.
    payload.pop("password", None)
    payload[CONFIRMATION_FIELD] = {
        "action": "dataset.delete", "dataset_id": parts[1],
        "user_id": user.user_id, "tenant_id": user.tenant_id,
        "expires_at": int(time.time()) + 60,
    }
    return json.dumps(payload, separators=(",", ":")).encode()
