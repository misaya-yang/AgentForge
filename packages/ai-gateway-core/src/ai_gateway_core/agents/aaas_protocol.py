"""2026 SOTA Agent-as-a-Service (AaaS) Protocol Definitions.

Provides standardized request/response models and envelope schemas conforming to
the 2026 open Agent Protocol and OpenAI Responses API specification.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any


@dataclass
class AaaSTaskStep:
    """A single discrete step in an agent's execution trajectory."""

    step_id: str
    turn_index: int
    status: str  # "running", "completed", "failed", "awaiting_approval"
    thought: str | None = None
    tool_call: dict[str, Any] | None = None
    tool_result: dict[str, Any] | None = None
    content_delta: str | None = None
    timestamp: str = field(default_factory=lambda: datetime.utcnow().isoformat())

    def to_dict(self) -> dict[str, Any]:
        return {
            "step_id": self.step_id,
            "turn_index": self.turn_index,
            "status": self.status,
            "thought": self.thought,
            "tool_call": self.tool_call,
            "tool_result": self.tool_result,
            "content_delta": self.content_delta,
            "timestamp": self.timestamp,
        }


@dataclass
class AaaSRunRequest:
    """Standardized client request to trigger an autonomous agent run."""

    agent_id: str
    input_prompt: str
    session_id: str | None = None
    stream: bool = True
    max_turns: int = 30
    temperature: float = 0.7
    context: dict[str, Any] = field(default_factory=dict)
    tool_allowlist: list[str] = field(default_factory=list)
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "agent_id": self.agent_id,
            "input_prompt": self.input_prompt,
            "session_id": self.session_id,
            "stream": self.stream,
            "max_turns": self.max_turns,
            "temperature": self.temperature,
            "context": self.context,
            "tool_allowlist": self.tool_allowlist,
            "metadata": self.metadata,
        }


@dataclass
class AaaSRunResponse:
    """Complete execution record returned by the agent service."""

    run_id: str
    agent_id: str
    session_id: str
    status: str  # "completed", "failed", "cancelled", "running"
    final_output: str | None = None
    steps: list[AaaSTaskStep] = field(default_factory=list)
    usage: dict[str, int] = field(default_factory=dict)
    error: str | None = None
    created_at: str = field(default_factory=lambda: datetime.utcnow().isoformat())

    def to_dict(self) -> dict[str, Any]:
        return {
            "run_id": self.run_id,
            "agent_id": self.agent_id,
            "session_id": self.session_id,
            "status": self.status,
            "final_output": self.final_output,
            "steps": [s.to_dict() for s in self.steps],
            "usage": self.usage,
            "error": self.error,
            "created_at": self.created_at,
        }
