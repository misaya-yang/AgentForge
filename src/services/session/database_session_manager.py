"""Back-compat shim — DatabaseSessionManager moved to ai_gateway_core in Phase 5f Batch C.

Canonical location: ``ai_gateway_core.session.database_manager``.
"""

from __future__ import annotations

from ai_gateway_core.session import DatabaseSessionManager

__all__ = ["DatabaseSessionManager"]
