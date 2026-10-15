"""Shared background-task primitives.

Gateway lifecycle and public task routes share these service-neutral contracts.
"""

from .task_manager import (
    TaskContext,
    get_task_manager,
    init_task_manager,
    shutdown_task_manager,
)
from .task_types import process_file_task

__all__ = [
    "TaskContext",
    "get_task_manager",
    "init_task_manager",
    "process_file_task",
    "shutdown_task_manager",
]
