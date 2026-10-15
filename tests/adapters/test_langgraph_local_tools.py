from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace
from typing import Annotated
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from ai_gateway_core.enums import ContentType, InvocationMode, ServiceType, TransportType
from langchain_core.callbacks import AsyncCallbackHandler
from langchain_core.callbacks.manager import AsyncCallbackManager
from langchain_core.language_models import BaseChatModel
from langchain_core.language_models.fake_chat_models import FakeListChatModel
from langchain_core.messages import AIMessageChunk
from langchain_core.outputs import ChatGenerationChunk
from langchain_core.runnables import RunnableLambda
from langchain_core.tools import InjectedToolCallId, StructuredTool, ToolException

from src.adapters.base import _owned_stream
from src.adapters.langgraph import LangGraphAdapter
from src.api.schemas.response import StreamChunkSchema
from src.api.v1.sessions import get_history
from src.core.auth.user_resolver import UserContext
from src.core.gateway.dispatcher import GatewayDispatcher
from src.models.request import ContentItem, UnifiedRequest
from src.models.service import ServiceDefinition
from src.services.session.session_manager import SessionManager

QUERY = "同一个 café 🚀"


def fragments(round_number, count=2):
    ids = [f"call-{round_number}-{letter}" for letter in "ab"[:count]]
    return [
        {"content": "Checking ", "tool_call_chunks": [
            {"name": "lookup", "id": call_id, "index": index, "args": '{"query":"'}
            for index, call_id in enumerate(ids)
        ]},
        {"content": "", "tool_call_chunks": [
            {"name": None, "id": None, "index": index, "args": QUERY}
            for index, _ in reversed(list(enumerate(ids)))
        ]},
        {"content": "now. ", "tool_call_chunks": [
            {"name": None, "id": None, "index": index, "args": '"}'}
            for index, _ in enumerate(ids)
        ]},
    ]


class FragmentModel(BaseChatModel):
    batches: list[list[dict]]
    cursor: int = 0

    @property
    def _llm_type(self):
        return "controlled-tool-fragments"

    def _generate(self, *_args, **_kwargs):
        raise AssertionError("official streaming callback path required")

    async def _astream(self, messages, **kwargs):
        del messages, kwargs
        batch = self.batches[self.cursor]
        self.cursor += 1
        for part in batch:
            yield ChatGenerationChunk(message=AIMessageChunk(**part))


class ExistingObserver(AsyncCallbackHandler):
    run_inline = True

    def __init__(self):
        self.starts = []
        self.ends = []

    async def on_tool_start(self, serialized, input_str, *, run_id, **kwargs):
        del serialized, input_str
        self.starts.append((str(run_id), kwargs.get("tool_call_id")))

    async def on_tool_end(self, output, *, run_id, **_kwargs):
        self.ends.append((str(run_id), output))


