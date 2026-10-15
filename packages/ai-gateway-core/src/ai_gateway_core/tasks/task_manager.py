"""
Task Manager - Enterprise Session Isolation and Resource Management.

This module provides the TaskManager class for managing session resources
with enterprise-grade isolation, concurrency control, and automatic cleanup.

Key Features:
- Session isolation: Each session has dedicated resources
- Concurrency control: Semaphore-based limiting per session
- Memory management: Automatic cleanup of expired sessions
- Task tracking: Monitor active tasks per session
- Thread safety: Asyncio primitives for safe concurrent access

Design Philosophy:
- Each session_id gets independent resources (WorkingMemory, locks)
- Resources are automatically cleaned up after timeout
- Concurrent requests to the same session are serialized with locks
- Maximum sessions are enforced with LRU eviction

Usage:
    ```python
    manager = TaskManager(max_sessions=1000)
    await manager.start()  # Start cleanup background task

    # Get or create session resources
    async with manager.session_context(
        session_id="session_123",
        tenant_id="tenant_1",
        user_id="user_1"
    ) as session:
        # Access working memory (thread-safe)
        async with session.lock:
            session.working_memory.add_task("task_1", "Do something")

        # Execute with concurrency limit
        async with session.semaphore:
            result = await execute_tool(...)

    await manager.stop()  # Clean up on shutdown
    ```

References:
- Enterprise AI Agent patterns
- Session isolation for multi-tenant systems
"""

from __future__ import annotations

import asyncio
import hashlib
import uuid
from collections.abc import AsyncGenerator
from contextlib import asynccontextmanager, suppress
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from typing import Any

from ai_gateway_core.logging import get_logger
from ai_gateway_core.working_memory import WorkingMemory

logger = get_logger(__name__)


class SessionDeletionBusyError(RuntimeError):
    """Raised when a session cannot enter the deletion admission fence."""


# =============================================================================
# Data Classes
# =============================================================================


