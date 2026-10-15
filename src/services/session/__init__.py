from __future__ import annotations

from importlib import import_module
from typing import Any

__all__ = [
    "SessionManager",
    "DatabaseSessionManager",
]


_EXPORTS = {
    "SessionManager": (".session_manager", "SessionManager"),
    "DatabaseSessionManager": (".database_session_manager", "DatabaseSessionManager"),
}


def __getattr__(name: str) -> Any:
    target = _EXPORTS.get(name)
    if not target:
        raise AttributeError(name)
    module_name, attr_name = target
    module = import_module(module_name, __package__)
    return getattr(module, attr_name)
