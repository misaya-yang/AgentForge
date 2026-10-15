import pytest

from src.persistence.database import DatabaseStorage


@pytest.mark.asyncio
async def test_get_usage_last_ingested_at_returns_none_when_no_pool():
    db = DatabaseStorage()
    assert await db.get_usage_last_ingested_at("tenant_1") is None
