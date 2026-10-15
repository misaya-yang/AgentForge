from __future__ import annotations

import json
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from ai_gateway_core.enums import ContentType, InvocationMode, ServiceType, TransportType
from langchain_core.language_models import BaseChatModel
from langchain_core.messages import AIMessage, HumanMessage, ToolMessage
from langchain_core.outputs import ChatGeneration, ChatResult
from langchain_core.runnables import RunnableLambda

from src.adapters.langgraph import LangGraphAdapter
from src.api.schemas.response import UnifiedResponseSchema
from src.api.v1.sessions import get_history
from src.core.auth.user_resolver import UserContext
from src.core.gateway.dispatcher import GatewayDispatcher
from src.models.request import ContentItem, UnifiedRequest
from src.models.service import ServiceDefinition
from src.services.session.session_manager import SessionManager

NON_TEXT = [
    {"type": "reasoning", "reasoning": "PRIVATE_REASONING_FIXTURE"},
    {"type": "image", "url": "https://invalid.example/IMAGE_PAYLOAD_FIXTURE"},
]


class ReplyModel(BaseChatModel):
    reply_content: Any
    calls: int = 0

    @property
    def _llm_type(self):
        return "controlled-local-invoke"

    def _generate(self, *_args, **_kwargs):
        raise AssertionError("official asynchronous invocation required")

    async def _agenerate(self, messages, **kwargs):
        del kwargs
        assert messages[-1].content == "Question"
        self.calls += 1
        return ChatResult(generations=[ChatGeneration(message=AIMessage(content=self.reply_content))])


async def invoke_roundtrip(content, *, shape="direct", as_dict=False, legacy=False):
    model = ReplyModel(reply_content=content)
    observed = []

    async def execute(state, config):
        assert state == {"messages": [{"role": "user", "content": "Question"}]}
        assert config["configurable"]["user_id"] == "user"
        assert config["configurable"]["tenant_id"] == "tenant"
        if legacy:
            return content
        reply = await model.ainvoke(state["messages"], config)
        observed.append(reply)
        if shape == "direct":
            return reply.model_dump() if as_dict else reply
        messages = [
            HumanMessage(content="Question"), AIMessage(content="OLDER_REPLY_FIXTURE"), reply,
            ToolMessage(content="TRAILING_TOOL_FIXTURE", tool_call_id="prior-tool"),
            HumanMessage(content="TRAILING_HUMAN_FIXTURE"),
        ]
        if as_dict:
            messages = [message.model_dump() for message in messages]
        envelope = {"messages": messages}
        return envelope if shape == "messages" else {shape: envelope}

    service = ServiceDefinition(
        service_id="local-invoke", name="Local invoke", service_type=ServiceType.CUSTOM,
        connector_type=TransportType.IN_PROCESS, supported_modes=[InvocationMode.SYNC],
        accepted_content_types=[ContentType.TEXT], output_content_types=[ContentType.TEXT],
        session_enabled=True, max_retries=1, circuit_breaker_enabled=False,
    )
    adapter = object.__new__(LangGraphAdapter)
    adapter.remote = False
    adapter.graph = RunnableLambda(execute)
    adapter.service = service
    adapter.config = {}
    adapter.assistant_id = "fixture-agent"
    registry = MagicMock()
    registry.get = AsyncMock(return_value=service)
    registry.get_adapter.return_value = adapter
    validator = MagicMock()
    validator.validate = AsyncMock()
    limiter = MagicMock()
    limiter.enforce = AsyncMock()
    sessions = SessionManager()
    dispatcher = GatewayDispatcher(registry, validator, limiter, MagicMock(), MagicMock(), sessions)
    request = UnifiedRequest(
        request_id="invoke-request", service_id=service.service_id,
        inputs=[ContentItem(ContentType.TEXT, "Question")], user_id="user", tenant_id="tenant",
    )
    with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
        response = await dispatcher.invoke(request, ["user"])
    wire = json.loads(UnifiedResponseSchema.from_domain(response).model_dump_json())
    history = await get_history(
        request.session_id, limit=50, session_manager=sessions,
        user=UserContext(user_id="user", tenant_id="tenant"),
    )
    assert model.calls == int(not legacy)
    return SimpleNamespace(wire=wire, history=history, observed=observed)


