from __future__ import annotations

from importlib import import_module
from typing import Any

__all__ = [
    "TaskManager",
    "TaskStorage",
    "MemoryTaskStorage",
    "DatabaseTaskStorage",
    "TaskQueue",
    "MemoryTaskQueue",
    "TaskWorker",
]


_EXPORTS = {
    "TaskManager": (".task_manager", "TaskManager"),
    "TaskStorage": (".task_manager", "TaskStorage"),
    "MemoryTaskStorage": (".task_manager", "MemoryTaskStorage"),
    "DatabaseTaskStorage": (".database_task_storage", "DatabaseTaskStorage"),
    "TaskQueue": (".task_queue", "TaskQueue"),
    "MemoryTaskQueue": (".task_queue", "MemoryTaskQueue"),
    "TaskWorker": (".task_worker", "TaskWorker"),
}


def __getattr__(name: str) -> Any:
    target = _EXPORTS.get(name)
    if not target:
        raise AttributeError(name)
    module_name, attr_name = target
    module = import_module(module_name, __package__)
    return getattr(module, attr_name)
