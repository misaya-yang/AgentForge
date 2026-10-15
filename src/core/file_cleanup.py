"""
File Lifecycle Management Service

Provides automatic cleanup of uploaded files based on:
- TTL (Time-To-Live) - delete files older than X days
- Storage quota - limit total storage per user
- Manual cleanup API endpoints

Usage:
    # In FastAPI lifespan
    from src.core.file_cleanup import FileCleanupService

    cleanup_service = FileCleanupService()

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        await cleanup_service.start()
        yield
        await cleanup_service.stop()
"""

from __future__ import annotations

import asyncio
import contextlib
import os
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import NamedTuple

from ai_gateway_core.logging import get_logger

logger = get_logger(__name__)


# ============ Configuration ============


@dataclass
class CleanupConfig:
    """File cleanup configuration."""

    # Storage path (from environment)
    storage_path: Path = field(
        default_factory=lambda: Path(os.getenv("FILE_STORAGE_PATH", "./uploads")).expanduser()
    )

    # TTL settings
    file_ttl_days: int = int(os.getenv("FILE_TTL_DAYS", "7"))  # Delete files older than X days

    # Quota settings (per user, in MB)
    user_quota_mb: int = int(os.getenv("FILE_USER_QUOTA_MB", "500"))  # 500 MB per user

    # Cleanup schedule
    cleanup_interval_hours: int = int(
        os.getenv("FILE_CLEANUP_INTERVAL_HOURS", "6")
    )  # Run every 6 hours

    # Enable/disable cleanup
    cleanup_enabled: bool = os.getenv("FILE_CLEANUP_ENABLED", "true").lower() == "true"

    @property
    def uploads_path(self) -> Path:
        return self.storage_path / "uploads"


class CleanupStats(NamedTuple):
    """Statistics from a cleanup run."""

    files_deleted: int
    bytes_freed: int
    users_cleaned: int
    errors: int
    duration_ms: int


# ============ Cleanup Service ============