async def roundtrip(*, as_dict=False, mode="success", rounds=1, recover=False, manager=False):
    count = 1 if mode == "unhandled" else 2
    model = FragmentModel(batches=[fragments(number, count) for number in range(rounds)])
    final_model = FakeListChatModel(responses=["Done."])
    calls, finished, wire_events, closed = [], [], [], []
    second_finished = {number: asyncio.Event() for number in range(rounds)}
    blocked = asyncio.Event()
    observer = ExistingObserver()
    callbacks = AsyncCallbackManager([observer], [observer]) if manager else [observer]
    original_handlers = list(callbacks.handlers if manager else callbacks)

    async def lookup(query: str, tool_call_id: Annotated[str, InjectedToolCallId]) -> str:
        """Return controlled data without accessing a provider."""
        assert query == QUERY
        calls.append(tool_call_id)
        try:
            if mode == "cancel":
                await blocked.wait()
            elif mode == "unhandled":
                raise RuntimeError("SYNTHETIC_PRIVATE_TOOL_DETAIL")
            elif tool_call_id.endswith("a"):
                await second_finished[int(tool_call_id.split("-")[1])].wait()
            if mode == "handled_error" and tool_call_id.endswith("b"):
                raise ToolException("SYNTHETIC_PRIVATE_TOOL_DETAIL")
            return "" if mode == "empty" else "result:" + tool_call_id
        finally:
            finished.append(tool_call_id)
            if tool_call_id.endswith("b"):
                second_finished[int(tool_call_id.split("-")[1])].set()

    tool = StructuredTool.from_function(
        coroutine=lookup, name="lookup", description="Controlled lookup",
        handle_tool_error=lambda _exc: "",
    )

    async def execute(state, config):
        assert state == {"messages": [{"role": "user", "content": "Question"}]}
        for _ in range(rounds):
            answer = await model.ainvoke(state["messages"], config)
            assert len(answer.tool_calls) == count
            try:
                await asyncio.gather(*(tool.ainvoke(call, config) for call in answer.tool_calls))
            except RuntimeError:
                if not recover:
                    raise
        return await final_model.ainvoke(state["messages"], config)

    graph = RunnableLambda(execute)

    async def events(state, config, *, version):
        assert config["configurable"]["user_id"] == "user"
        assert config["configurable"]["tenant_id"] == "tenant"
        try:
            async with _owned_stream(graph.astream_events(state, config, version=version)) as producer:
                async for event in producer:
                    wire_events.append(event)
                    if as_dict:
                        data = dict(event.get("data") or {})
                        for name in ("chunk", "output"):
                            if hasattr(data.get(name), "model_dump"):
                                data[name] = data[name].model_dump()
                        event = {**event, "data": data}
                    yield event
        finally:
            closed.append(True)

    service = ServiceDefinition(
        service_id="local-tools", name="Local tools", service_type=ServiceType.CUSTOM,
        connector_type=TransportType.IN_PROCESS, supported_modes=[InvocationMode.STREAM],
        accepted_content_types=[ContentType.TEXT], output_content_types=[ContentType.TEXT],
        session_enabled=True,
    )
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
        request_id="tool-request", service_id=service.service_id,
        inputs=[ContentItem(ContentType.TEXT, "Question")], user_id="user", tenant_id="tenant",
        parameters={"config": {"callbacks": callbacks}},
    )
    chunks, failure = [], None
    with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
        stream = dispatcher.stream(request, ["user"])
        try:
            async for chunk in stream:
                chunks.append(json.loads(StreamChunkSchema.from_domain(chunk).model_dump_json()))
                if mode == "cancel" and sum(c["event_type"] == "tool_call_start" for c in chunks) == count:
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
    assert list(callbacks.handlers if manager else callbacks) == original_handlers
    assert len(observer.starts) == count * model.cursor == len(calls) == len(set(calls))
    assert sorted(finished) == sorted(calls)
    # Actual SDK run IDs differ from provider call IDs: correlation cannot be guessed.
    assert all(run_id != call_id for run_id, call_id in observer.starts)
    return SimpleNamespace(
        chunks=chunks, history=history, failure=failure, calls=calls, finished=finished,
        observed=wire_events, observer=observer, model_calls=model.cursor,
    )


def assert_tools(result, *, rounds=1, mode="success", run_status="completed"):
    assistant = next(message for message in result.history if message["role"] == "assistant")
    expected_ids = {f"call-{number}-{letter}" for number in range(rounds) for letter in "ab"}
    tools = assistant["metadata"]["tool_calls"]
    assert len(tools) == len(expected_ids)
    assert {tool["tool_call_id"] for tool in tools} == expected_ids
    assert assistant["metadata"]["gateway_stream_status"] == run_status
    for tool in tools:
        assert tool["name"] == "lookup"
        assert json.loads(tool["arguments"]) == {"query": QUERY}
        failed = mode == "handled_error" and tool["tool_call_id"].endswith("b")
        assert tool["status"] == ("running" if mode == "cancel" else "error" if failed else "completed")
        assert tool["result"] == (
            None if mode == "cancel" else "" if failed or mode == "empty"
            else "result:" + tool["tool_call_id"]
        )
    assert {
        chunk["tool_call"]["tool_call_id"] for chunk in result.chunks if chunk.get("tool_call")
    } == expected_ids
    assert "SYNTHETIC_PRIVATE_TOOL_DETAIL" not in repr(result.chunks)
    return assistant


@pytest.mark.asyncio
@pytest.mark.parametrize("as_dict", [False, True], ids=["object", "dict"])
@pytest.mark.parametrize("mode", ["success", "handled_error"])
async def test_official_parallel_tools_roundtrip(as_dict, mode):
    result = await roundtrip(as_dict=as_dict, mode=mode)
    assistant = assert_tools(result, mode=mode)
    assert result.failure is None and result.model_calls == 1
    assert result.finished == ["call-0-b", "call-0-a"]
    assert assistant["content"] == "Checking now. Done."
    assert sum(chunk["is_final"] for chunk in result.chunks) == 1
    assert result.chunks[-1]["event_type"] == "stream_end"
    deltas = [chunk for chunk in result.chunks if chunk["event_type"] == "tool_call_delta"]
    assert len(deltas) == 6
    for call_id in ("call-0-a", "call-0-b"):
        arguments = [c["tool_call"]["arguments"] for c in deltas if c["tool_call"]["tool_call_id"] == call_id]
        assert arguments == ['{"query":"', '{"query":"' + QUERY, '{"query":"' + QUERY + '"}']


