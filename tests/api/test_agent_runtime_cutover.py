from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_contracts.agent_launch import ResolvedAgentLaunchV1
from ai_gateway_contracts.agent_runtime import runtime_sha256
from fastapi import HTTPException

from src.api.v1.agent_runtime import _start_runtime_stream
from src.core.auth.user_resolver import UserContext


class _Control:
    def __init__(self) -> None:
        self.start: dict[str, Any] | None = None

    async def start_turn(self, **kwargs: Any) -> Any:
        self.start = kwargs
        return SimpleNamespace(run_id="run-1")

    async def stream_events(self, **_kwargs: Any):
        yield b'data: {"event_type":"run_finished","data":{"status":"succeeded"}}\n\n'


def _snapshot() -> dict[str, Any]:
    return {
        "schema_version": "agent-runtime/v1",
        "tenant_id": "tenant-a",
        "agent_id": "agent-a",
        "agent_version_id": "version-a",
        "publication": {
            "id": None,
            "channel": "preview",
            "auth_mode": "private",
        },
        "model": {
            "id": "qwen3.7-plus",
            "provider": "dashscope",
            "parameters": {"temperature": 0.2, "max_tokens": 1024, "thinking_mode": "fast"},
        },
        "agent_spec": {
            "agentId": "agent-a",
            "agentVersionId": "version-a",
            "channel": "preview",
            "developerInstructions": "Answer with evidence.",
            "model": {
                "id": "qwen3.7-plus",
                "provider": "dashscope",
                "parameters": {
                    "temperature": 0.2,
                    "max_tokens": 1024,
                    "thinking_mode": "fast",
                },
            },
            "knowledge": {
                "datasets": ["dataset-a"],
                "retrieval": {"mode": "tool", "top_k": 3, "threshold": 0.5},
            },
            "capabilities": [],
            "memory": {"mode": "session"},
        },
        "capabilities": [],
        "knowledge": {"datasets": ["dataset-a"], "retrieval": {"mode": "tool", "top_k": 3, "threshold": 0.5}},
        "memory": {"mode": "session"},
        "channel_policy": {
            "attachments": True,
            "high_risk_tools": True,
            "allowed_origins": [],
        },
        "fingerprints": {
            "spec": runtime_sha256("spec"),
            "tool_schema": runtime_sha256([]),
            "skills": runtime_sha256([]),
            "knowledge_revision": runtime_sha256("knowledge"),
        },
    }


@pytest.mark.asyncio
async def test_agent_studio_stream_uses_control_plane_and_signed_snapshot() -> None:
    control = _Control()
    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(agent_runtime_control=control)),
        state=SimpleNamespace(),
    )
    user = UserContext(
        user_id="user-a", tenant_id="tenant-a", tier="normal", is_authenticated=True
    )
    response = await _start_runtime_stream(
        request,
        user,
        body={"message": "hello", "session_id": "session-a", "attachments": []},
        snapshot=_snapshot(),
    )
    assert response.headers["x-ai-agent-kernel"] == "agent_runtime"
    assert control.start is not None
    launch = control.start["resolved_agent_launch"]
    assert isinstance(launch, ResolvedAgentLaunchV1)
    assert launch.to_control_snapshot()["agent_spec"]["developerInstructions"] == (
        "Answer with evidence."
    )
    assert launch.runtime_inputs["readonly_capabilities"]["knowledge"][
        "dataset_ids"
    ] == ["dataset-a"]
    assert "resolved_agent_snapshot" not in control.start


@pytest.mark.asyncio
async def test_agent_studio_resume_does_not_fall_back_to_python_loop() -> None:
    control = _Control()
    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(agent_runtime_control=control)),
        state=SimpleNamespace(),
    )
    user = UserContext(
        user_id="user-a", tenant_id="tenant-a", tier="normal", is_authenticated=True
    )
    with pytest.raises(HTTPException) as raised:
        await _start_runtime_stream(
            request,
            user,
            body={
                "message": "resume",
                "session_id": "session-a",
                "attachments": [],
                "resume_run_id": "run-a",
                "resume_approval_id": "approval-a",
            },
            snapshot=_snapshot(),
        )
    assert raised.value.status_code == 409
    assert raised.value.detail["code"] == "AGENT_RUNTIME_RESUME_NOT_AVAILABLE"
    assert control.start is None
