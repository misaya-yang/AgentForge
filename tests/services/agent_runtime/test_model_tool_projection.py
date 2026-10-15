from __future__ import annotations

import copy

from ai_gateway_core.models import get_builtin_model_capabilities

from src.services.agent_runtime.model_plane import (
    _chat_tools_from_runtime,
    _namespace_alias,
    _native_responses_body,
)


def _profile() -> dict:
    profile = get_builtin_model_capabilities("dashscope", "qwen3.7-plus")
    assert profile is not None
    return profile


def _native_search_profile() -> dict:
    profile = copy.deepcopy(_profile())
    profile["native_search"] = {
        "adapter_id": "search/dashscope-native-v1",
        "enabled": True,
        "config": {},
    }
    profile["tools"]["web_search_wire"] = "native"
    return profile


def test_chat_completions_flattens_namespaces_and_omits_native_search() -> None:
    tools = _chat_tools_from_runtime(
        [
            {
                "type": "function",
                "name": "lookup",
                "description": "Look up one value.",
                "parameters": {"type": "object", "properties": {}},
            },
            {"type": "web_search"},
            {
                "type": "namespace",
                "name": "mcp",
                "tools": [
                    {
                        "type": "function",
                        "name": "write",
                        "description": "Write through MCP.",
                        "parameters": {"type": "object", "properties": {}},
                    }
                ],
            },
        ],
        _native_search_profile(),
        allowed_tool_names={"lookup", "write"},
    )

    assert [tool["function"]["name"] for tool in tools] == ["lookup", _namespace_alias("mcp", "write")]


def test_native_tool_choice_none_omits_runtime_tools_on_first_call() -> None:
    body, _aliases = _native_responses_body(
        {
            "input": [{"role": "user", "content": "hello"}],
            "tools": [
                {
                    "type": "function",
                    "name": "lookup",
                    "description": "Look up one value.",
                    "parameters": {"type": "object", "properties": {}},
                }
            ],
        },
        model_id="qwen3.7-plus",
        max_output_tokens=128,
        profile=_native_search_profile(),
        reasoning_option="minimal",
        tool_choice="none",
    )

    assert "tools" not in body
    assert "tool_choice" not in body


def test_chat_namespace_history_roundtrip_preserves_parallel_calls() -> None:
    from src.services.agent_runtime.model_plane import (
        _native_tool_transcript,
        _responses_input_to_messages,
        _validated_native_tools,
    )

    namespaced = [{
        "type": "namespace", "name": namespace,
        "tools": [{"type": "function", "name": "read", "description": "Read.",
                   "parameters": {"type": "object"}}],
    } for namespace in ("docs", "mail")]
    tools = _chat_tools_from_runtime(namespaced, _profile(), allowed_tool_names={"read"})
    aliases = [tool["function"]["name"] for tool in tools]
    assert aliases == [_namespace_alias("docs", "read"), _namespace_alias("mail", "read")]
    validated = _validated_native_tools(namespaced, _profile())
    history = [
        {"type": "function_call", "name": "read", "namespace": namespace,
         "call_id": namespace, "arguments": "{}"} for namespace in ("docs", "mail")
    ] + [
        {"type": "function_call_output", "call_id": namespace, "output": namespace}
        for namespace in ("docs", "mail")
    ]
    wire = _native_tool_transcript(history, aliases=validated.aliases, wire_aliases=validated.wire_aliases)
    messages = _responses_input_to_messages({"input": wire}, allowed_tool_names=set(aliases))
    assert len(messages) == 3
    assert [call["function"]["name"] for call in messages[0]["tool_calls"]] == aliases
    assert [message["tool_call_id"] for message in messages[1:]] == ["docs", "mail"]
    assert history[0]["namespace"] == "docs"


def test_chat_tool_projector_restores_namespace_and_rejects_undeclared_tools() -> None:
    import json

    import pytest

    from src.services.agent_runtime.model_plane import AgentModelPlaneError, _ResponsesProjector

    alias = _namespace_alias("docs", "read")
    projector = _ResponsesProjector(model_id="model", estimated_input_tokens=1,
                                    tool_aliases={alias: ("docs", "read")}, allowed_tool_names={alias})
    raw = [{"index": 0, "id": "call-1", "function": {"name": alias, "arguments": "{}"}}]
    chunks = projector.tool_call_delta(raw) + projector.complete()
    items = [json.loads(chunk.decode().split("data: ", 1)[1])["item"] for chunk in chunks
             if b'"type":"response.output_item.' in chunk]
    assert all(item["name"] == "read" and item["namespace"] == "docs" for item in items)
    assert all("_argument_bytes" not in item for item in items)
    denied = _ResponsesProjector(model_id="model", estimated_input_tokens=1, allowed_tool_names=set())
    with pytest.raises(AgentModelPlaneError, match="RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH"):
        denied.tool_call_delta(raw)