@pytest.mark.asyncio
@pytest.mark.parametrize("as_dict", [False, True], ids=["object", "dict"])
async def test_model_run_scopes_reset_tool_indexes(as_dict):
    result = await roundtrip(as_dict=as_dict, rounds=2)
    assistant = assert_tools(result, rounds=2)
    assert result.model_calls == 2
    assert assistant["content"] == "Checking now. Checking now. Done."


@pytest.mark.asyncio
async def test_empty_official_tool_results_remain_present():
    result = await roundtrip(mode="empty")
    assert_tools(result, mode="empty")


@pytest.mark.asyncio
@pytest.mark.parametrize("recover", [False, True], ids=["propagated", "handled_by_graph"])
async def test_official_tool_error_has_canonical_failed_result(recover):
    result = await roundtrip(mode="unhandled", recover=recover)
    assistant = next(message for message in result.history if message["role"] == "assistant")
    tools = assistant["metadata"]["tool_calls"]
    assert len(tools) == 1 and tools[0]["tool_call_id"] == "call-0-a"
    assert tools[0]["status"] == "error" and tools[0]["result"] == ""
    assert json.loads(tools[0]["arguments"]) == {"query": QUERY}
    assert assistant["metadata"]["gateway_stream_status"] == ("completed" if recover else "failed")
    assert bool(result.failure) is not recover
    assert sum(chunk["is_final"] for chunk in result.chunks) == int(recover)
    assert "SYNTHETIC_PRIVATE_TOOL_DETAIL" not in repr(result.chunks)


@pytest.mark.asyncio
@pytest.mark.parametrize("as_dict", [False, True], ids=["object", "dict"])
async def test_cancel_during_parallel_tools_keeps_canonical_unfinished_history(as_dict):
    result = await roundtrip(as_dict=as_dict, mode="cancel")
    assert_tools(result, mode="cancel", run_status="interrupted")
    assert result.failure is None
    assert not any(chunk["is_final"] for chunk in result.chunks)
    assert not any(chunk["event_type"] == "tool_result" for chunk in result.chunks)


@pytest.mark.asyncio
async def test_existing_callback_manager_is_copied_and_remains_active():
    result = await roundtrip(manager=True)
    assert_tools(result)
    assert len(result.observer.starts) == len(result.observer.ends) == 2


def local_state():
    from src.adapters.langgraph_local import LocalToolStream
    return LocalToolStream(AIMessageChunk)


def model_event(chunks, run_id="model-run", content=""):
    return {"event": "on_chat_model_stream", "run_id": run_id, "data": {"chunk": AIMessageChunk(
        content=content, tool_call_chunks=chunks,
    )}}


def test_tool_chunks_without_indexes_remain_distinct_complete_calls():
    state = local_state()
    projected = state.project(model_event([
        {"id": "a", "name": "lookup", "args": "{}", "index": None},
        {"id": "b", "name": "lookup", "args": "{}", "index": None},
    ]))
    assert [row["data"]["chunk"]["tool_calls"][0]["id"] for row in projected] == ["a", "b"]


def test_no_unchanged_or_terminal_tool_is_reemitted():
    state = local_state()
    state.project(model_event([
        {"id": "a", "name": "lookup", "args": "{}", "index": 0},
        {"id": "b", "name": "lookup", "args": "{", "index": 1},
    ]))
    state.project({"event": "on_tool_end", "run_id": "tool-run", "data": {
        "output": {"type": "tool", "tool_call_id": "a", "content": "done", "status": "success"},
    }})
    projected = state.project(model_event([
        {"id": None, "name": None, "args": "}", "index": 1},
    ]))
    assert len(projected) == 1
    assert projected[0]["data"]["chunk"]["tool_calls"] == [{"id": "b", "name": "lookup", "args": "{}"}]


def test_unresolved_or_duplicate_tool_identity_fails_closed():
    from ai_gateway_core.exceptions import ValidationFailedError
    state = local_state()
    assert state.project(model_event([{"id": None, "name": "lookup", "args": "{}", "index": 0}])) == []
    with pytest.raises(ValidationFailedError, match="langgraph_tool_chunk_identity_missing"):
        state.project({"event": "on_chat_model_end", "run_id": "model-run", "data": {}})
    with pytest.raises(ValidationFailedError, match="langgraph_tool_call_identity_conflict"):
        local_state().project(model_event([
            {"id": "same", "name": "lookup", "args": "{}", "index": 0},
            {"id": "same", "name": "lookup", "args": "{}", "index": 1},
        ]))


