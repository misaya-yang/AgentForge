"""Gateway-owned read-only schema gate, before shared storage bootstrap."""

from __future__ import annotations

from ai_gateway_core.logging import get_logger

from database.authority.commands import command_startup_check, default_paths
from database.authority.runner import MigrationAuthority

logger = get_logger(__name__)


async def require_compatible_schema(dsn: str) -> None:
    """Check the Python Gateway window using its own application identity."""
    authority = MigrationAuthority(dsn, default_paths())
    await command_startup_check(authority, principal="gateway", log=logger.info)
