from __future__ import annotations

import json
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from ai_gateway_core.enums import ContentType, InvocationMode, ServiceType, TransportType
from langchain_core.language_models import BaseChatModel
from langchain_core.messages import AIMessageChunk
from langchain_core.outputs import ChatGenerationChunk
from langchain_core.runnables import RunnableLambda

from src.adapters.base import _owned_stream
from src.adapters.langgraph import LangGraphAdapter
from src.adapters.langgraph_local import LocalToolStream
from src.api.schemas.response import StreamChunkSchema
from src.api.v1.sessions import get_history
from src.core.auth.user_resolver import UserContext
from src.core.gateway.dispatcher import GatewayDispatcher
from src.models.request import ContentItem, UnifiedRequest
from src.models.service import ServiceDefinition
from src.services.session.session_manager import SessionManager
from tests.adapters import test_langgraph_local_tools as tool_fixture

NON_TEXT = [
    {"type": "reasoning", "reasoning": "PRIVATE_REASONING_FIXTURE"},
    {"type": "image", "url": "https://invalid.example/IMAGE_PAYLOAD_FIXTURE"},
    {"type": "non_standard", "value": {"text": "NESTED_NON_TEXT_FIXTURE"}},
]


class BlockModel(BaseChatModel):
    contents: list[Any]
    fail_after: int | None = None

    @property
    def _llm_type(self):
        return "controlled-text-blocks"

    def _generate(self, *_args, **_kwargs):
        raise AssertionError("official streaming callback path required")

    async def _astream(self, messages, **kwargs):
        del messages, kwargs
        for index, content in enumerate(self.contents):
            if index == self.fail_after:
                raise RuntimeError("controlled model failure")
            yield ChatGenerationChunk(message=AIMessageChunk(content=content))


async def text_roundtrip(contents, *, as_dict=False, fail_after=None, stop_after=None):
    service = ServiceDefinition(
        service_id="local-blocks", name="Local blocks", service_type=ServiceType.CUSTOM,
        connector_type=TransportType.IN_PROCESS, supported_modes=[InvocationMode.STREAM],
        accepted_content_types=[ContentType.TEXT], output_content_types=[ContentType.TEXT],
        session_enabled=True,
    )
    observed, closed = [], []

    async def messages(state):
        assert state == {"messages": [{"role": "user", "content": "Question"}]}
        return state["messages"]

    graph = RunnableLambda(messages) | BlockModel(contents=contents, fail_after=fail_after)

    async def events(state, config, *, version):
        assert version == "v2"
        assert config["configurable"]["user_id"] == "user"
        assert config["configurable"]["tenant_id"] == "tenant"
        try:
            async with _owned_stream(graph.astream_events(state, config, version=version)) as stream:
                async for event in stream:
                    if event["event"] == "on_chat_model_stream":
                        observed.append(event["data"]["chunk"])
                    if as_dict:
                        data = dict(event.get("data") or {})
                        for name in ("chunk", "output"):
                            if hasattr(data.get(name), "model_dump"):
                                data[name] = data[name].model_dump()
                        event = {**event, "data": data}
                    yield event
        finally:
            closed.append(True)

    adapter = object.__new__(LangGraphAdapter)
    adapter.remote = False
    adapter.graph = SimpleNamespace(astream_events=events)
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
        request_id="block-request", service_id=service.service_id,
        inputs=[ContentItem(ContentType.TEXT, "Question")], user_id="user", tenant_id="tenant",
    )
    chunks, failure = [], None
    with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
        stream = dispatcher.stream(request, ["user"])
        try:
            async for chunk in stream:
                chunks.append(json.loads(StreamChunkSchema.from_domain(chunk).model_dump_json()))
                text_chunks = [c for c in chunks if c["event_type"] == "text_delta" and c["content"]["data"]]
                if stop_after and len(text_chunks) == stop_after:
                    break
        except RuntimeError as exc:
            failure = exc
        finally:
            await stream.aclose()
    history = await get_history(
        request.session_id, limit=50, session_manager=sessions,
        user=UserContext(user_id="user", tenant_id="tenant"),
    )
    assert closed == [True]
    assert observed and observed[0].id
    assert len({chunk.id for chunk in observed}) == 1
    return SimpleNamespace(chunks=chunks, history=history, failure=failure, observed=observed)


