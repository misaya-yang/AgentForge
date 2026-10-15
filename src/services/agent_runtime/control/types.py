"""Shared vocabulary for the Agent Runtime control-plane subpackage.

ARC-02 split of ``control_plane.py``: constants, dataclasses, and the small
protocol/error types that every control-plane submodule needs.  The facade
``control_plane.py`` re-exports every name that was previously importable
from it, so external import paths are unchanged.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import Any, Protocol

from ai_gateway_core.agents.system_prompt import (
    CORE_ASSISTANT_PROMPT,
    GENERIC_AGENT_INSTRUCTIONS,
)

# Stable, provider-neutral instructions for the generic Assistant. These are
# sent through the Runtime's typed ThreadResume contract, not as user input.
BASE_AGENT_INSTRUCTIONS_V1 = CORE_ASSISTANT_PROMPT
GENERIC_AGENT_INSTRUCTIONS_V1 = GENERIC_AGENT_INSTRUCTIONS
DISCOVERY_BRIDGE_NAMES = frozenset({"tool_search", "tool_describe", "tool_call"})
KERNEL_OWNED_AGENT_TOOL_ALIASES = frozenset(
    {
        # The Rust kernel owns deferred tool search/call and compaction. Keep
        # the public compatibility records, but never install a second set of
        # dynamic functions that would recurse through the Worker.
        "tool_search",
        "tool_describe",
        "tool_call",
        "context_compact",
        # The kernel exposes the native spawn_agent lifecycle.
        "spawn_subagent",
    }
)


class _Database(Protocol):
    async def fetchrow(self, query: str, *args): ...

    async def execute(self, query: str, *args): ...


@dataclass(frozen=True, slots=True)
class AgentTurn:
    runtime_thread_id: str
    run_id: str
    snapshot_id: str
    lease_id: str
    after_sequence: int
    requested_reasoning_option: str
    effective_reasoning_option: str
    reasoning_adapter_id: str
    capability_revision: int
    fallback_reason: str | None


class AgentRuntimeControlError(RuntimeError):
    def __init__(self, code: str, *, status_code: int = 409) -> None:
        self.code = code
        self.status_code = status_code
        super().__init__(code)


def _provider_revision(value: Any) -> str:
    if isinstance(value, datetime):
        return value.isoformat()
    return str(value or "")


__all__ = [
    "BASE_AGENT_INSTRUCTIONS_V1",
    "DISCOVERY_BRIDGE_NAMES",
    "GENERIC_AGENT_INSTRUCTIONS_V1",
    "KERNEL_OWNED_AGENT_TOOL_ALIASES",
    "AgentRuntimeControlError",
    "AgentTurn",
    "_Database",
    "_provider_revision",
]
