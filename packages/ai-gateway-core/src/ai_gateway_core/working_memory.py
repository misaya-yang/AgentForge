"""
Working Memory with todo.md Pattern for Task Focus.

This module implements the "attention manipulation through recitation" pattern
from Manus Context Engineering. By maintaining a todo.md-style task state
in the context, we leverage the recency effect to keep the agent focused
on the current objective during long, multi-step workflows.

Key Features:
- Task list with status tracking (pending, in_progress, completed, failed, blocked)
- Collected information storage for intermediate results
- Notes for observations and decisions
- Markdown rendering for context injection

Reference: https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any


class TaskStatus(str, Enum):
    """Status values for task items."""

    PENDING = "pending"
    IN_PROGRESS = "in_progress"
    COMPLETED = "completed"
    FAILED = "failed"
    BLOCKED = "blocked"


@dataclass
class TaskItem:
    """
    Represents a single task in the working memory.

    Attributes:
        id: Unique identifier for the task
        description: Human-readable description of what needs to be done
        status: Current status of the task
        result: Result or output from completing the task (truncated for memory)
        error: Error message if task failed
        created_at: When the task was created
        completed_at: When the task was completed (if applicable)
    """

    id: str
    description: str
    status: TaskStatus = TaskStatus.PENDING
    result: str | None = None
    error: str | None = None
    created_at: datetime = field(default_factory=datetime.now)
    completed_at: datetime | None = None

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization."""
        return {
            "id": self.id,
            "description": self.description,
            "status": self.status.value,
            "result": self.result,
            "error": self.error,
            "created_at": self.created_at.isoformat(),
            "completed_at": self.completed_at.isoformat() if self.completed_at else None,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> TaskItem:
        """Create from dictionary."""
        return cls(
            id=data["id"],
            description=data["description"],
            status=TaskStatus(data["status"]),
            result=data.get("result"),
            error=data.get("error"),
            created_at=datetime.fromisoformat(data["created_at"]),
            completed_at=(
                datetime.fromisoformat(data["completed_at"]) if data.get("completed_at") else None
            ),
        )


@dataclass
class CollectedInfo:
    """
    Information collected during task execution.

    Stores intermediate results, search findings, and other data
    that may be needed for subsequent tasks.

    Attributes:
        key: Short identifier for the information
        value: The actual information content
        source: Where this information came from (tool name or "user")
        timestamp: When this information was collected
    """

    key: str
    value: str
    source: str
    timestamp: datetime = field(default_factory=datetime.now)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization."""
        return {
            "key": self.key,
            "value": self.value,
            "source": self.source,
            "timestamp": self.timestamp.isoformat(),
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> CollectedInfo:
        """Create from dictionary."""
        return cls(
            key=data["key"],
            value=data["value"],
            source=data["source"],
            timestamp=datetime.fromisoformat(data["timestamp"]),
        )


# Turns of inactivity before a task-less goal is considered stale and archived.
GOAL_ONLY_SETTLE_AFTER_TURNS = 3


class WorkingMemory:
    """
    Maintains task state and collected information during complex workflows.

    This class implements the "todo.md pattern" for maintaining agent focus.
    By keeping a structured task list and rendering it as markdown, we can
    inject it into the context to leverage the recency effect and prevent
    goal drift during long, multi-step tasks.

    Usage:
        ```python
        memory = WorkingMemory(session_id="session_123")
        memory.set_goal("Generate a product comparison report")

        # Add tasks
        memory.add_task("task_1", "Search KB for Product A specs")
        memory.add_task("task_2", "Search KB for Product B specs")
        memory.add_task("task_3", "Generate comparison table")

        # Update task status
        memory.update_task("task_1", TaskStatus.IN_PROGRESS)

        # Record collected info
        memory.add_info("product_a_price", "$1000", source="kb_search")

        # Render for context injection
        markdown = memory.to_markdown()
        ```

    The rendered markdown is injected into ContextStructure.task_state
    to be included in the system prompt.
    """

    def __init__(self, session_id: str):
        """
        Initialize working memory for a session.

        Args:
            session_id: Unique identifier for the session
        """
        self.session_id = session_id
        self.goal: str | None = None
        self.goal_set_at: datetime | None = None
        self.turns_since_goal: int = 0
        self.tasks: list[TaskItem] = []
        self.collected_info: list[CollectedInfo] = []
        self.notes: list[str] = []
        self.archived: dict[str, Any] | None = None

    def set_goal(self, goal: str) -> None:
        """
        Set the high-level goal for this task.

        Args:
            goal: Description of what the user wants to achieve
        """
        self.goal = goal
        self.goal_set_at = datetime.now()
        self.turns_since_goal = 0

    def add_task(self, task_id: str, description: str) -> TaskItem:
        """
        Add a new task to the plan.

        Args:
            task_id: Unique identifier for the task
            description: Human-readable task description

        Returns:
            The created TaskItem
        """
        task = TaskItem(id=task_id, description=description)
        self.tasks.append(task)
        return task

    def update_task(
        self,
        task_id: str,
        status: TaskStatus,
        result: str | None = None,
        error: str | None = None,
    ) -> bool:
        """
        Update task status and optionally set result or error.

        Args:
            task_id: ID of the task to update
            status: New status for the task
            result: Result from completing the task (will be truncated to 500 chars)
            error: Error message if task failed

        Returns:
            True if task was found and updated, False otherwise
        """
        for task in self.tasks:
            if task.id == task_id:
                task.status = status
                if result is not None:
                    # Truncate result to prevent context bloat
                    task.result = result[:500] if len(result) > 500 else result
                if error is not None:
                    task.error = error
                if status == TaskStatus.COMPLETED:
                    task.completed_at = datetime.now()
                return True
        return False

    def get_task(self, task_id: str) -> TaskItem | None:
        """
        Get a task by ID.

        Args:
            task_id: ID of the task to retrieve

        Returns:
            The TaskItem if found, None otherwise
        """
        for task in self.tasks:
            if task.id == task_id:
                return task
        return None

    def get_current_task(self) -> TaskItem | None:
        """
        Get the currently in-progress task.

        Returns:
            The first task with IN_PROGRESS status, or None
        """
        for task in self.tasks:
            if task.status == TaskStatus.IN_PROGRESS:
                return task
        return None

    def add_info(self, key: str, value: str, source: str) -> None:
        """
        Record collected information.

        Args:
            key: Short identifier for the information
            value: The actual information content
            source: Where this information came from (tool name or "user")
        """
        self.collected_info.append(CollectedInfo(key=key, value=value, source=source))

    def add_note(self, note: str) -> None:
        """
        Add a note or observation.

        Args:
            note: The note content
        """
        self.notes.append(note)

    def to_markdown(self) -> str:
        """
        Render working memory as todo.md format.

        This is placed at the end of the system prompt to leverage
        the recency effect and maintain task focus during complex workflows.

        Returns:
            Markdown-formatted string representing current task state

        Example output:
            ```markdown
            # Current Task State

            **Goal:** Generate a product comparison report

            ## Tasks
            - [x] Search KB for Product A specs
            - [~] Search KB for Product B specs <- current
            - [ ] Generate comparison table

            ## Collected Information
            - **product_a_price**: $1000

            ## Notes
            - User prefers table format for comparisons
            ```
        """
        lines: list[str] = ["# Current Task State", ""]

        # Goal section
        if self.goal:
            lines.append(f"**Goal:** {self.goal}")
            lines.append("")

        # Task list with status indicators
        if self.tasks:
            lines.append("## Tasks")
            for task in self.tasks:
                indicator = self._get_status_indicator(task.status)
                line = f"- {indicator} {task.description}"

                # Mark current task
                if task.status == TaskStatus.IN_PROGRESS:
                    line += " <- current"

                # Show error for failed tasks
                if task.error:
                    line += f" (error: {task.error})"

                lines.append(line)
            lines.append("")

        # Collected information section
        if self.collected_info:
            lines.append("## Collected Information")
            for info in self.collected_info:
                # Truncate long values in display
                display_value = info.value
                if len(display_value) > 100:
                    display_value = display_value[:100] + "..."
                lines.append(f"- **{info.key}**: {display_value}")
            lines.append("")

        # Notes section
        if self.notes:
            lines.append("## Notes")
            for note in self.notes:
                lines.append(f"- {note}")
            lines.append("")

        return "\n".join(lines)

    def _get_status_indicator(self, status: TaskStatus) -> str:
        """
        Get the markdown checkbox indicator for a task status.

        Args:
            status: The task status

        Returns:
            Markdown indicator string
        """
        indicators = {
            TaskStatus.PENDING: "[ ]",
            TaskStatus.IN_PROGRESS: "[~]",
            TaskStatus.COMPLETED: "[x]",
            TaskStatus.FAILED: "[!]",
            TaskStatus.BLOCKED: "[B]",
        }
        return indicators.get(status, "[ ]")

    _UNRESOLVED = frozenset(
        {TaskStatus.PENDING, TaskStatus.IN_PROGRESS, TaskStatus.BLOCKED}
    )
    _SETTLED = frozenset({TaskStatus.COMPLETED, TaskStatus.FAILED})

    def has_active_job(self) -> bool:
        """Return whether this session still has live work to carry forward."""

        if any(task.status in self._UNRESOLVED for task in self.tasks):
            return True
        return bool(self.goal) and not self.tasks

    def archive_if_settled(self, *, turns_since_goal: int = 0) -> bool:
        """Clear finished work so later turns do not inherit a stale job.

        Task status is the primary signal; a task-less goal settles after
        ``GOAL_ONLY_SETTLE_AFTER_TURNS`` turns of inactivity so goal-only
        sessions do not carry a stale objective forever.  The pre-clear
        state is preserved in ``archived`` before it is wiped.
        """

        if self.tasks:
            if any(task.status in self._UNRESOLVED for task in self.tasks):
                return False
            if not all(task.status in self._SETTLED for task in self.tasks):
                return False
        elif not self.goal or turns_since_goal < GOAL_ONLY_SETTLE_AFTER_TURNS:
            return False
        self.preserve_snapshot()
        self.clear()
        return True

    def preserve_snapshot(self) -> None:
        """Keep a pre-clear copy of the live state for audit and debugging.

        Callers that destructively replace the state (settle, todo rewrite)
        invoke this first so goal/notes/collected info are not lost silently.
        Each destructive event refreshes the snapshot to the current live
        state, so a later settle captures everything accumulated since an
        earlier todo rewrite.  The nested ``archived`` key is dropped in the
        snapshot to avoid unbounded nesting across work cycles.
        """

        if self.goal or self.tasks or self.collected_info or self.notes:
            snapshot = self.to_dict()
            snapshot["archived"] = None
            self.archived = snapshot

    def get_progress(self) -> dict[str, Any]:
        """
        Get task completion progress statistics.

        Returns:
            Dictionary with progress information:
            - total: Total number of tasks
            - completed: Number of completed tasks
            - failed: Number of failed tasks
            - percentage: Completion percentage (0-100)
        """
        total = len(self.tasks)
        completed = sum(1 for t in self.tasks if t.status == TaskStatus.COMPLETED)
        failed = sum(1 for t in self.tasks if t.status == TaskStatus.FAILED)

        return {
            "total": total,
            "completed": completed,
            "failed": failed,
            "percentage": (completed / total * 100) if total > 0 else 0,
        }

    def clear(self) -> None:
        """Clear all working memory state (the archived snapshot is kept)."""
        self.goal = None
        self.goal_set_at = None
        self.turns_since_goal = 0
        self.tasks = []
        self.collected_info = []
        self.notes = []

    def to_dict(self) -> dict[str, Any]:
        """
        Serialize working memory to dictionary.

        Returns:
            Dictionary representation for persistence
        """
        return {
            "session_id": self.session_id,
            "goal": self.goal,
            "goal_set_at": self.goal_set_at.isoformat() if self.goal_set_at else None,
            "turns_since_goal": self.turns_since_goal,
            "tasks": [t.to_dict() for t in self.tasks],
            "collected_info": [i.to_dict() for i in self.collected_info],
            "notes": self.notes,
            "archived": self.archived,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> WorkingMemory:
        """
        Deserialize working memory from dictionary.

        Args:
            data: Dictionary representation

        Returns:
            WorkingMemory instance
        """
        memory = cls(session_id=data["session_id"])
        memory.goal = data.get("goal")
        memory.goal_set_at = (
            datetime.fromisoformat(data["goal_set_at"]) if data.get("goal_set_at") else None
        )
        memory.turns_since_goal = int(data.get("turns_since_goal", 0))
        memory.tasks = [TaskItem.from_dict(t) for t in data.get("tasks", [])]
        memory.collected_info = [CollectedInfo.from_dict(i) for i in data.get("collected_info", [])]
        memory.notes = data.get("notes", [])
        memory.archived = data.get("archived")
        return memory

    @classmethod
    def from_persisted_dict(
        cls,
        data: dict[str, Any],
        *,
        expected_session_id: str,
    ) -> WorkingMemory:
        """Restore bounded state only when it belongs to the active session."""

        if not isinstance(data, dict):
            raise ValueError("Persisted working memory must be an object")
        if str(data.get("session_id") or "") != expected_session_id:
            raise ValueError("Persisted working memory session mismatch")
        if len(json.dumps(data, ensure_ascii=False, default=str)) > 100_000:
            raise ValueError("Persisted working memory exceeds size limit")

        goal = data.get("goal")
        if goal is not None and (not isinstance(goal, str) or len(goal) > 2_000):
            raise ValueError("Persisted working memory goal is invalid")

        tasks = data.get("tasks", [])
        info_items = data.get("collected_info", [])
        notes = data.get("notes", [])
        if not isinstance(tasks, list) or len(tasks) > 100:
            raise ValueError("Persisted working memory task list is invalid")
        if not isinstance(info_items, list) or len(info_items) > 100:
            raise ValueError("Persisted working memory info list is invalid")
        if not isinstance(notes, list) or len(notes) > 100:
            raise ValueError("Persisted working memory notes are invalid")

        task_ids: set[str] = set()
        for task in tasks:
            if not isinstance(task, dict):
                raise ValueError("Persisted working memory task is invalid")
            task_id = task.get("id")
            description = task.get("description")
            if not isinstance(task_id, str) or not task_id or len(task_id) > 128:
                raise ValueError("Persisted working memory task id is invalid")
            if task_id in task_ids:
                raise ValueError("Persisted working memory has duplicate task ids")
            task_ids.add(task_id)
            if not isinstance(description, str) or len(description) > 1_000:
                raise ValueError("Persisted working memory task description is invalid")
            for field_name in ("result", "error"):
                value = task.get(field_name)
                if value is not None and (not isinstance(value, str) or len(value) > 500):
                    raise ValueError(f"Persisted working memory task {field_name} is invalid")

        for info in info_items:
            if not isinstance(info, dict):
                raise ValueError("Persisted working memory info item is invalid")
            for field_name, max_chars in (
                ("key", 128),
                ("value", 2_000),
                ("source", 128),
            ):
                value = info.get(field_name)
                if not isinstance(value, str) or len(value) > max_chars:
                    raise ValueError(f"Persisted working memory info {field_name} is invalid")

        if any(not isinstance(note, str) or len(note) > 1_000 for note in notes):
            raise ValueError("Persisted working memory note is invalid")

        archived = data.get("archived")
        if archived is not None and not isinstance(archived, dict):
            raise ValueError("Persisted working memory archived snapshot is invalid")

        turns = data.get("turns_since_goal", 0)
        if isinstance(turns, bool) or not isinstance(turns, int) or not 0 <= turns <= 100_000:
            raise ValueError("Persisted working memory turns_since_goal is invalid")

        try:
            return cls.from_dict(data)
        except (KeyError, TypeError, ValueError) as exc:
            raise ValueError("Persisted working memory payload is invalid") from exc
