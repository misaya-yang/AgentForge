from typing import Any

import pytest
from ai_gateway_core.models import get_builtin_model_capabilities

from src.services.agent_runtime.model_plane import (
    AgentModelPlaneError,
    _native_responses_body,
)


def _profile() -> dict[str, Any]:
    profile = get_builtin_model_capabilities("dashscope", "qwen3.7-plus")
    assert profile is not None
    return profile


def _tools(enabled: bool) -> list[dict[str, Any]]:
    if not enabled:
        return []
    return [
        {
            "type": "function",
            "name": "read",
            "description": "Read one value.",
            "parameters": {"type": "object", "properties": {}},
        }
    ]


def _history(output: str) -> list[dict[str, str]]:
    return [
        {
            "type": "function_call",
            "call_id": "call-unknown",
            "name": "execute_bash",
            "arguments": '{"command":"pwd"}',
        },
        {
            "type": "function_call_output",
            "call_id": "call-unknown",
            "output": output,
        },
    ]


@pytest.mark.parametrize("tools_enabled", [True, False])
def test_runtime_rejection_of_unadvertised_web_tool_is_replayed(tools_enabled: bool) -> None:
    body, _aliases = _native_responses_body(
        {"input": _history("unsupported call: execute_bash"), "tools": _tools(tools_enabled)},
        model_id="qwen3.7-plus",
        max_output_tokens=256,
        profile=_profile(),
        reasoning_option="minimal",
    )

    assert body["input"][-1]["output"] == "unsupported call: execute_bash"
    if not tools_enabled:
        assert all(tool.get("type") != "function" for tool in body.get("tools", []))


@pytest.mark.parametrize(
    ("tools_enabled", "error_code"),
    [(True, "TRANSCRIPT"), (False, "TOOLS_NOT_ENABLED")],
)
def test_fake_success_for_unadvertised_web_tool_fails_closed(
    tools_enabled: bool,
    error_code: str,
) -> None:
    with pytest.raises(AgentModelPlaneError, match=error_code):
        _native_responses_body(
            {"input": _history("command completed"), "tools": _tools(tools_enabled)},
            model_id="qwen3.7-plus",
            max_output_tokens=256,
            profile=_profile(),
            reasoning_option="minimal",
        )
