"""Gateway-owned MCP protocol client and resilience primitives."""

from .client import MCPClient, MCPError, MCPServerConfig, MCPTool, MCPToolResult
from .resilience import (
    MCPFailureDecision,
    MCPFailureKind,
    MCPInvocationPolicy,
    MCPOperationKind,
)

__all__ = [
    "MCPClient",
    "MCPError",
    "MCPFailureDecision",
    "MCPFailureKind",
    "MCPInvocationPolicy",
    "MCPOperationKind",
    "MCPServerConfig",
    "MCPTool",
    "MCPToolResult",
]
