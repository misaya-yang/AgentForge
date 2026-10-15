"""Shared service-access constraint loading for proxy and services routes."""

from __future__ import annotations

import asyncio
import time
from typing import Any

from ai_gateway_core.logging import get_logger
from fastapi import Request

from .service_access import (
    ServiceAccessPolicy,
    normalize_service_scope,
    service_access_policy_from_metadata,
)
from .user_resolver import UserContext

logger = get_logger(__name__)

_SERVICE_ACCESS_CACHE: dict[
    str, tuple[float, list[tuple[str, list[str]]], ServiceAccessPolicy]
] = {}
_SERVICE_ACCESS_CACHE_LOCK = asyncio.Lock()
_SERVICE_ACCESS_CACHE_MAX_SIZE = 5000


def build_service_access_cache_key(request: Request, user: UserContext) -> str:
    api_key_hash = str(getattr(request.state, "api_key_hash", "") or "")
    api_key_info = getattr(request.state, "api_key_info", None)
    api_allowed_key = ""
    if isinstance(api_key_info, dict):
        api_allowed_key = ",".join(
            normalize_service_scope(api_key_info.get("allowed_services"))
        )
    db_identity = id(getattr(request.app.state, "database", None))
    role_key = ",".join(sorted(str(role) for role in (user.roles or [])))
    return "|".join(
        (
            str(db_identity),
            str(user.user_id or ""),
            str(user.tenant_id or ""),
            api_key_hash,
            api_allowed_key,
            role_key,
        )
    )


def constraint_cache_ttl_seconds(request: Request) -> float:
    db = getattr(request.app.state, "database", None)
    if type(db).__module__ in {"types", "unittest.mock"}:
        return 0.0
    settings = getattr(request.app.state, "settings", None)
    ttl = getattr(getattr(settings, "proxy", None), "constraint_cache_ttl_seconds", 0.0)
    try:
        return max(float(ttl or 0.0), 0.0)
    except Exception:
        return 0.0


async def _get_cached_constraints(
    request: Request, user: UserContext
) -> tuple[list[tuple[str, list[str]]], ServiceAccessPolicy] | None:
    ttl = constraint_cache_ttl_seconds(request)
    if ttl <= 0:
        return None
    now = time.monotonic()
    cache_key = build_service_access_cache_key(request, user)
    async with _SERVICE_ACCESS_CACHE_LOCK:
        entry = _SERVICE_ACCESS_CACHE.get(cache_key)
        if not entry:
            return None
        expires_at, allowed_sources, user_policy = entry
        if now >= expires_at:
            _SERVICE_ACCESS_CACHE.pop(cache_key, None)
            return None
    copied_sources = [(source, list(scope)) for source, scope in allowed_sources]
    return copied_sources, user_policy


async def _set_cached_constraints(
    request: Request,
    user: UserContext,
    allowed_sources: list[tuple[str, list[str]]],
    user_policy: ServiceAccessPolicy,
) -> None:
    ttl = constraint_cache_ttl_seconds(request)
    if ttl <= 0:
        return
    cache_key = build_service_access_cache_key(request, user)
    expires_at = time.monotonic() + ttl
    copied_sources = [(source, list(scope)) for source, scope in allowed_sources]
    async with _SERVICE_ACCESS_CACHE_LOCK:
        while len(_SERVICE_ACCESS_CACHE) >= _SERVICE_ACCESS_CACHE_MAX_SIZE:
            _SERVICE_ACCESS_CACHE.pop(next(iter(_SERVICE_ACCESS_CACHE)))
        _SERVICE_ACCESS_CACHE[cache_key] = (expires_at, copied_sources, user_policy)


async def load_service_access_constraints(
    request: Request,
    user: UserContext,
) -> tuple[list[tuple[str, list[str]]], ServiceAccessPolicy]:
    """Collect API key / tenant / user-level service access constraints."""
    request_cached = getattr(request.state, "_service_access_constraints_cache", None)
    if isinstance(request_cached, tuple) and len(request_cached) == 2:
        return request_cached

    cached = await _get_cached_constraints(request, user)
    if cached is not None:
        request.state._service_access_constraints_cache = cached
        return cached

    allowed_sources: list[tuple[str, list[str]]] = []
    user_policy = ServiceAccessPolicy()

    api_key_info = getattr(request.state, "api_key_info", None)
    if api_key_info:
        api_allowed = normalize_service_scope(api_key_info.get("allowed_services"))
        if api_allowed:
            allowed_sources.append(("api_key", api_allowed))

    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False) or not user.is_authenticated:
        result = (allowed_sources, user_policy)
        request.state._service_access_constraints_cache = result
        await _set_cached_constraints(request, user, allowed_sources, user_policy)
        return result

    if user.tenant_id:
        try:
            tenant = await db.get_tenant(user.tenant_id)
            if tenant:
                tenant_allowed = normalize_service_scope(tenant.get("allowed_services"))
                if tenant_allowed:
                    allowed_sources.append(("tenant", tenant_allowed))
        except Exception as exc:
            logger.warning(
                "[ServiceAccess] Failed to load tenant allowed_services for tenant %s: %s",
                user.tenant_id,
                exc,
            )

    if user.user_id:
        try:
            user_record = await db.get_user(user.user_id)
            if user_record:
                user_policy = service_access_policy_from_metadata(user_record.get("metadata"))
        except Exception as exc:
            logger.warning(
                "[ServiceAccess] Failed to load user service access policy for user %s: %s",
                user.user_id,
                exc,
            )

    result = (allowed_sources, user_policy)
    request.state._service_access_constraints_cache = result
    await _set_cached_constraints(request, user, allowed_sources, user_policy)
    return result


def clear_service_access_constraint_cache() -> None:
    """Test helper to reset the in-process service access cache."""
    _SERVICE_ACCESS_CACHE.clear()