"""Owner-scoped assistant memory preference used by both V1 and V2 turns."""

from __future__ import annotations

import json
from typing import Any

from fastapi import HTTPException, Request

MEMORY_CONTROL_KEY = "__assistant_memory_control__"


def memory_database(request: Request) -> Any:
    database = getattr(request.app.state, "database", None)
    if database is None:
        raise HTTPException(503, "Memory storage is unavailable")
    return database


async def memory_enabled(request: Request, tenant_id: str, user_id: str) -> bool:
    try:
        row = await memory_database(request).fetchrow(
            "SELECT value FROM user_memory WHERE tenant_id = $1 AND user_id = $2 AND key = $3",
            tenant_id, user_id, MEMORY_CONTROL_KEY,
        )
    except HTTPException:
        raise
    except Exception:
        raise HTTPException(503, "Memory preference is unavailable") from None
    if row is None:
        return True
    value = row["value"]
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except (TypeError, ValueError):
            return False
    return value is True


async def effective_assistant_memory_mode(
    request: Request, tenant_id: str, user_id: str, requested: str,
) -> str:
    return requested if await memory_enabled(request, tenant_id, user_id) else "off"
