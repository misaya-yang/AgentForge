"""
Unit tests for File Cleanup Service

Tests:
- TTL-based file cleanup
- Quota enforcement
- Storage statistics
- Manual cleanup triggers
"""

import os
import tempfile
from datetime import datetime, timedelta
from pathlib import Path

import pytest


# We need to set environment variables before importing the module
@pytest.fixture
def temp_storage():
    """Create a temporary storage directory."""
    with tempfile.TemporaryDirectory() as tmpdir:
        yield Path(tmpdir)


@pytest.fixture
def cleanup_config(temp_storage):
    """Create a cleanup config for testing."""
    from src.core.file_cleanup import CleanupConfig

    return CleanupConfig(
        storage_path=temp_storage,
        file_ttl_days=1,  # 1 day for testing
        user_quota_mb=10,  # 10 MB quota
        cleanup_interval_hours=1,
        cleanup_enabled=True,
    )


@pytest.fixture
def cleanup_service(cleanup_config):
    """Create a cleanup service instance."""
    from src.core.file_cleanup import FileCleanupService

    return FileCleanupService(config=cleanup_config)


@pytest.fixture
def sample_files(temp_storage):
    """Create sample files for testing."""
    uploads_dir = temp_storage / "uploads"
    user_dir = uploads_dir / "test_user"
    user_dir.mkdir(parents=True, exist_ok=True)

    files = []

    # Create some test files
    for i in range(5):
        file_path = user_dir / f"file_{i}.txt"
        file_path.write_text(f"Test content {i}" * 100)  # ~1KB each
        files.append(file_path)

    return files


