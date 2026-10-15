"""Knowledge startup must accept the schema shipped with the application."""

from pathlib import Path
from unittest.mock import AsyncMock

import pytest
from ai_gateway_contracts.database_revision import DATABASE_BASELINE_ID
from knowledge_service.persistence.schema_compatibility import require_compatible_schema

from database.authority.manifest import load_epoch_manifest


def shipped_epoch() -> int:
    root = Path(__file__).resolve().parents[3]
    manifest = load_epoch_manifest(
        root / "database" / "migrations" / DATABASE_BASELINE_ID / "manifest.yml"
    )
    return max(change.sequence for change in manifest.changes)


def schema_connection(sequence: int) -> AsyncMock:
    conn = AsyncMock()
    conn.fetch.return_value = [{"baseline_id": DATABASE_BASELINE_ID}]
    conn.fetchval.side_effect = [True, sequence]
    return conn


@pytest.mark.asyncio
async def test_knowledge_starts_on_shipped_schema() -> None:
    await require_compatible_schema(schema_connection(shipped_epoch()))


@pytest.mark.asyncio
@pytest.mark.parametrize("sequence", [4, shipped_epoch() + 1])
async def test_knowledge_rejects_unsupported_schema(sequence: int) -> None:
    with pytest.raises(RuntimeError, match="epoch is incompatible"):
        await require_compatible_schema(schema_connection(sequence))
