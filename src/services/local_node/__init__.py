"""Gateway-owned Local Node control and execution boundary."""

from .control_plane import PostgresLocalNodeControlPlane, build_local_node_control_plane
from .repository import LocalNodeExecutionRepository, PostgresLocalNodeExecutionRepository


def build_local_node_execution_repository(pool):
    """Public Gateway startup hook; never substitutes an in-memory store."""
    return PostgresLocalNodeExecutionRepository(pool)

__all__ = [
    "LocalNodeExecutionRepository",
    "PostgresLocalNodeControlPlane",
    "PostgresLocalNodeExecutionRepository",
    "build_local_node_control_plane",
    "build_local_node_execution_repository",
]