def assert_text(result, expected, *, status="completed", final=True):
    text = "".join(c["content"]["data"] for c in result.chunks if c["event_type"] == "text_delta")
    assert text == expected
    assistants = [message for message in result.history if message["role"] == "assistant"]
    assert len(assistants) == 1 and assistants[0]["content"] == expected
    assert assistants[0]["metadata"]["gateway_stream_status"] == status
    assert sum(c["is_final"] for c in result.chunks) == int(final)
    if final:
        assert result.chunks[-1]["event_type"] == "stream_end"
    rendered = repr(result.chunks) + repr(result.history)
    for marker in ("PRIVATE_REASONING_FIXTURE", "IMAGE_PAYLOAD_FIXTURE", "NESTED_NON_TEXT_FIXTURE"):
        assert marker not in rendered


@pytest.mark.asyncio
@pytest.mark.parametrize("as_dict", [False, True], ids=["object", "dict"])
@pytest.mark.parametrize(("contents", "expected"), [
    ([[{"type": "text", "text": "Hello "}], [{"type": "text", "text": "Hello "}],
      [{"type": "text", "text": "café 🚀 e\u0301"}]], "Hello Hello café 🚀 e\u0301"),
    (["first ", ["raw ", {"type": "text", "text": "文字"}], "!"], "first raw 文字!"),
    ([NON_TEXT, [{"type": "text", "text": "Visible "}, *NON_TEXT],
      [*NON_TEXT, {"type": "text", "text": "only"}]], "Visible only"),
], ids=["repeated_unicode", "string_list_order", "non_text_excluded"])
async def test_official_text_blocks_survive_stream_and_history(as_dict, contents, expected):
    result = await text_roundtrip(contents, as_dict=as_dict)
    assert result.failure is None
    assert_text(result, expected)
    assert any(isinstance(chunk.content, list) for chunk in result.observed)


@pytest.mark.asyncio
@pytest.mark.parametrize("as_dict", [False, True], ids=["object", "dict"])
async def test_nontext_only_blocks_do_not_invent_assistant_prose(as_dict):
    result = await text_roundtrip([NON_TEXT, []], as_dict=as_dict)
    assert result.failure is None
    assert not any(c["content"]["data"] for c in result.chunks)
    assert_text(result, "")
    assert sum(c["is_final"] for c in result.chunks) == 1
    assert not any(c.get("tool_call") for c in result.chunks)


@pytest.mark.asyncio
@pytest.mark.parametrize("as_dict", [False, True], ids=["object", "dict"])
async def test_block_text_before_model_failure_is_retained(as_dict):
    result = await text_roundtrip([
        [{"type": "text", "text": "Partial"}], [{"type": "text", "text": "unreached"}],
    ], as_dict=as_dict, fail_after=1)
    assert isinstance(result.failure, RuntimeError)
    assert_text(result, "Partial", status="failed", final=False)


@pytest.mark.asyncio
@pytest.mark.parametrize("as_dict", [False, True], ids=["object", "dict"])
async def test_early_close_retains_only_observed_block_text(as_dict):
    result = await text_roundtrip([
        [{"type": "text", "text": "Observed"}], [{"type": "text", "text": "later"}],
    ], as_dict=as_dict, stop_after=1)
    assert result.failure is None
    assert_text(result, "Observed", status="interrupted", final=False)


@pytest.mark.asyncio
@pytest.mark.parametrize("as_dict", [False, True], ids=["object", "dict"])
@pytest.mark.parametrize("mode", ["success", "handled_error"])
async def test_official_mixed_tool_blocks_preserve_text_and_correlation(monkeypatch, as_dict, mode):
    original = tool_fixture.fragments

    def block_fragments(round_number, count=2):
        return [
            {**part, "content": [{"type": "text", "text": part["content"]}, *NON_TEXT]}
            for part in original(round_number, count)
        ]

    monkeypatch.setattr(tool_fixture, "fragments", block_fragments)
    result = await tool_fixture.roundtrip(as_dict=as_dict, mode=mode)
    tool_fixture.assert_tools(result, mode=mode)
    assert result.failure is None and result.model_calls == 1
    assert result.finished == ["call-0-b", "call-0-a"]
    assert_text(result, "Checking now. Done.")
    assert sum(c["event_type"] == "tool_call_delta" for c in result.chunks) == 6


@pytest.mark.parametrize("kind", ["on_chat_model_end", "on_chain_stream", "on_chain_end"])
def test_block_aggregate_snapshots_do_not_replay_text(kind):
    chunk = AIMessageChunk(content=[{"type": "text", "text": "full aggregate"}])
    state = LocalToolStream(AIMessageChunk)
    adapter = object.__new__(LangGraphAdapter)
    projected = state.project({
        "event": kind, "run_id": "model-run", "data": {"chunk": chunk, "output": chunk},
    })
    assert all(adapter._extract_stream_event(event, set()) is None for event in projected)
