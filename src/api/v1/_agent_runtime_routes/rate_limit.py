"""Channel rate limiting for published Agent Runtime traffic.

ARC-01B split of ``src/api/v1/agent_runtime.py``.  Moved verbatim; the facade
keeps time-limited re-exports for pre-split import paths.
"""

from __future__ import annotations

import hashlib
from typing import Any

from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentRuntimeUnavailableError,
)
from fastapi import HTTPException, Request

from ....core.client_ip import get_client_ip_from_request
from .core import (
    _map_repository_error,
    _raise_runtime_error,
    _repository,
    _request_id,
)


class RedisAgentChannelLimiter:
    """One atomic Redis decision across principal, IP and Publication buckets."""

    _SCRIPT = """
    for i = 1, #KEYS do
      local current = tonumber(redis.call('GET', KEYS[i]) or '0')
      local limit = tonumber(ARGV[(i - 1) * 2 + 1])
      if current >= limit then
        return i
      end
    end
    for i = 1, #KEYS do
      local value = redis.call('INCR', KEYS[i])
      if value == 1 then
        redis.call('EXPIRE', KEYS[i], tonumber(ARGV[(i - 1) * 2 + 2]))
      end
    end
    return 0
    """

    def __init__(self, redis_client: Any):
        self._redis = redis_client

    @staticmethod
    def _digest(value: str) -> str:
        return hashlib.sha256(value.encode("utf-8")).hexdigest()[:32]

    async def consume(
        self,
        *,
        publication_id: str,
        principal_id: str,
        client_ip: str,
        limits: tuple[int, int, int, int, int, int],
    ) -> int:
        publication_key = self._digest(publication_id)
        principal_key = self._digest(principal_id)
        ip_key = self._digest(client_ip or "unknown")
        tag = "{" + publication_key + "}"
        keys = [
            f"agent:channel:{tag}:principal:{principal_key}:minute",
            f"agent:channel:{tag}:principal:{principal_key}:day",
            f"agent:channel:{tag}:ip:{ip_key}:minute",
            f"agent:channel:{tag}:ip:{ip_key}:day",
            f"agent:channel:{tag}:publication:minute",
            f"agent:channel:{tag}:publication:day",
        ]
        ttl = (60, 86_400, 60, 86_400, 60, 86_400)
        args: list[int] = []
        for limit, window in zip(limits, ttl, strict=True):
            args.extend((limit, window))
        return int(await self._redis.eval(self._SCRIPT, len(keys), *keys, *args))


def _bounded_policy_int(policy: dict[str, Any], key: str, default: int, maximum: int) -> int:
    value = policy.get(key, default)
    if isinstance(value, bool) or not isinstance(value, int):
        return default
    return max(1, min(value, maximum))


async def _enforce_channel_limits(
    request: Request,
    *,
    publication: dict[str, Any],
    principal_id: str,
) -> None:
    """Atomically bound cost by principal, client IP and Publication."""

    agent_id = str(publication.get("agent_id") or "")
    publication_id = str(publication.get("publication_id") or "")
    if not agent_id or not publication_id:
        _raise_runtime_error(
            request,
            503,
            "AGENT_RUNTIME_QUOTA_UNAVAILABLE",
            "Agent runtime quota identity is unavailable",
        )
    try:
        governance_result = await _repository(request).get_runtime_governance_usage(
            tenant_id=str(publication.get("tenant_id") or ""),
            agent_id=agent_id,
            publication_id=publication_id,
        )
    except HTTPException:
        raise
    except AgentRuntimeUnavailableError as exc:
        _map_repository_error(request, exc)
    except Exception as exc:
        raise HTTPException(
            status_code=503,
            detail={
                "code": "AGENT_RUNTIME_QUOTA_UNAVAILABLE",
                "message": "Agent runtime quota policy is unavailable; retry later",
                "request_id": _request_id(request),
            },
        ) from exc
    governance = governance_result.get("policy")
    if not isinstance(governance, dict):
        _raise_runtime_error(
            request,
            503,
            "AGENT_RUNTIME_QUOTA_UNAVAILABLE",
            "Agent runtime quota policy is unavailable; retry later",
        )
    blocking_exceeded = [
        str(code)
        for code in (governance_result.get("exceeded") or [])
        if code != "AGENT_RUNTIME_STORAGE_QUOTA_EXCEEDED"
    ]
    if blocking_exceeded:
        _raise_runtime_error(
            request,
            429,
            blocking_exceeded[0],
            "Agent runtime quota exceeded; wait for active runs to finish or raise the governance limit",
        )
    policy = publication.get("policy") if isinstance(publication.get("policy"), dict) else {}
    governance_limits = (
        int(governance["principal_requests_per_minute"]),
        int(governance["principal_requests_per_day"]),
        int(governance["ip_requests_per_minute"]),
        int(governance["ip_requests_per_day"]),
        int(governance["publication_requests_per_minute"]),
        int(governance["publication_requests_per_day"]),
    )
    limits = (
        min(_bounded_policy_int(policy, "requests_per_minute", governance_limits[0], 10_000), governance_limits[0]),
        min(_bounded_policy_int(policy, "requests_per_day", governance_limits[1], 10_000_000), governance_limits[1]),
        min(_bounded_policy_int(policy, "ip_requests_per_minute", governance_limits[2], 10_000), governance_limits[2]),
        min(_bounded_policy_int(policy, "ip_requests_per_day", governance_limits[3], 10_000_000), governance_limits[3]),
        min(_bounded_policy_int(policy, "publication_requests_per_minute", governance_limits[4], 100_000), governance_limits[4]),
        min(_bounded_policy_int(policy, "publication_requests_per_day", governance_limits[5], 100_000_000), governance_limits[5]),
    )
    limiter = getattr(request.app.state, "agent_channel_limiter", None)
    if limiter is None:
        redis_storage = getattr(request.app.state, "redis", None)
        native_getter = getattr(redis_storage, "get_native_client", None)
        redis_client = native_getter() if callable(native_getter) else None
        if redis_client is None:
            _raise_runtime_error(
                request,
                503,
                "AGENT_RUNTIME_QUOTA_UNAVAILABLE",
                "Agent runtime quota enforcement is unavailable",
            )
        limiter = RedisAgentChannelLimiter(redis_client)
        request.app.state.agent_channel_limiter = limiter
    try:
        rejected_bucket = await limiter.consume(
            publication_id=publication_id,
            principal_id=principal_id,
            client_ip=get_client_ip_from_request(request),
            limits=limits,
        )
    except HTTPException:
        raise
    except Exception as exc:
        raise HTTPException(
            status_code=503,
            detail={
                "code": "AGENT_RUNTIME_QUOTA_UNAVAILABLE",
                "message": "Agent runtime quota enforcement is unavailable",
                "request_id": _request_id(request),
            },
        ) from exc
    if rejected_bucket:
        daily_bucket = rejected_bucket in {2, 4, 6}
        _raise_runtime_error(
            request,
            429,
            "AGENT_RUNTIME_QUOTA_EXCEEDED" if daily_bucket else "AGENT_RUNTIME_RATE_LIMITED",
            "Daily quota exceeded; retry after the quota window resets"
            if daily_bucket
            else "Rate limit exceeded; retry after the current window resets",
        )
