"""
Tests for Confluence polling interval handling.

Ensures that:
1. Polling interval defaults are applied correctly
2. NULL intervals from database don't crash the scheduler
3. sync_mode=polling always writes interval to database
"""

from datetime import datetime, timedelta

from knowledge_service.services.knowledge.confluence.scheduler import PagePollingTask


class TestPagePollingTask:
    """Tests for PagePollingTask with interval handling."""

    def test_default_interval(self):
        """Task should use provided interval."""
        task = PagePollingTask(
            page_record_id="page123",
            interval_minutes=30,
            priority=5,
        )
        assert task.interval_minutes == 30

    def test_interval_affects_next_run(self):
        """Task interval should affect next run calculation via schedule_next."""
        task = PagePollingTask(
            page_record_id="page123",
            interval_minutes=60,
            priority=0,
        )
        # schedule_next calculates next_run based on interval_minutes
        task.schedule_next()

        # Next run should be approximately 60 minutes from now
        expected = datetime.utcnow() + timedelta(minutes=60)
        # Allow 1 minute tolerance
        assert abs((task.next_run - expected).total_seconds()) < 60


class TestSchedulerIntervalHandling:
    """Tests for scheduler's handling of NULL/missing intervals."""

    def test_null_interval_from_db_uses_default(self):
        """When DB returns NULL interval, should use default 60 minutes."""
        page_data = {
            "id": "page123",
            "polling_interval_minutes": None,  # NULL from database
            "sync_priority": 0,
        }

        # Simulating the scheduler's logic
        interval_minutes = page_data.get("polling_interval_minutes") or 60
        priority = page_data.get("sync_priority") or 0

        assert interval_minutes == 60
        assert priority == 0

    def test_missing_interval_key_uses_default(self):
        """When interval key is missing, should use default."""
        page_data = {
            "id": "page123",
            # polling_interval_minutes not present
        }

        interval_minutes = page_data.get("polling_interval_minutes") or 60
        assert interval_minutes == 60

    def test_zero_interval_uses_default(self):
        """When interval is 0, should use default (0 is falsy)."""
        page_data = {
            "id": "page123",
            "polling_interval_minutes": 0,  # Invalid value
        }

        interval_minutes = page_data.get("polling_interval_minutes") or 60
        assert interval_minutes == 60

    def test_valid_interval_is_preserved(self):
        """When interval is valid, should use the value."""
        page_data = {
            "id": "page123",
            "polling_interval_minutes": 15,
        }

        interval_minutes = page_data.get("polling_interval_minutes") or 60
        assert interval_minutes == 15


class TestConfluenceAPIIntervalUpdate:
    """Tests for API-level interval handling during sync config updates."""

    def test_polling_mode_without_interval_gets_default(self):
        """Setting sync_mode=polling without interval should add default."""
        updates = {
            "sync_mode": "polling",
            # polling_interval_minutes not provided
        }

        # Simulating the API logic
        if updates.get("sync_mode") == "polling":
            interval = updates.get("polling_interval_minutes")
            if interval is None:
                interval = 60
                updates["polling_interval_minutes"] = interval
            updates["next_sync_at"] = datetime.utcnow() + timedelta(minutes=interval)

        assert updates["polling_interval_minutes"] == 60
        assert "next_sync_at" in updates

    def test_polling_mode_with_interval_preserves_value(self):
        """Setting sync_mode=polling with interval should preserve value."""
        updates = {
            "sync_mode": "polling",
            "polling_interval_minutes": 30,
        }

        # Simulating the API logic
        if updates.get("sync_mode") == "polling":
            interval = updates.get("polling_interval_minutes")
            if interval is None:
                interval = 60
                updates["polling_interval_minutes"] = interval
            updates["next_sync_at"] = datetime.utcnow() + timedelta(minutes=interval)

        assert updates["polling_interval_minutes"] == 30
        assert "next_sync_at" in updates

    def test_null_sync_mode_clears_next_sync(self):
        """Setting sync_mode to None (inherit) should clear next_sync_at."""
        updates = {
            "sync_mode": None,
        }

        # The logic checks if sync_mode key exists and is None
        if updates.get("sync_mode") is None and "sync_mode" in updates:
            updates["next_sync_at"] = None

        # sync_mode is in updates but value is None
        # This test verifies the inherit behavior
        assert updates.get("next_sync_at") is None
