"""User-owned long-term memory controls for the built-in assistant."""

from __future__ import annotations

from typing import Any

from ai_gateway_core.memory import MemoryService
from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel, Field

from ....core.auth.user_resolver import UserContext
from ....services.assistant_entry.memory_controls import (
    MEMORY_CONTROL_KEY,
    memory_database,
    memory_enabled,
)
from ...deps import get_user_context

router = APIRouter()


def _actor(user: UserContext) -> None:
    if not user.is_authenticated or not user.user_id or not user.tenant_id:
        raise HTTPException(401, "Authentication required")


def _value(raw: Any) -> Any:
    if isinstance(raw, str):
        import json

        try:
            return json.loads(raw)
        except (TypeError, ValueError):
            return raw
    return raw


class MemoryPreferenceUpdate(BaseModel):
    enabled: bool


class MemoryItemUpdate(BaseModel):
    key: str = Field(min_length=1, max_length=255)
    value: str = Field(min_length=1, max_length=4000)


@router.get("/memory")
async def get_memory(
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    _actor(user)
    database = memory_database(request)
    try:
        rows = await database.fetch(
            "SELECT key, value, namespace, source, created_at, updated_at, expires_at "
            "FROM user_memory WHERE tenant_id = $1 AND user_id = $2 "
            "AND key <> $3 AND (expires_at IS NULL OR expires_at > NOW()) "
            "ORDER BY updated_at DESC LIMIT 101",
            user.tenant_id, user.user_id, MEMORY_CONTROL_KEY,
        )
    except Exception:
        raise HTTPException(503, "Memory storage is unavailable") from None
    return {
        "enabled": await memory_enabled(request, user.tenant_id, user.user_id),
        "effect": "New runs only; off disables both long-term and session memory for that run",
        "items": [
            {
                "key": row["key"],
                "value": _value(row["value"]),
                "namespace": row["namespace"],
                "source": row["source"],
                "created_at": row["created_at"].isoformat() if row["created_at"] else None,
                "updated_at": row["updated_at"].isoformat() if row["updated_at"] else None,
                "expires_at": row["expires_at"].isoformat() if row["expires_at"] else None,
            }
            for row in rows[:100]
        ],
        "has_more": len(rows) > 100,
    }


@router.patch("/memory")
async def update_memory_preference(
    body: MemoryPreferenceUpdate,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    _actor(user)
    service = MemoryService(memory_database(request))
    saved = await service.set_user_memory(
        user.tenant_id, user.user_id, MEMORY_CONTROL_KEY, body.enabled,
        metadata={"namespace": "assistant-control", "source": "user-preference"},
    )
    if not saved:
        raise HTTPException(503, "Memory preference could not be saved")
    return {"enabled": await memory_enabled(request, user.tenant_id, user.user_id)}


@router.put("/memory/items")
async def put_memory_item(
    body: MemoryItemUpdate,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    _actor(user)
    if body.key.startswith("__assistant_"):
        raise HTTPException(400, "This memory key is reserved")
    service = MemoryService(memory_database(request))
    saved = await service.set_user_memory(
        user.tenant_id, user.user_id, body.key, body.value,
        metadata={"source": "user-edit"},
    )
    if not saved:
        raise HTTPException(503, "Memory item could not be saved")
    return {"key": body.key, "value": await service.get_user_memory(user.tenant_id, user.user_id, body.key)}


@router.delete("/memory/items")
async def delete_memory_item(
    request: Request,
    key: str = Query(min_length=1, max_length=255),
    user: UserContext = Depends(get_user_context),
):
    _actor(user)
    if key.startswith("__assistant_"):
        raise HTTPException(400, "This memory key is reserved")
    service = MemoryService(memory_database(request))
    if not await service.delete_user_memory(user.tenant_id, user.user_id, key):
        raise HTTPException(503, "Memory item could not be deleted")
    return {"key": key, "status": "deleted"}
