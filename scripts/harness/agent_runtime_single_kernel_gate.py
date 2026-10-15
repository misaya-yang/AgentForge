#!/usr/bin/env python3
"""Fail-closed source contract for the platform's single Agent kernel."""

from __future__ import annotations

import ast
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def _calls_in_function(source: str, function_name: str) -> set[str]:
    """Return direct call targets from one model-producing function."""

    tree = ast.parse(source)
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        if node.name != function_name:
            continue
        calls: set[str] = set()
        for child in ast.walk(node):
            if not isinstance(child, ast.Call):
                continue
            target = child.func
            if isinstance(target, ast.Name):
                calls.add(target.id)
            elif isinstance(target, ast.Attribute):
                calls.add(target.attr)
        return calls
    return set()


def main() -> int:
    assignment = (ROOT / "src/services/assistant_runtime_assignment.py").read_text()
    gateway_v1 = (ROOT / "src/api/v1/assistant.py").read_text()
    responses_v1 = (ROOT / "src/api/v1/responses.py").read_text()
    agent_runtime_v1 = (ROOT / "src/api/v1/agent_runtime.py").read_text()
    agent_public_v1 = (ROOT / "src/api/v1/agent_public.py").read_text()
    gateway_v2 = (ROOT / "src/api/v2/agent.py").read_text()
    compose = (ROOT / "docker-compose.yml").read_text()

    assert 'RuntimeOwner = Literal["agent_runtime"]' in assignment
    assert "CANARY_PERCENT" not in assignment
    assert "python_control" not in assignment
    assert "codex_candidate" not in assignment
    assert not any(
        "proxy" in call and "assistant" in call
        for call in _calls_in_function(gateway_v1, "_start_agent_runtime_turn")
    )
    assert "_start_agent_runtime_turn" in gateway_v1
    # Public model-producing functions must all enter the V2 Runtime.  Check
    # call targets in those functions rather than banning harmless management
    assert not any(
        "proxy" in call and "assistant" in call
        for call in _calls_in_function(responses_v1, "create_response")
    )
    assert not any(
        "proxy" in call and "assistant" in call
        for call in _calls_in_function(agent_runtime_v1, "_start_runtime_stream")
    )
    for function_name in (
        "preview_chat_stream",
        "version_preview_chat_stream",
        "published_chat_stream",
    ):
        assert "_start_runtime_stream" in _calls_in_function(
            agent_runtime_v1, function_name
        ), f"{function_name} must enter the single Agent Runtime"
    assert "_start_runtime_stream" in _calls_in_function(
        agent_public_v1, "public_agent_chat_stream"
    )
    assert "agent_runtime_control" in gateway_v2
    assert "\n  agent-runtime:\n" in compose
    runtime_block = compose.split("\n  agent-runtime:\n", 1)[1].split("\nnetworks:", 1)[0]
    assert "profiles:" not in runtime_block
    assert "container_name: ai-gateway-agent-runtime" in runtime_block
    assert "python_control" not in compose
    assert "codex_candidate" not in compose

    print(
        json.dumps(
            {
                "single_kernel": "agent_runtime",
                "v1_projection": "passed",
                "v2_native": "passed",
                "python_loop_route": "absent",
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
