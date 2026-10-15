from __future__ import annotations

import json
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from ai_gateway_core.enums import (
    ContentType,
    InvocationMode,
    ServiceType,
    StreamEventType,
    TransportType,
)
from langchain_core.language_models.fake_chat_models import (
    FakeListChatModel,
    FakeListChatModelError,
)
from langchain_core.messages import AIMessageChunk
from langchain_core.runnables import RunnableLambda

from src.adapters.base import _owned_stream
from src.adapters.langgraph import LangGraphAdapter
from src.api.schemas.response import StreamChunkSchema
from src.api.v1.sessions import get_history
from src.core.auth.user_resolver import UserContext
from src.core.gateway.dispatcher import GatewayDispatcher
from src.models.request import ContentItem, UnifiedRequest
from src.models.service import ServiceDefinition
from src.services.session.session_manager import SessionManager


async def sdk_roundtrip(answer, *, fail_at=None, stop_after=None):
    service = ServiceDefinition(
        service_id="local-fixture", name="Local fixture", service_type=ServiceType.CUSTOM,
        connector_type=TransportType.IN_PROCESS, supported_modes=[InvocationMode.STREAM],
        accepted_content_types=[ContentType.TEXT], output_content_types=[ContentType.TEXT],
        session_enabled=True,
    )
    observed = []
    closed = []

    async def messages(state):
        assert state == {"messages": [{"role": "user", "content": "Question"}]}
        return state["messages"]

    graph = RunnableLambda(messages) | FakeListChatModel(
        responses=[answer], error_on_chunk_number=fail_at,
    )

    async def events(state, config, *, version):
        assert version == "v2"
        assert config["configurable"]["user_id"] == "fixture-user"
        assert config["configurable"]["tenant_id"] == "fixture-tenant"
        try:
            async with _owned_stream(graph.astream_events(state, config, version=version)) as stream:
                async for event in stream:
                    if event["event"] == "on_chat_model_stream":
                        observed.append(event["data"]["chunk"])
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
        request_id="local-request", service_id=service.service_id,
        inputs=[ContentItem(ContentType.TEXT, "Question")],
        user_id="fixture-user", tenant_id="fixture-tenant",
    )
    chunks = []
    failure = None
    with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
        stream = dispatcher.stream(request, ["user"])
        try:
            async for chunk in stream:
                chunks.append(json.loads(StreamChunkSchema.from_domain(chunk).model_dump_json()))
                if stop_after and sum(c["event_type"] == "text_delta" for c in chunks) == stop_after:
                    break
        except FakeListChatModelError as exc:
            failure = exc
        finally:
            await stream.aclose()
    history = await get_history(
        request.session_id, limit=50, session_manager=sessions,
        user=UserContext(user_id="fixture-user", tenant_id="fixture-tenant"),
    )
    assert closed == [True]
    assert observed and len({chunk.id for chunk in observed}) == 1
    assert observed[0].id
    return chunks, history, failure


@pytest.mark.asyncio
@pytest.mark.parametrize("answer", ["Hello hello", "查查 café 🚀 e\u0301"])
async def test_official_local_sdk_deltas_survive_stream_and_history(answer):
    chunks, history, failure = await sdk_roundtrip(answer)
    assert failure is None
    assert "".join(c["content"]["data"] for c in chunks if c["event_type"] == "text_delta") == answer
    assistants = [message for message in history if message["role"] == "assistant"]
    assert len(assistants) == 1 and assistants[0]["content"] == answer
    assert assistants[0]["metadata"]["gateway_stream_status"] == "completed"
    assert sum(c["is_final"] for c in chunks) == 1
    assert chunks[-1]["event_type"] == "stream_end"


@pytest.mark.asyncio
async def test_official_local_sdk_failure_preserves_all_observed_text_without_success():
    chunks, history, failure = await sdk_roundtrip("abcd", fail_at=2)
    assert isinstance(failure, FakeListChatModelError)
    assert "".join(c["content"]["data"] for c in chunks if c["event_type"] == "text_delta") == "ab"
    assistant = next(message for message in history if message["role"] == "assistant")
    assert assistant["content"] == "ab"
    assert assistant["metadata"]["gateway_stream_status"] == "failed"
    assert not any(c["is_final"] for c in chunks)


@pytest.mark.asyncio
async def test_official_local_sdk_early_close_keeps_interrupted_history():
    chunks, history, failure = await sdk_roundtrip("abcd", stop_after=1)
    assert failure is None
    assert "".join(c["content"]["data"] for c in chunks) == "a"
    assistant = next(message for message in history if message["role"] == "assistant")
    assert assistant["content"] == "a"
    assert assistant["metadata"]["gateway_stream_status"] == "interrupted"
    assert not any(c["is_final"] for c in chunks)


@pytest.mark.parametrize("as_dict", [False, True], ids=["object", "dict"])
@pytest.mark.parametrize("parts", [["", "a", "a", "b"], ["a", "a", "b"]], ids=["empty_first", "repeated"])
def test_parent_message_id_does_not_deduplicate_local_text(parts, as_dict):
    adapter = object.__new__(LangGraphAdapter)
    seen = set()
    projected = []
    for text in parts:
        chunk = AIMessageChunk(id="shared-message", content=text)
        event = adapter._extract_stream_event({
            "event": "on_chat_model_stream",
            "data": {"chunk": chunk.model_dump() if as_dict else chunk},
        }, seen)
        if event:
            projected.append(event.text)
    assert projected == [part for part in parts if part]


@pytest.mark.parametrize("as_dict", [False, True], ids=["object", "dict"])
def test_parent_message_id_does_not_drop_later_local_tool_delta(as_dict):
    adapter = object.__new__(LangGraphAdapter)
    seen = set()
    projected = []
    for argument in ['{"query":', '"weather"}']:
        chunk = AIMessageChunk(id="shared-message", content="", tool_call_chunks=[{
            "name": "lookup", "id": "call-1", "index": 0, "args": argument,
        }])
        event = adapter._extract_stream_event({
            "event": "on_chat_model_stream",
            "data": {"chunk": chunk.model_dump() if as_dict else chunk},
        }, seen)
        if event:
            projected.append(event.tool_call.arguments)
    assert projected == ['{"query":', '"weather"}']


def test_local_tool_lifecycle_still_deduplicates_run_events():
    adapter = object.__new__(LangGraphAdapter)
    seen = set()
    for name, data, expected in [
        ("on_tool_start", {"input": {"query": "weather"}}, StreamEventType.TOOL_CALL_START),
        ("on_tool_end", {"output": "sunny"}, StreamEventType.TOOL_RESULT),
    ]:
        event = {"event": name, "run_id": "tool-run", "name": "lookup", "data": data}
        assert adapter._extract_stream_event(event, seen).event_type == expected
        assert adapter._extract_stream_event(event, seen) is None


@pytest.mark.parametrize("event_type", ["on_chat_model_end", "on_chain_stream", "on_chain_end"])
def test_full_model_or_chain_snapshots_do_not_repeat_local_text(event_type):
    adapter = object.__new__(LangGraphAdapter)
    chunk = AIMessageChunk(id="shared-message", content="full answer")
    assert adapter._extract_stream_event({
        "event": event_type, "data": {"chunk": chunk, "output": chunk},
    }, set()) is None
