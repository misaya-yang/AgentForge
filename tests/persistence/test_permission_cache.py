"""
Tests for permission cache memory safety.

Ensures permission cache doesn't grow unbounded.
"""

import pytest


class TestPermissionCache:
    """Test permission cache memory safety."""

    @pytest.mark.asyncio
    async def test_cache_has_size_limit(self):
        """Cache should not grow unbounded."""
        from src.persistence.database import DatabaseStorage

        db = DatabaseStorage(dsn="", enabled=False, permission_cache_ttl_seconds=300)

        # Verify max_size attribute exists
        assert hasattr(db, "_permission_cache_max_size")
        max_size = db._permission_cache_max_size
        assert max_size > 0

        # Try to add more entries than limit
        for i in range(max_size + 100):
            await db._set_cached_permissions(f"user_{i}", ["read"])

        # Cache should be bounded
        assert len(db._permission_cache) <= max_size

    @pytest.mark.asyncio
    async def test_cache_evicts_oldest(self):
        """When cache is full, oldest entries should be evicted."""
        from src.persistence.database import DatabaseStorage

        db = DatabaseStorage(dsn="", enabled=False, permission_cache_ttl_seconds=300)
        db._permission_cache_max_size = 3  # Small size for testing

        await db._set_cached_permissions("user_1", ["read"])
        await db._set_cached_permissions("user_2", ["write"])
        await db._set_cached_permissions("user_3", ["admin"])
        await db._set_cached_permissions("user_4", ["new"])

        # user_1 should be evicted (oldest)
        assert "user_1" not in db._permission_cache
        assert "user_4" in db._permission_cache
        assert len(db._permission_cache) <= 3

    @pytest.mark.asyncio
    async def test_recent_entries_preserved(self):
        """Most recent entries should be kept during eviction."""
        from src.persistence.database import DatabaseStorage

        db = DatabaseStorage(dsn="", enabled=False, permission_cache_ttl_seconds=300)
        db._permission_cache_max_size = 5

        # Add 10 entries
        for i in range(10):
            await db._set_cached_permissions(f"user_{i}", ["perm"])

        # Only most recent 5 should remain
        assert len(db._permission_cache) <= 5

        # The last entries should be present
        assert "user_9" in db._permission_cache or "user_8" in db._permission_cache

    @pytest.mark.asyncio
    async def test_cache_disabled_no_limit_needed(self):
        """When cache is disabled (TTL=0), no size checking needed."""
        from src.persistence.database import DatabaseStorage

        db = DatabaseStorage(dsn="", enabled=False, permission_cache_ttl_seconds=0)

        # Should not add anything when disabled
        await db._set_cached_permissions("user_1", ["read"])
        assert len(db._permission_cache) == 0

    @pytest.mark.asyncio
    async def test_max_size_is_reasonable(self):
        """Max size should be reasonable for production."""
        from src.persistence.database import DatabaseStorage

        db = DatabaseStorage(dsn="", enabled=False)

        # Should be large enough for production but bounded
        assert db._permission_cache_max_size >= 1000  # At least 1000
        assert db._permission_cache_max_size <= 100000  # But not crazy