def test_legacy_iterator_keeps_observed_start_identity():
    state = local_state()
    start = state.project({"event": "on_tool_start", "run_id": "legacy-run", "data": {"input": {}}})[0]
    end = state.project({"event": "on_tool_end", "run_id": "legacy-run", "data": {"output": {
        "type": "tool", "tool_call_id": "unbound-provider-id", "content": "done", "status": "success",
    }}})[0]
    assert state.tool_identity(start) == state.tool_identity(end) == "legacy-run"
    assert start["run_id"] == end["run_id"] == "legacy-run"
    assert end["data"]["output"].content == "done"


def test_correlation_conflicts_fail_and_lifecycle_dedup_uses_original_run():
    from ai_gateway_core.exceptions import ValidationFailedError
    state = local_state()
    state.bindings.update({"attempt-1": "call", "attempt-2": "call"})
    adapter = object.__new__(LangGraphAdapter)
    seen = set()
    for run_id in ["attempt-1", "attempt-2"]:
        event = state.project({"event": "on_tool_start", "run_id": run_id, "data": {"input": {}}})[0]
        assert state.tool_identity(event) == "call"
        assert adapter._extract_stream_event(event, seen) is not None
        assert adapter._extract_stream_event(event, seen) is None
    with pytest.raises(ValidationFailedError, match="langgraph_tool_call_identity_conflict"):
        state.project({"event": "on_tool_end", "run_id": "attempt-1", "data": {"output": {
            "type": "tool", "tool_call_id": "different", "content": "done",
        }}})


def test_arbitrary_dict_tool_result_is_still_json():
    state = local_state()
    output = {"content": "ordinary result", "count": 2}
    event = state.project({"event": "on_tool_end", "run_id": "run", "data": {"output": output}})[0]
    adapter = object.__new__(LangGraphAdapter)
    result = adapter._extract_stream_event(event, set())
    assert json.loads(result.text) == output


@pytest.mark.asyncio
async def test_official_root_tool_call_envelope_is_not_arguments():
    from src.adapters.langgraph_local import prepare_local_tool_stream

    async def echo(query: str) -> str:
        """Return controlled input."""
        return query
    tool = StructuredTool.from_function(coroutine=echo)
    config, state = prepare_local_tool_stream({})
    projected = []
    async with _owned_stream(tool.astream_events({
        "type": "tool_call", "name": "echo", "id": "root-call", "args": {"query": "hello"},
    }, config, version="v2")) as stream:
        async for event in stream:
            projected.extend(state.project(event))
    start = next(event for event in projected if event["event"] == "on_tool_start")
    end = next(event for event in projected if event["event"] == "on_tool_end")
    assert start["data"]["input"] == {"query": "hello"}
    assert state.tool_identity(start) == state.tool_identity(end) == "root-call"
    assert end["data"]["output"].content == "hello"


@pytest.mark.parametrize("missing", ["langchain_core", "dependency_inside_langchain"])
def test_optional_sdk_absence_preserves_legacy_but_nested_import_errors_propagate(monkeypatch, missing):
    import builtins

    from src.adapters.langgraph_local import prepare_local_tool_stream
    original = builtins.__import__

    def guarded(name, *args, **kwargs):
        if name == "langchain_core.callbacks.base":
            raise ModuleNotFoundError("controlled missing package", name=missing)
        return original(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", guarded)
    config = {"configurable": {"user_id": "user"}}
    if missing == "langchain_core":
        assert prepare_local_tool_stream(config) == (config, None)
    else:
        with pytest.raises(ModuleNotFoundError, match="controlled missing package"):
            prepare_local_tool_stream(config)


def test_invalid_callback_container_is_not_silently_replaced():
    from ai_gateway_core.exceptions import ValidationFailedError

    from src.adapters.langgraph_local import prepare_local_tool_stream
    with pytest.raises(ValidationFailedError, match="langgraph_callbacks_invalid"):
        prepare_local_tool_stream({"callbacks": "invalid"})


def test_late_call_id_and_fragmented_name_are_buffered_by_index():
    state = local_state()
    assert state.project(model_event([{"id": None, "name": None, "args": "{", "index": 0}])) == []
    first = state.project(model_event([{"id": "call", "name": "look", "args": "", "index": 0}]))
    second = state.project(model_event([{"id": None, "name": "up", "args": "}", "index": 0}]))
    assert first[0]["data"]["chunk"]["tool_calls"] == [{"id": "call", "name": "look", "args": "{"}]
    assert second[0]["data"]["chunk"]["tool_calls"] == [{"id": "call", "name": "lookup", "args": "{}"}]
    state.finish()


def test_stream_exhaustion_cannot_accept_unidentified_tool_fragments():
    from ai_gateway_core.exceptions import ValidationFailedError
    state = local_state()
    state.project(model_event([{"id": None, "name": "lookup", "args": "{}", "index": 0}]))
    with pytest.raises(ValidationFailedError, match="langgraph_tool_chunk_identity_missing"):
        state.finish()