@dataclass
class SessionResources:
    """
    Resources allocated to a session.

    Each session gets its own isolated set of resources including:
    - Working memory for task state tracking
    - Lock for thread-safe access
    - Semaphore for concurrency control
    - Tracking of active tasks

    Attributes:
        session_id: Unique session identifier
        tenant_id: Tenant for multi-tenancy isolation
        user_id: User owning this session
        working_memory: Task state tracking
        active_tasks: Set of currently running task IDs
        pending_tool_calls: Map of pending tool call IDs to requests
        created_at: When the session was created
        last_activity: Last activity timestamp (for timeout)
        timeout_seconds: Session timeout in seconds
        lock: Asyncio lock for thread-safe access
        semaphore: Asyncio semaphore for concurrency control
    """

    session_id: str
    tenant_id: str
    user_id: str

    # Memory resources (initialized in __post_init__ if not provided)
    working_memory: WorkingMemory | None = None

    # Execution state
    active_tasks: set[str] = field(default_factory=set)
    pending_tool_calls: dict[str, Any] = field(default_factory=dict)
    active_contexts: int = 0
    deletion_pending: bool = False

    # Timing (using UTC for consistent timezone handling)
    created_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    last_activity: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    timeout_seconds: int = 3600  # 1 hour default

    # Concurrency control (created in __post_init__)
    lock: asyncio.Lock = field(default_factory=asyncio.Lock)
    semaphore: asyncio.Semaphore | None = None  # Initialized in __post_init__

    # Maximum concurrent tasks per session
    max_concurrent_tasks: int = 5

    def __post_init__(self):
        """Initialize semaphore and working memory with correct parameters."""
        # Initialize semaphore with the configured concurrency limit
        self.semaphore = asyncio.Semaphore(self.max_concurrent_tasks)
        # Initialize working memory with session_id if not provided
        if self.working_memory is None:
            self.working_memory = WorkingMemory(session_id=self.session_id)

    def is_expired(self) -> bool:
        """Check if session has timed out."""
        return datetime.now(timezone.utc) - self.last_activity > timedelta(
            seconds=self.timeout_seconds
        )

    def touch(self) -> None:
        """Update last activity timestamp."""
        self.last_activity = datetime.now(timezone.utc)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization/logging."""
        return {
            "session_id": self.session_id,
            "tenant_id": self.tenant_id,
            "user_id": self.user_id,
            "active_tasks": list(self.active_tasks),
            "pending_tool_calls": len(self.pending_tool_calls),
            "active_contexts": self.active_contexts,
            "deletion_pending": self.deletion_pending,
            "created_at": self.created_at.isoformat(),
            "last_activity": self.last_activity.isoformat(),
            "is_expired": self.is_expired(),
        }


@dataclass
class TaskContext:
    """
    Context for a single task within a session.

    Provides task-level tracking and cancellation support.

    Attributes:
        task_id: Unique task identifier
        session_id: Parent session identifier
        started_at: When the task started
        cancelled: Whether cancellation was requested
        cancel_event: Event to signal cancellation
    """

    task_id: str
    session_id: str
    started_at: datetime = field(default_factory=datetime.now)

    # Cancellation support
    cancelled: bool = False
    cancel_event: asyncio.Event = field(default_factory=asyncio.Event)

    def request_cancel(self) -> None:
        """Request cancellation of this task."""
        self.cancelled = True
        self.cancel_event.set()

    async def wait_for_cancel(self, timeout: float | None = None) -> bool:
        """Wait for cancellation signal."""
        try:
            await asyncio.wait_for(self.cancel_event.wait(), timeout=timeout)
            return True
        except asyncio.TimeoutError:
            return False


# =============================================================================
# Task Manager
# =============================================================================


class TaskManager:
    """
    Manages session resources and task isolation for enterprise workloads.

    Provides:
    - Session isolation: Each session has dedicated resources
    - Concurrency control: Semaphore-based limiting per session
    - Memory management: Automatic cleanup of expired sessions
    - Task tracking: Monitor active tasks per session

    Usage:
        ```python
        manager = TaskManager(max_sessions=1000)

        # Get or create session
        async with manager.session_context(
            session_id="session_123",
            tenant_id="tenant_1",
            user_id="user_1"
        ) as session:
            # Access working memory (thread-safe)
            async with session.lock:
                session.working_memory.add_task("task_1", "Do something")

            # Execute with concurrency limit
            async with session.semaphore:
                result = await execute_tool(...)
        ```
    """

    def __init__(
        self,
        max_sessions: int = 1000,
        default_timeout_seconds: int = 3600,
        cleanup_interval_seconds: int = 300,
        max_concurrent_per_session: int = 5,
    ):
        """
        Initialize the TaskManager.

        Args:
            max_sessions: Maximum number of concurrent sessions
            default_timeout_seconds: Default session timeout (1 hour)
            cleanup_interval_seconds: Cleanup interval (5 minutes)
            max_concurrent_per_session: Max concurrent tasks per session
        """
        self.max_sessions = max_sessions
        self.default_timeout = default_timeout_seconds
        self.cleanup_interval = cleanup_interval_seconds
        self.max_concurrent_per_session = max_concurrent_per_session

        self._sessions: dict[str, SessionResources] = {}
        self._task_contexts: dict[str, TaskContext] = {}  # task_id -> TaskContext
        self._lock = asyncio.Lock()
        self._cleanup_task: asyncio.Task | None = None
        self._started = False

    async def start(self) -> None:
        """Start the cleanup background task."""
        if self._started:
            return

        self._cleanup_task = asyncio.create_task(self._cleanup_loop())
        self._started = True
        logger.info(
            f"TaskManager started with max_sessions={self.max_sessions}, "
            f"cleanup_interval={self.cleanup_interval}s"
        )

    async def stop(self) -> None:
        """Stop the cleanup task and clean up all sessions."""
        if not self._started:
            return

        if self._cleanup_task:
            self._cleanup_task.cancel()
            with suppress(asyncio.CancelledError):
                await self._cleanup_task

        # Clean up all sessions
        async with self._lock:
            for session in self._sessions.values():
                session.working_memory.clear()
            self._sessions.clear()
            self._task_contexts.clear()

        self._started = False
        logger.info("TaskManager stopped")

    @asynccontextmanager
    async def session_context(
        self,
        session_id: str,
        tenant_id: str,
        user_id: str,
        timeout_seconds: int | None = None,
    ) -> AsyncGenerator[SessionResources, None]:
        """
        Get or create a session with automatic resource management.

        Args:
            session_id: Unique session identifier
            tenant_id: Tenant for multi-tenancy isolation
            user_id: User identifier
            timeout_seconds: Session timeout override

        Yields:
            SessionResources for the session

        Example:
            ```python
            async with manager.session_context("session_1", "tenant_1", "user_1") as session:
                session.working_memory.set_goal("Complete the task")
            ```
        """
        session = await self._get_or_create_session(
            session_id=session_id,
            tenant_id=tenant_id,
            user_id=user_id,
            timeout_seconds=timeout_seconds,
        )

        async with session.lock, self._lock:
            if self._sessions.get(session_id) is not session or session.deletion_pending:
                raise SessionDeletionBusyError("Session deletion is pending")
            session.active_contexts += 1

        try:
            session.touch()
            yield session
        finally:
            async with session.lock, self._lock:
                if self._sessions.get(session_id) is session:
                    session.active_contexts = max(0, session.active_contexts - 1)
            session.touch()

    @asynccontextmanager
    async def session_deletion_context(
        self,
        session_id: str,
        tenant_id: str,
        user_id: str,
    ) -> AsyncGenerator[SessionResources, None]:
        """Fence new run admission while a caller deletes durable session state.

        The session lock remains held across the caller's durable-memory and
        session-row cleanup.  A normal exit removes the in-process resources;
        any exception releases the fence so the still-durable session remains
        usable.
        """

        session = await self._get_or_create_session(
            session_id=session_id,
            tenant_id=tenant_id,
            user_id=user_id,
        )
        fence_owned = False
        finalized = False
        await session.lock.acquire()
        try:
            async with self._lock:
                if self._sessions.get(session_id) is not session:
                    raise SessionDeletionBusyError("Session admission changed")
                if session.deletion_pending:
                    raise SessionDeletionBusyError("Session deletion is already pending")
                session.deletion_pending = True
                fence_owned = True
                if session.active_tasks or session.active_contexts:
                    raise SessionDeletionBusyError("Session has active tasks or contexts")

            yield session

            async with self._lock:
                if self._sessions.get(session_id) is not session:
                    raise SessionDeletionBusyError("Session admission changed")
                if session.active_tasks or session.active_contexts:
                    raise SessionDeletionBusyError("Session gained an active admission")
                self._sessions.pop(session_id)
                session.working_memory.clear()
                to_remove = [
                    task_id
                    for task_id, context in self._task_contexts.items()
                    if context.session_id == session_id
                ]
                for task_id in to_remove:
                    del self._task_contexts[task_id]
                finalized = True
        finally:
            if fence_owned and not finalized:
                async with self._lock:
                    if self._sessions.get(session_id) is session:
                        session.deletion_pending = False
            session.lock.release()

    async def _get_or_create_session(
        self,
        session_id: str,
        tenant_id: str,
        user_id: str,
        timeout_seconds: int | None = None,
    ) -> SessionResources:
        """Get existing session or create new one."""
        async with self._lock:
            if session_id in self._sessions:
                session = self._sessions[session_id]
                if session.tenant_id != tenant_id or session.user_id != user_id:
                    raise PermissionError("Session is already bound to a different tenant or user")
                if session.deletion_pending:
                    raise SessionDeletionBusyError("Session deletion is pending")
                if session.is_expired():
                    if session.active_tasks or session.active_contexts:
                        return session
                    # Clean up expired session
                    session.working_memory.clear()
                    del self._sessions[session_id]
                    logger.debug(f"Removed expired session: {session_id}")
                else:
                    return session

            # Check capacity
            if len(self._sessions) >= self.max_sessions:
                # Remove oldest inactive session
                await self._evict_oldest()

            # Create new session
            session = SessionResources(
                session_id=session_id,
                tenant_id=tenant_id,
                user_id=user_id,
                working_memory=WorkingMemory(session_id=session_id),
                timeout_seconds=timeout_seconds or self.default_timeout,
                max_concurrent_tasks=self.max_concurrent_per_session,
            )
            self._sessions[session_id] = session

            logger.debug(f"Created new session: {session_id}")
            return session

    async def get_session(
        self,
        session_id: str,
        *,
        tenant_id: str | None = None,
        user_id: str | None = None,
    ) -> SessionResources | None:
        """Get a live session, optionally enforcing its tenant/user owner."""
        async with self._lock:
            session = self._sessions.get(session_id)
            if session is None or session.deletion_pending:
                return None
            if session.is_expired() and not session.active_tasks and not session.active_contexts:
                return None
            if tenant_id is not None and session.tenant_id != tenant_id:
                return None
            if user_id is not None and session.user_id != user_id:
                return None
            return session

    async def delete_session(self, session_id: str) -> bool:
        """Delete a session and clean up resources."""
        async with self._lock:
            session = self._sessions.get(session_id)
        if session is None:
            return False
        async with session.lock, self._lock:
            if self._sessions.get(session_id) is not session:
                return False
            if session.deletion_pending or session.active_tasks or session.active_contexts:
                return False
            session.deletion_pending = True
            self._sessions.pop(session_id)
            session.working_memory.clear()

            # Clean up associated task contexts
            to_remove = [
                tid
                for tid, context in self._task_contexts.items()
                if context.session_id == session_id
            ]
            for tid in to_remove:
                del self._task_contexts[tid]

            logger.debug(f"Deleted session: {session_id}")
            return True

    async def register_task(
        self,
        session_id: str,
        task_id: str | None = None,
    ) -> TaskContext | None:
        """
        Register a new task within a session.

        Args:
            session_id: Session to register task in
            task_id: Optional task ID (generated if not provided)

        Returns:
            TaskContext if session exists, None otherwise
        """
        session = await self.get_session(session_id)
        if not session:
            return None

        task_id = task_id or str(uuid.uuid4())
        context = TaskContext(task_id=task_id, session_id=session_id)

        async with session.lock, self._lock:
            if self._sessions.get(session_id) is not session or session.deletion_pending:
                return None
            session.active_tasks.add(task_id)
            self._task_contexts[task_id] = context

        logger.debug(f"Registered task {task_id} in session {session_id}")
        return context

    async def complete_task(
        self,
        session_id: str,
        task_id: str,
    ) -> None:
        """Mark a task as complete."""
        async with self._lock:
            session = self._sessions.get(session_id)
        if session:
            async with session.lock, self._lock:
                if self._sessions.get(session_id) is session:
                    session.active_tasks.discard(task_id)
                self._task_contexts.pop(task_id, None)
        else:
            async with self._lock:
                self._task_contexts.pop(task_id, None)

        logger.debug(f"Completed task {task_id} in session {session_id}")

    async def cancel_task(
        self,
        session_id: str,
        task_id: str,
    ) -> bool:
        """
        Request cancellation of a task.

        Args:
            session_id: Session containing the task
            task_id: Task to cancel

        Returns:
            True if task was found and cancellation requested
        """
        async with self._lock:
            session = self._sessions.get(session_id)
        if session is not None:
            async with session.lock, self._lock:
                context = self._task_contexts.get(task_id)
                if (
                    self._sessions.get(session_id) is session
                    and context is not None
                    and context.session_id == session_id
                ):
                    # Cancellation is only a request. Keep the active admission
                    # until AgentLoop reaches finally/complete_task, otherwise a
                    # durable delete could race a still-running task.
                    context.request_cancel()
                    task_digest = hashlib.sha256(
                        str(task_id).encode("utf-8", errors="replace")
                    ).hexdigest()[:16]
                    logger.info(
                        "event_code=assistant_task_cancel_signal event_type=task_cancel "
                        "task_sha256=%s accepted=true",
                        task_digest,
                    )
                    return True
        return False

    async def get_task_context(self, task_id: str) -> TaskContext | None:
        """Get the context for a task."""
        async with self._lock:
            return self._task_contexts.get(task_id)

    async def get_session_stats(self) -> dict[str, Any]:
        """Get statistics about active sessions."""
        async with self._lock:
            total = len(self._sessions)
            active = sum(1 for s in self._sessions.values() if not s.is_expired())
            total_tasks = sum(len(s.active_tasks) for s in self._sessions.values())

            # Group by tenant
            by_tenant: dict[str, int] = {}
            for s in self._sessions.values():
                by_tenant[s.tenant_id] = by_tenant.get(s.tenant_id, 0) + 1

            return {
                "total_sessions": total,
                "active_sessions": active,
                "expired_sessions": total - active,
                "max_sessions": self.max_sessions,
                "total_active_tasks": total_tasks,
                "sessions_by_tenant": by_tenant,
            }

    async def _cleanup_loop(self) -> None:
        """Background task to clean up expired sessions."""
        while True:
            try:
                await asyncio.sleep(self.cleanup_interval)
                await self._cleanup_expired()
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error(f"Cleanup error: {e}")

    async def _cleanup_expired(self) -> int:
        """Remove expired sessions."""
        async with self._lock:
            expired = [
                sid
                for sid, session in self._sessions.items()
                if session.is_expired()
                and not session.deletion_pending
                and not session.active_tasks
                and not session.active_contexts
            ]
            for sid in expired:
                session = self._sessions.pop(sid)
                session.working_memory.clear()

                # Clean up associated task contexts
                to_remove = [
                    tid for tid, ctx in self._task_contexts.items() if ctx.session_id == sid
                ]
                for tid in to_remove:
                    del self._task_contexts[tid]

            if expired:
                logger.info(f"Cleaned up {len(expired)} expired sessions")

            return len(expired)

    async def _evict_oldest(self) -> None:
        """Evict the oldest inactive session (LRU eviction)."""
        # Find oldest by last_activity
        oldest_id: str | None = None
        oldest_time = datetime.now(timezone.utc)

        for sid, session in self._sessions.items():
            if (
                not session.deletion_pending
                and not session.active_tasks
                and not session.active_contexts
                and session.last_activity < oldest_time
            ):
                oldest_time = session.last_activity
                oldest_id = sid

        if oldest_id:
            session = self._sessions.pop(oldest_id)
            session.working_memory.clear()

            # Clean up associated task contexts
            to_remove = [
                tid for tid, ctx in self._task_contexts.items() if ctx.session_id == oldest_id
            ]
            for tid in to_remove:
                del self._task_contexts[tid]

            logger.info(f"Evicted oldest session: {oldest_id}")


# =============================================================================
# Singleton Instance
# =============================================================================


_task_manager: TaskManager | None = None


def get_task_manager() -> TaskManager:
    """Get the global TaskManager instance."""
    global _task_manager
    if _task_manager is None:
        _task_manager = TaskManager()
    return _task_manager


async def init_task_manager(
    max_sessions: int = 1000,
    default_timeout_seconds: int = 3600,
    cleanup_interval_seconds: int = 300,
) -> TaskManager:
    """
    Initialize and start the global TaskManager.

    Args:
        max_sessions: Maximum concurrent sessions
        default_timeout_seconds: Default session timeout
        cleanup_interval_seconds: Cleanup interval

    Returns:
        The initialized TaskManager
    """
    global _task_manager
    if _task_manager is None:
        _task_manager = TaskManager(
            max_sessions=max_sessions,
            default_timeout_seconds=default_timeout_seconds,
            cleanup_interval_seconds=cleanup_interval_seconds,
        )
    await _task_manager.start()
    return _task_manager


async def shutdown_task_manager() -> None:
    """Shutdown the global TaskManager."""
    global _task_manager
    if _task_manager:
        await _task_manager.stop()
        _task_manager = None