def test_chat_tool_argument_limit_counts_utf8_bytes() -> None:
    import pytest

    from src.services.agent_runtime.model_plane import AgentModelPlaneError, _ResponsesProjector

    projector = _ResponsesProjector(model_id="model", estimated_input_tokens=1)
    with pytest.raises(AgentModelPlaneError, match="RUNTIME_PROVIDER_TOOL_ARGUMENTS_LIMIT"):
        projector.tool_call_delta([{"index": 0, "id": "call-1", "function": {
            "name": "lookup", "arguments": "字" * (1024 * 1024 // 3 + 1),
        }}])
    assert projector.tool_calls[0]["arguments"] == ""


def test_native_tool_validator_rejects_wrong_namespace_and_unadvertised_search() -> None:
    import json

    import pytest

    from src.services.agent_runtime.model_plane import (
        AgentModelPlaneError,
        _NativeResponsesStreamValidator,
    )

    alias = _namespace_alias("docs", "read")
    for item in (
        {"type": "function_call", "name": "exec_command"},
        {"type": "function_call", "name": "read", "namespace": "mail"},
        {"type": "function_call", "name": alias, "namespace": "mail"},
        {"type": "web_search_call"},
    ):
        validator = _NativeResponsesStreamValidator(
            {alias: ("docs", "read")}, allow_tools=True, allowed_tool_identities={("docs", "read")},
        )
        validator.consume(json.dumps({"type": "response.created", "sequence_number": 0}))
        with pytest.raises(AgentModelPlaneError, match="RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH"):
            validator.consume(json.dumps({"type": "response.output_item.added", "sequence_number": 1, "item": item}))


def test_chat_split_tool_name_waits_for_provider_identity() -> None:
    import json

    import pytest

    from src.services.agent_runtime.model_plane import AgentModelPlaneError, _ResponsesProjector

    projector = _ResponsesProjector(model_id="model", estimated_input_tokens=1, allowed_tool_names={"get_data"})
    assert projector.tool_call_delta([{"index": 0, "function": {"name": "get"}}]) == []
    assert projector.tool_call_delta([{"index": 0, "function": {"name": "_data", "arguments": "{"}}]) == []
    chunks = projector.tool_call_delta([{"index": 0, "id": "actual_call", "function": {"arguments": "}"}}])
    chunks += projector.complete()
    items = [json.loads(chunk.decode().split("data: ", 1)[1])["item"] for chunk in chunks
             if b'"type":"response.output_item.' in chunk]
    assert [item["call_id"] for item in items] == ["actual_call", "actual_call"]
    assert all(item["name"] == "get_data" for item in items)
    with pytest.raises(AgentModelPlaneError, match="RUNTIME_PROVIDER_STREAM_INVALID"):
        projector.tool_call_delta([{"index": 1, "id": "actual_call", "function": {"name": "get_data"}}])


def test_native_current_tool_subset_keeps_completed_history() -> None:
    from src.services.agent_runtime.model_plane import _native_responses_body

    tools = [{"type": "function", "name": name, "description": name, "parameters": {"type": "object"}} for name in ("lookup", "read")]
    history = [{"type": "function_call", "name": "read", "call_id": "prior", "arguments": "{}"},
               {"type": "function_call_output", "call_id": "prior", "output": "previous read"}]
    for choice, names in (("auto", {"lookup"}), ("none", set())):
        body, _ = _native_responses_body({"input": history, "tools": tools}, model_id="model",
            max_output_tokens=100, profile=_profile(), reasoning_option="minimal", allowed_tool_names=names, tool_choice=choice)
        assert body["input"] == history
        assert [tool["name"] for tool in body.get("tools", [])] == (["lookup"] if choice == "auto" else [])


def test_named_tool_output_must_match_call_namespace() -> None:
    import pytest

    from src.services.agent_runtime.model_plane import (
        AgentModelPlaneError,
        _validate_tool_transcript,
    )

    history = [{"type": "function_call", "name": "read", "namespace": "docs", "call_id": "call", "arguments": "{}"},
               {"type": "function_call_output", "name": "read", "namespace": "mail", "call_id": "call", "output": "content"}]
    with pytest.raises(AgentModelPlaneError, match="RUNTIME_TOOL_TRANSCRIPT_INVALID"):
        _validate_tool_transcript(history)


def test_mcp_multiple_text_outputs_replay_on_both_wires() -> None:
    import pytest

    from src.services.agent_runtime.model_plane import (
        AgentModelPlaneError,
        _native_responses_body,
        _responses_input_to_messages,
    )

    output = [{"type": "input_text", "text": "part one"}, {"type": "input_text", "text": " + part two"}]
    history = [{"type": "function_call", "name": "lookup", "call_id": "call", "arguments": "{}"},
               {"type": "function_call_output", "call_id": "call", "output": output}]
    tools = [{"type": "function", "name": "lookup", "description": "Lookup", "parameters": {"type": "object"}}]
    body, _ = _native_responses_body({"input": history, "tools": tools}, model_id="model",
        max_output_tokens=100, profile=_profile(), reasoning_option="minimal")
    assert body["input"][1]["output"] == output
    messages = _responses_input_to_messages({"input": history}, allowed_tool_names={"lookup"})
    assert messages[-1]["content"] == "part one + part two"
    history[-1]["output"] = [{"type": "input_image", "image_url": "hidden"}]
    with pytest.raises(AgentModelPlaneError, match="RUNTIME_TOOL_TRANSCRIPT_INVALID"):
        _responses_input_to_messages({"input": history}, allowed_tool_names={"lookup"})
