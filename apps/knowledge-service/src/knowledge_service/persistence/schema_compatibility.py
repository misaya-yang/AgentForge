"""Read-only startup precondition used by both the Knowledge API and worker."""
from __future__ import annotations

from typing import Any

from ai_gateway_contracts.database_revision import (
    DATABASE_BASELINE_ID,
    KNOWLEDGE_MAX_SUPPORTED_EPOCH_SEQUENCE,
    KNOWLEDGE_MIN_REQUIRED_EPOCH_SEQUENCE,
)


async def require_compatible_schema(conn: Any) -> None:
    if not await conn.fetchval("SELECT to_regclass('public.platform_schema_baselines') IS NOT NULL AND to_regclass('public.platform_schema_changes') IS NOT NULL"):
        raise RuntimeError("Knowledge requires the schema authority migration before startup")
    baselines = await conn.fetch("SELECT baseline_id FROM public.platform_schema_baselines")
    if len(baselines) != 1 or baselines[0]["baseline_id"] != DATABASE_BASELINE_ID:
        raise RuntimeError("Knowledge schema baseline is incompatible")
    sequence = await conn.fetchval("SELECT COALESCE(max(sequence),0) FROM public.platform_schema_changes WHERE baseline_id=$1", DATABASE_BASELINE_ID)
    # Dataset reads include is_archived from authority sequence 5 onward.
    # Reject older schemas at startup instead of failing individual requests.
    if not KNOWLEDGE_MIN_REQUIRED_EPOCH_SEQUENCE <= sequence <= KNOWLEDGE_MAX_SUPPORTED_EPOCH_SEQUENCE:
        raise RuntimeError("Knowledge schema epoch is incompatible; run the matching authority migration")
