"""Validate the Gateway's body-bound, one-use dataset deletion confirmation."""

from __future__ import annotations

import time
from typing import Any

from fastapi import HTTPException, Request

from ..core.auth.user_resolver import UserContext


def require_deletion_confirmation(
    request: Request, user: UserContext, dataset_id: str, confirmation: dict[str, Any] | None,
) -> None:
    # Anonymous development mode and legacy v1 signatures are insufficient:
    # only v2 binds the confirmation body to the authenticated actor and route.
    if (
        getattr(request.state, "gateway_secret_verified", False) is not True
        or not request.headers.get("X-Gateway-Secret", "").startswith("v2:")
        or not user.is_authenticated
        or not user.tenant_id
        or user.tenant_id == "public"
    ):
        raise HTTPException(403, detail={"code": "KB_DELETE_CONFIRMATION_REQUIRED"})
    claims = confirmation or {}
    expires = claims.get("expires_at")
    now = int(time.time())
    if (
        claims.get("action") != "dataset.delete"
        or claims.get("dataset_id") != dataset_id
        or claims.get("user_id") != user.user_id
        or claims.get("tenant_id") != user.tenant_id
        or type(expires) is not int
        # The envelope already limits signer clock skew to 60 seconds.
        or not now - 60 <= expires <= now + 120
    ):
        raise HTTPException(403, detail={"code": "KB_DELETE_CONFIRMATION_INVALID"})
