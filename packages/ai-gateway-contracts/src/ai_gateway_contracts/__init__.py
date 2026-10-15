"""I/O-free cross-service protocol contracts (PRD §ARC-04).

This package holds only protocol models, schema-version constants, pure
validation / signed-payload specifications and I/O-free serialization.  It
must never import database, Redis, HTTP, FastAPI, provider SDK or service
configuration code — ``scripts/core_boundary/check_core_boundary.py``
enforces that mechanically.

Dependency direction: ``ai-gateway-core`` depends on this package, never the
other way around.
"""

from __future__ import annotations

__all__: list[str] = []
