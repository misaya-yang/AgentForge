"""Route modules for the ``/api/v1/assistant`` surface (ARC-01 split).

``src/api/v1/assistant.py`` is the stable facade: it builds the router from
these sub-modules and keeps time-limited compatibility re-exports.  Route
modules must not import each other as libraries — shared helpers live in
``src/services/assistant_entry``.
"""

from __future__ import annotations

from . import artifacts, catalog, chat, memory, metrics, runs, sessions, tools

__all__ = ["artifacts", "catalog", "chat", "memory", "metrics", "runs", "sessions", "tools"]
