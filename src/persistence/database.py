"""Back-compat shim — DatabaseStorage moved to ai_gateway_core in Phase 5f Batch C.

The 7K-LOC asyncpg-backed concrete now lives at
``packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py``
so platform services can reach it without the
AS container needing ``COPY src/`` in its Dockerfile.

This file is intentionally a thin re-export so existing import sites in
gateway src/ keep working. New code should import from
``ai_gateway_core.persistence`` directly.
"""

from __future__ import annotations

from ai_gateway_core.persistence import HAS_ASYNCPG, DatabaseStorage
from ai_gateway_core.persistence.database import build_service_query

__all__ = ["DatabaseStorage", "HAS_ASYNCPG", "build_service_query"]
