"""Back-compat shim — RedisStorage moved to ai_gateway_core in Phase 6 hot-fix.

The session-cache regression of 2026-04-28 surfaced because the
the legacy session manager was constructed without a
Redis client — its writes invalidated only an in-process dict, never
the gateway-side Redis cache. The cache then served stale empty
history to the frontend until TTL expiry. Moving RedisStorage to
ai_gateway_core lets both services share the cache layer correctly.

Canonical location: ``ai_gateway_core.persistence.redis``.
"""

from __future__ import annotations

from ai_gateway_core.persistence import HAS_REDIS, RedisStorage

__all__ = ["HAS_REDIS", "RedisStorage"]