class FileCleanupService:
    """
    Background service for automatic file cleanup.

    Features:
    - TTL-based cleanup: Delete files older than configured days
    - Quota enforcement: Limit storage per user (oldest files deleted first)
    - Scheduled runs: Automatic cleanup at configured intervals
    - Manual triggers: API endpoints for on-demand cleanup
    """

    def __init__(self, config: CleanupConfig | None = None):
        self.config = config or CleanupConfig()
        self._task: asyncio.Task | None = None
        self._running = False

    async def start(self) -> None:
        """Start the background cleanup task."""
        if not self.config.cleanup_enabled:
            logger.info("[FileCleanup] Cleanup disabled by configuration")
            return

        if self._running:
            logger.warning("[FileCleanup] Service already running")
            return

        self._running = True
        self._task = asyncio.create_task(self._run_scheduler())
        logger.info(
            f"[FileCleanup] Started - TTL={self.config.file_ttl_days}d, "
            f"Quota={self.config.user_quota_mb}MB, "
            f"Interval={self.config.cleanup_interval_hours}h"
        )

    async def stop(self) -> None:
        """Stop the background cleanup task."""
        self._running = False
        if self._task:
            self._task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._task
            self._task = None
        logger.info("[FileCleanup] Stopped")

    async def _run_scheduler(self) -> None:
        """Background scheduler loop."""
        # Run initial cleanup after short delay
        await asyncio.sleep(60)  # Wait 1 minute after startup

        while self._running:
            try:
                stats = await self.run_cleanup()
                logger.info(
                    f"[FileCleanup] Completed - "
                    f"deleted={stats.files_deleted} files, "
                    f"freed={self._format_size(stats.bytes_freed)}, "
                    f"users={stats.users_cleaned}, "
                    f"errors={stats.errors}, "
                    f"duration={stats.duration_ms}ms"
                )
            except Exception as e:
                logger.error(f"[FileCleanup] Scheduler error: {e}")

            # Sleep until next run
            await asyncio.sleep(self.config.cleanup_interval_hours * 3600)

    async def run_cleanup(self) -> CleanupStats:
        """
        Run full cleanup operation.

        1. Delete files exceeding TTL
        2. Enforce per-user quota
        3. Clean up empty directories
        """
        start_time = datetime.now(timezone.utc)
        files_deleted = 0
        bytes_freed = 0
        users_cleaned = 0
        errors = 0

        uploads_path = self.config.uploads_path
        if not uploads_path.exists():
            return CleanupStats(0, 0, 0, 0, 0)

        # Process each user directory
        for user_dir in uploads_path.iterdir():
            if not user_dir.is_dir():
                continue

            try:
                user_stats = await self._cleanup_user_directory(user_dir)
                files_deleted += user_stats["files_deleted"]
                bytes_freed += user_stats["bytes_freed"]
                if user_stats["files_deleted"] > 0:
                    users_cleaned += 1
            except Exception as e:
                logger.error(f"[FileCleanup] Error processing {user_dir.name}: {e}")
                errors += 1

        # Clean up empty user directories
        for user_dir in uploads_path.iterdir():
            if user_dir.is_dir() and not any(user_dir.iterdir()):
                try:
                    user_dir.rmdir()
                    logger.debug(f"[FileCleanup] Removed empty directory: {user_dir.name}")
                except Exception as e:
                    logger.warning(f"[FileCleanup] Failed to remove empty dir {user_dir.name}: {e}")

        duration_ms = int((datetime.now(timezone.utc) - start_time).total_seconds() * 1000)

        return CleanupStats(
            files_deleted=files_deleted,
            bytes_freed=bytes_freed,
            users_cleaned=users_cleaned,
            errors=errors,
            duration_ms=duration_ms,
        )

    async def _cleanup_user_directory(self, user_dir: Path) -> dict:
        """Clean up files in a user directory."""
        files_deleted = 0
        bytes_freed = 0

        now = datetime.now(timezone.utc)
        ttl_threshold = now - timedelta(days=self.config.file_ttl_days)
        quota_bytes = self.config.user_quota_mb * 1024 * 1024

        # Get all files with their stats
        files_with_stats = []
        total_size = 0

        for file_path in user_dir.iterdir():
            if file_path.is_file():
                stat = file_path.stat()
                mtime = datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc)
                files_with_stats.append(
                    {
                        "path": file_path,
                        "size": stat.st_size,
                        "mtime": mtime,
                    }
                )
                total_size += stat.st_size

        # Sort by modification time (oldest first)
        files_with_stats.sort(key=lambda x: x["mtime"])

        # Phase 1: TTL-based cleanup
        for file_info in files_with_stats[:]:
            if file_info["mtime"] < ttl_threshold:
                try:
                    file_info["path"].unlink()
                    files_deleted += 1
                    bytes_freed += file_info["size"]
                    total_size -= file_info["size"]
                    files_with_stats.remove(file_info)
                    logger.debug(f"[FileCleanup] TTL expired: {file_info['path'].name}")
                except Exception as e:
                    logger.warning(f"[FileCleanup] Failed to delete {file_info['path'].name}: {e}")

        # Phase 2: Quota enforcement (delete oldest files until under quota)
        while total_size > quota_bytes and files_with_stats:
            file_info = files_with_stats.pop(0)  # Remove oldest
            try:
                file_info["path"].unlink()
                files_deleted += 1
                bytes_freed += file_info["size"]
                total_size -= file_info["size"]
                logger.debug(f"[FileCleanup] Quota exceeded: {file_info['path'].name}")
            except Exception as e:
                logger.warning(f"[FileCleanup] Failed to delete {file_info['path'].name}: {e}")

        return {"files_deleted": files_deleted, "bytes_freed": bytes_freed}

    async def cleanup_user(self, user_id: str) -> dict:
        """
        Clean up files for a specific user.

        Returns cleanup statistics.
        """
        user_dir = self._resolve_user_dir(user_id)
        if not user_dir.exists():
            return {"files_deleted": 0, "bytes_freed": 0, "message": "User directory not found"}

        stats = await self._cleanup_user_directory(user_dir)
        return {
            **stats,
            "bytes_freed_formatted": self._format_size(stats["bytes_freed"]),
            "message": f"Cleaned up {stats['files_deleted']} files for user {user_id}",
        }

    def _resolve_user_dir(self, user_id: str) -> Path:
        uploads_root = self.config.uploads_path.resolve()
        user_dir = (uploads_root / user_id).resolve()
        if not user_id or user_dir == uploads_root:
            raise ValueError("Invalid user_id for cleanup")
        try:
            user_dir.relative_to(uploads_root)
        except ValueError as exc:
            raise ValueError("Invalid user_id for cleanup") from exc
        return user_dir

    async def get_storage_stats(self) -> dict:
        """Get storage statistics."""
        uploads_path = self.config.uploads_path

        if not uploads_path.exists():
            return {
                "total_files": 0,
                "total_size": 0,
                "total_size_formatted": "0 B",
                "users": [],
            }

        users = []
        total_files = 0
        total_size = 0

        for user_dir in uploads_path.iterdir():
            if not user_dir.is_dir():
                continue

            user_files = 0
            user_size = 0
            oldest_file = None
            newest_file = None

            for file_path in user_dir.iterdir():
                if file_path.is_file():
                    stat = file_path.stat()
                    user_files += 1
                    user_size += stat.st_size
                    mtime = datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc)

                    if oldest_file is None or mtime < oldest_file:
                        oldest_file = mtime
                    if newest_file is None or mtime > newest_file:
                        newest_file = mtime

            if user_files > 0:
                users.append(
                    {
                        "user_id": user_dir.name,
                        "files": user_files,
                        "size": user_size,
                        "size_formatted": self._format_size(user_size),
                        "quota_used_percent": round(
                            user_size / (self.config.user_quota_mb * 1024 * 1024) * 100, 1
                        ),
                        "oldest_file": oldest_file.isoformat() if oldest_file else None,
                        "newest_file": newest_file.isoformat() if newest_file else None,
                    }
                )
                total_files += user_files
                total_size += user_size

        return {
            "total_files": total_files,
            "total_size": total_size,
            "total_size_formatted": self._format_size(total_size),
            "users": sorted(users, key=lambda x: x["size"], reverse=True),
            "config": {
                "ttl_days": self.config.file_ttl_days,
                "user_quota_mb": self.config.user_quota_mb,
                "cleanup_interval_hours": self.config.cleanup_interval_hours,
                "cleanup_enabled": self.config.cleanup_enabled,
            },
        }

    def _format_size(self, size_bytes: int) -> str:
        """Format bytes to human-readable size."""
        for unit in ["B", "KB", "MB", "GB"]:
            if size_bytes < 1024:
                return f"{size_bytes:.1f} {unit}"
            size_bytes /= 1024
        return f"{size_bytes:.1f} TB"


# ============ Singleton Instance ============

_cleanup_service: FileCleanupService | None = None


def get_cleanup_service() -> FileCleanupService:
    """Get the singleton cleanup service instance."""
    global _cleanup_service
    if _cleanup_service is None:
        _cleanup_service = FileCleanupService()
    return _cleanup_service
