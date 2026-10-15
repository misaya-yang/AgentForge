"""Back-compat shim — Session models moved to ai_gateway_core in Phase 5f Batch C.

Canonical location: ``ai_gateway_core.session.models``.
"""

from __future__ import annotations

from ai_gateway_core.session import Session, SessionMessage

__all__ = ["Session", "SessionMessage"]