@pytest.mark.asyncio
@pytest.mark.parametrize("shape", ["direct", "messages", "values", "output"])
@pytest.mark.parametrize("as_dict", [False, True], ids=["object", "dict"])
@pytest.mark.parametrize(("content", "expected"), [
    ("Plain reply", "Plain reply"),
    (["Raw ", *NON_TEXT, {"type": "text", "text": "café 🚀"}], "Raw café 🚀"),
    (NON_TEXT, ""),
], ids=["plain", "text_blocks", "nontext"])
async def test_official_local_invoke_text_projection(shape, as_dict, content, expected):
    result = await invoke_roundtrip(content, shape=shape, as_dict=as_dict)
    assert result.wire["status"] == "success"
    assert result.wire["outputs"][0]["data"] == expected
    assistants = [message for message in result.history if message["role"] == "assistant"]
    assert [message["content"] for message in assistants] == [expected]
    assert [message["content"] for message in result.history if message["role"] == "user"] == ["Question"]
    assert len(result.observed) == 1 and isinstance(result.observed[0], AIMessage)
    rendered = repr(result.wire) + repr(result.history)
    for marker in (
        "PRIVATE_REASONING_FIXTURE", "IMAGE_PAYLOAD_FIXTURE", "OLDER_REPLY_FIXTURE",
        "TRAILING_TOOL_FIXTURE", "TRAILING_HUMAN_FIXTURE",
    ):
        assert marker not in rendered


@pytest.mark.asyncio
@pytest.mark.parametrize("as_dict", [False, True], ids=["object", "dict"])
async def test_empty_latest_assistant_does_not_fall_back(as_dict):
    result = await invoke_roundtrip("", shape="messages", as_dict=as_dict)
    assert result.wire["outputs"][0]["data"] == ""
    assistants = [message for message in result.history if message["role"] == "assistant"]
    assert len(assistants) == 1 and assistants[0]["content"] == ""
    assert assistants[0]["metadata"]["gateway_stream_status"] == "completed"


@pytest.mark.asyncio
@pytest.mark.parametrize("legacy_result", [
    "legacy text", {"answer": "ordinary dictionary"}, ["ordinary list"], 12,
    {"messages": [{"role": "user", "content": "unchanged legacy envelope"}]},
])
async def test_nonmessage_legacy_invoke_results_keep_prior_projection(legacy_result):
    result = await invoke_roundtrip(legacy_result, legacy=True)
    expected = str(legacy_result)
    assert result.wire["outputs"][0]["data"] == expected
    assert next(message for message in result.history if message["role"] == "assistant")["content"] == expected
    assert not result.observed


@pytest.mark.asyncio
@pytest.mark.parametrize("content", ["remote string", [{"type": "text", "text": "remote block"}]])
async def test_remote_invoke_retains_original_extraction(content):
    adapter = object.__new__(LangGraphAdapter)
    adapter.remote = True
    adapter._remote_wait = AsyncMock(return_value={"messages": [{"type": "ai", "content": content}]})
    request = UnifiedRequest(
        request_id="remote-control", service_id="remote",
        inputs=[ContentItem(ContentType.TEXT, "Question")], user_id="user", tenant_id="tenant",
    )
    with patch("src.adapters.langgraph_local.extract_local_result_text", side_effect=AssertionError("local helper used remotely")):
        response = await adapter.invoke(request)
    assert response.outputs[0].data == str(content)
    adapter._remote_wait.assert_awaited_once()


@pytest.mark.parametrize("missing", ["langchain_core", "nested_sdk_dependency"])
def test_local_result_optional_sdk_boundary(monkeypatch, missing):
    import builtins

    from src.adapters.langgraph_local import extract_local_result_text

    original = builtins.__import__

    def guarded(name, *args, **kwargs):
        if name == "langchain_core.messages":
            raise ModuleNotFoundError("controlled missing package", name=missing)
        return original(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", guarded)
    if missing == "langchain_core":
        assert extract_local_result_text({"messages": [{"type": "ai", "content": "legacy"}]}) is None
    else:
        with pytest.raises(ModuleNotFoundError, match="controlled missing package"):
            extract_local_result_text("legacy")