class TestFileCleanupService:
    """Tests for FileCleanupService."""

    @pytest.mark.asyncio
    async def test_get_storage_stats_empty(self, cleanup_service, temp_storage):
        """Test storage stats on empty directory."""
        stats = await cleanup_service.get_storage_stats()

        assert stats["total_files"] == 0
        assert stats["total_size"] == 0
        assert stats["users"] == []

    @pytest.mark.asyncio
    async def test_get_storage_stats_with_files(self, cleanup_service, sample_files):
        """Test storage stats with files present."""
        stats = await cleanup_service.get_storage_stats()

        assert stats["total_files"] == 5
        assert stats["total_size"] > 0
        assert len(stats["users"]) == 1
        assert stats["users"][0]["user_id"] == "test_user"
        assert stats["users"][0]["files"] == 5

    @pytest.mark.asyncio
    async def test_ttl_cleanup(self, cleanup_service, temp_storage):
        """Test TTL-based cleanup."""
        uploads_dir = temp_storage / "uploads"
        user_dir = uploads_dir / "test_user"
        user_dir.mkdir(parents=True, exist_ok=True)

        # Create an old file (modify time in the past)
        old_file = user_dir / "old_file.txt"
        old_file.write_text("Old content")

        # Set modification time to 2 days ago
        old_time = (datetime.now() - timedelta(days=2)).timestamp()
        os.utime(old_file, (old_time, old_time))

        # Create a new file
        new_file = user_dir / "new_file.txt"
        new_file.write_text("New content")

        # Run cleanup
        stats = await cleanup_service.run_cleanup()

        # Old file should be deleted
        assert not old_file.exists()
        # New file should remain
        assert new_file.exists()
        assert stats.files_deleted >= 1

    @pytest.mark.asyncio
    async def test_quota_enforcement(self, cleanup_service, temp_storage):
        """Test per-user quota enforcement."""
        uploads_dir = temp_storage / "uploads"
        user_dir = uploads_dir / "test_user"
        user_dir.mkdir(parents=True, exist_ok=True)

        # Override quota to 1KB for testing
        cleanup_service.config.user_quota_mb = 0.001  # ~1KB

        # Create files that exceed quota
        files = []
        for i in range(5):
            file_path = user_dir / f"file_{i}.txt"
            file_path.write_text("X" * 500)  # 500 bytes each
            files.append(file_path)
            # Stagger modification times
            mtime = (datetime.now() - timedelta(minutes=5 - i)).timestamp()
            os.utime(file_path, (mtime, mtime))

        # Run cleanup
        stats = await cleanup_service.run_cleanup()

        # Some files should be deleted to enforce quota
        remaining_files = list(user_dir.iterdir())
        assert len(remaining_files) < 5
        assert stats.files_deleted > 0

    @pytest.mark.asyncio
    async def test_cleanup_user(self, cleanup_service, sample_files):
        """Test cleaning up specific user."""
        # Modify file times to be old
        for f in sample_files:
            old_time = (datetime.now() - timedelta(days=2)).timestamp()
            os.utime(f, (old_time, old_time))

        result = await cleanup_service.cleanup_user("test_user")

        assert result["files_deleted"] == 5
        assert result["bytes_freed"] > 0

    @pytest.mark.asyncio
    async def test_cleanup_nonexistent_user(self, cleanup_service):
        """Test cleaning up non-existent user."""
        result = await cleanup_service.cleanup_user("nonexistent_user")

        assert result["files_deleted"] == 0
        assert "not found" in result["message"].lower()

    @pytest.mark.asyncio
    async def test_cleanup_user_rejects_path_traversal(self, cleanup_service, temp_storage):
        """Manual cleanup must not resolve outside the uploads root."""
        outside_dir = temp_storage / "outside"
        outside_dir.mkdir()
        protected_file = outside_dir / "protected.txt"
        protected_file.write_text("do not delete")

        with pytest.raises(ValueError):
            await cleanup_service.cleanup_user("../outside")

        assert protected_file.exists()

    @pytest.mark.asyncio
    async def test_empty_directory_cleanup(self, cleanup_service, temp_storage):
        """Test that empty user directories are removed."""
        uploads_dir = temp_storage / "uploads"
        empty_user_dir = uploads_dir / "empty_user"
        empty_user_dir.mkdir(parents=True, exist_ok=True)

        # Create and then delete a file
        temp_file = empty_user_dir / "temp.txt"
        temp_file.write_text("temp")
        temp_file.unlink()

        # Run cleanup
        await cleanup_service.run_cleanup()

        # Empty directory should be removed
        assert not empty_user_dir.exists()

    @pytest.mark.asyncio
    async def test_service_start_stop(self, cleanup_service):
        """Test service start and stop."""
        # Should not error
        await cleanup_service.start()
        assert cleanup_service._running

        await cleanup_service.stop()
        assert not cleanup_service._running

    @pytest.mark.asyncio
    async def test_service_disabled(self, cleanup_config):
        """Test that disabled service doesn't start."""
        from src.core.file_cleanup import FileCleanupService

        cleanup_config.cleanup_enabled = False
        service = FileCleanupService(config=cleanup_config)

        await service.start()

        # Should not be running when disabled
        assert not service._running


class TestCleanupConfig:
    """Tests for CleanupConfig."""

    def test_default_config(self):
        """Test default configuration values."""
        from src.core.file_cleanup import CleanupConfig

        config = CleanupConfig()

        assert config.file_ttl_days > 0
        assert config.user_quota_mb > 0
        assert config.cleanup_interval_hours > 0

    def test_uploads_path(self, cleanup_config):
        """Test uploads_path property."""
        assert cleanup_config.uploads_path == cleanup_config.storage_path / "uploads"


class TestFormatSize:
    """Tests for _format_file_size helper."""

    def test_format_bytes(self, cleanup_service):
        """Test formatting bytes."""
        assert "B" in cleanup_service._format_size(100)
        assert "512" in cleanup_service._format_size(512)

    def test_format_kb(self, cleanup_service):
        """Test formatting kilobytes."""
        result = cleanup_service._format_size(2048)
        assert "KB" in result

    def test_format_mb(self, cleanup_service):
        """Test formatting megabytes."""
        result = cleanup_service._format_size(5 * 1024 * 1024)
        assert "MB" in result

    def test_format_gb(self, cleanup_service):
        """Test formatting gigabytes."""
        result = cleanup_service._format_size(2 * 1024 * 1024 * 1024)
        assert "GB" in result
