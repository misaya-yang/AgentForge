from __future__ import annotations

import json
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from ai_gateway_core.enums import ContentType, InvocationMode, ServiceType, TransportType
from langchain_core.caches import InMemoryCache
from langchain_core.callbacks import AsyncCallbackHandler
from langchain_core.callbacks.manager import AsyncCallbackManager
from langchain_core.language_models import BaseChatModel
from langchain_core.messages import AIMessage, AIMessageChunk, HumanMessage
from langchain_core.outputs import ChatGeneration, ChatGenerationChunk, ChatResult
from langchain_core.runnables import RunnableLambda

from src.adapters.langgraph import LangGraphAdapter
from src.api.schemas.response import UnifiedResponseSchema
from src.api.v1.sessions import get_history
from src.core.auth.user_resolver import UserContext
from src.core.gateway.dispatcher import GatewayDispatcher, estimate_tokens
from src.models.request import ContentItem, UnifiedRequest
from src.models.service import ServiceDefinition
from src.services.session.session_manager import SessionManager


class UsageModel(BaseChatModel):
    reported: dict | None = None
    fail: bool = False
    calls: int = 0

    @property
    def _llm_type(self):
        return "controlled-local-usage"

    def _generate(self, *_args, **_kwargs):
        raise AssertionError("official asynchronous invocation required")

    async def _agenerate(self, messages, **kwargs):
        del messages, kwargs
        self.calls += 1
        if self.fail:
            raise RuntimeError("SYNTHETIC_MODEL_FAILURE")
        return ChatResult(generations=[ChatGeneration(message=AIMessage(
            content="Answer", usage_metadata=self.reported,
        ))])


    async def _astream(self, messages, **kwargs):
        del messages, kwargs
        self.calls += 1
        yield ChatGenerationChunk(message=AIMessageChunk(content="An", usage_metadata=self.reported))
        yield ChatGenerationChunk(message=AIMessageChunk(content="swer"))


class Observer(AsyncCallbackHandler):
    run_inline = True

    def __init__(self):
        self.ends = []

    async def on_llm_end(self, response, *, run_id, **kwargs):
        del response, kwargs
        self.ends.append(str(run_id))


async def roundtrip(reports, *, historical=False, recover=False, fail=False, manager=False, bypass=False, cached=False, streamed=False):
    models = [UsageModel(reported=usage) for usage in reports]
    if cached:
        models[0].cache = InMemoryCache()
        models = [models[0], models[0]]
    observer = Observer()
    callbacks = AsyncCallbackManager([observer], [observer]) if manager else [observer]
    original = list(callbacks.handlers if manager else callbacks)

    async def execute(state, config):
        reply = AIMessage(content="Answer")
        if fail or recover:
            try:
                await UsageModel(fail=True).ainvoke(state["messages"], config)
            except RuntimeError:
                if not recover:
                    raise
        for model in models:
            if streamed:
                reply = None
                async for chunk in model.astream(state["messages"], config):
                    reply = chunk if reply is None else reply + chunk
            else:
                reply = await model.ainvoke(state["messages"], config)
        if historical:
            return {"messages": [HumanMessage(content="old question"), AIMessage(
                content="old answer", usage_metadata={
                    "input_tokens": 9000, "output_tokens": 8000, "total_tokens": 17000,
                },
            ), reply]}
        return reply

    service = ServiceDefinition(
        service_id="local-usage", name="Local usage", service_type=ServiceType.CUSTOM,
        connector_type=TransportType.IN_PROCESS, supported_modes=[InvocationMode.SYNC],
        accepted_content_types=[ContentType.TEXT], output_content_types=[ContentType.TEXT],
        session_enabled=True, max_retries=1, circuit_breaker_enabled=False,
    )
    adapter = object.__new__(LangGraphAdapter)
    adapter.remote = False
    adapter.graph = RunnableLambda(execute)
    if bypass:
        adapter.graph = SimpleNamespace(ainvoke=AsyncMock(return_value=AIMessage(
            content="Answer", usage_metadata=reports[0],
        )))
    adapter.service = service
    adapter.config = {}
    adapter.assistant_id = "fixture-agent"
    adapter._build_run_config = AsyncMock(return_value={"callbacks": callbacks})
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
        request_id="usage-request", service_id=service.service_id,
        inputs=[ContentItem(ContentType.TEXT, "Question")], user_id="user", tenant_id="tenant",
    )
    recorder = AsyncMock()
    error = None
    with patch("src.services.metrics.get_usage_recorder", return_value=recorder):
        try:
            response = await dispatcher.invoke(request, ["user"])
        except RuntimeError as exc:
            error = exc
            response = None
    history = await get_history(
        request.session_id, limit=50, session_manager=sessions,
        user=UserContext(user_id="user", tenant_id="tenant"),
    )
    assert list(callbacks.handlers if manager else callbacks) == original
    return SimpleNamespace(
        wire=json.loads(UnifiedResponseSchema.from_domain(response).model_dump_json()) if response else None,
        history=history, recorder=recorder, observer=observer, error=error,
        model_calls=[model.calls for model in models],
    )


A = {"input_tokens": 101, "output_tokens": 23, "total_tokens": 124}
B = {"input_tokens": 59, "output_tokens": 7, "total_tokens": 66}
ZERO = {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}


@pytest.mark.asyncio
@pytest.mark.parametrize("historical", [False, True])
@pytest.mark.parametrize("manager", [False, True])
async def test_actual_sdk_usage_observation_survives_response_history_and_recorder(historical, manager):
    value = await roundtrip([A, B], historical=historical, manager=manager)
    observed = value.wire["metadata"]["local_model_usage"]
    assert observed["coverage"] == "complete"
    assert observed["observed_model_runs"] == observed["reported_model_runs"] == 2
    assert observed["observed_input"] == 160
    assert observed["observed_output"] == 30
    assert observed["observed_total"] == 190
    assert observed["accounting_eligible"] is False
    assert observed["accounting_reason"] == "model_binding_unverified"
    assert value.wire["usage"] is None
    assistant = next(message for message in value.history if message["role"] == "assistant")
    assert assistant["metadata"]["local_model_usage"] == observed
    recorded = value.recorder.record_usage.await_args.kwargs
    assert recorded["metadata"]["local_model_usage"] == observed
    assert recorded["metadata"]["token_source"] == "estimated"
    assert assistant["metadata"]["stats"]["token_provenance"]["input_tokens"] == "estimated"
    assert recorded["input_tokens"] == estimate_tokens("Question")
    assert recorded["output_tokens"] == estimate_tokens("Answer")
    assert len(value.observer.ends) == 2 and value.model_calls == [1, 1]


@pytest.mark.asyncio
@pytest.mark.parametrize(("reports", "coverage", "reported", "expected"), [
    ([A, None], "partial", 1, A),
    ([None, None], "unknown", 0, None),
    ([ZERO], "complete", 1, ZERO),
    ([], "unknown", 0, None),
])
async def test_usage_coverage_and_explicit_zero(reports, coverage, reported, expected):
    value = await roundtrip(reports)
    observation = value.wire["metadata"]["local_model_usage"]
    assert observation["coverage"] == coverage
    assert observation["observed_model_runs"] == len(reports)
    assert observation["reported_model_runs"] == reported
    if expected is None:
        assert "observed_total" not in observation
    else:
        assert observation["observed_total"] == expected["total_tokens"]
    assert value.wire["usage"] is None
    assert value.recorder.record_usage.await_args.kwargs["metadata"]["token_source"] == "estimated"


@pytest.mark.asyncio
async def test_recovered_model_error_prevents_complete_coverage():
    value = await roundtrip([A], recover=True)
    observation = value.wire["metadata"]["local_model_usage"]
    assert observation["coverage"] == "partial"
    assert observation["observed_model_runs"] == 2
    assert observation["reported_model_runs"] == observation["completed_model_runs"] == 1
    assert observation["observed_total"] == 124


@pytest.mark.asyncio
async def test_unhandled_model_failure_keeps_existing_error_accounting():
    value = await roundtrip([A], fail=True)
    assert value.error is not None and value.wire is None
    assistants = [message for message in value.history if message["role"] == "assistant"]
    assert len(assistants) == 1
    assert assistants[0]["content"] == ""
    assert assistants[0]["metadata"] == {"gateway_stream_status": "failed"}
    recorded = value.recorder.record_usage.await_args.kwargs
    assert recorded["status"] == "error"
    assert "local_model_usage" not in recorded["metadata"]
    assert value.model_calls == [0]


@pytest.mark.asyncio
async def test_unobserved_returned_message_does_not_claim_execution_usage():
    value = await roundtrip([A], bypass=True)
    observation = value.wire["metadata"]["local_model_usage"]
    assert observation["coverage"] == "unknown"
    assert observation["observed_model_runs"] == 0
    assert "observed_total" not in observation


@pytest.mark.asyncio
async def test_actual_sdk_cache_replay_is_not_provider_consumption():
    value = await roundtrip([A], cached=True)
    observation = value.wire["metadata"]["local_model_usage"]
    assert observation["coverage"] == "complete"
    assert observation["observed_model_runs"] == 2
    assert observation["observed_total"] == 248
    assert observation["sdk_zero_cost_runs"] == 1
    assert value.model_calls == [1, 1]  # The same model instance executed only once.
    assert len(value.observer.ends) == 2
    assert value.wire["usage"] is None
    assert value.recorder.record_usage.await_args.kwargs["metadata"]["token_source"] == "estimated"


@pytest.mark.asyncio
async def test_blocking_graph_observes_sdk_aggregated_streamed_model_usage_once():
    value = await roundtrip([A, B], streamed=True)
    observation = value.wire["metadata"]["local_model_usage"]
    assert observation["coverage"] == "complete"
    assert observation["observed_total"] == 190
    assert observation["observed_model_runs"] == 2
    assert value.wire["outputs"][0]["data"] == "Answer"


@pytest.mark.asyncio
async def test_duplicate_model_end_replaces_observation_without_double_counting():
    from langchain_core.outputs import LLMResult

    from src.adapters.langgraph_local_usage import prepare_local_invoke_usage

    _, observer = prepare_local_invoke_usage({})
    await observer.on_chat_model_start({}, [], run_id="model-a")
    result = LLMResult(generations=[[ChatGeneration(message=AIMessage(content="", usage_metadata=A))]])
    await observer.on_llm_end(result, run_id="model-a")
    await observer.on_llm_end(result, run_id="model-a")
    assert observer.result()["observed_total"] == 124
    assert observer.result()["observed_model_runs"] == 1
    await observer.on_llm_error(RuntimeError("synthetic"), run_id="model-a")
    await observer.on_llm_end(result, run_id="model-a")
    assert observer.result()["coverage"] == "partial"


@pytest.mark.asyncio
@pytest.mark.parametrize("usage", [
    {"input_tokens": True, "output_tokens": 1, "total_tokens": 2},
    {"input_tokens": -1, "output_tokens": 1, "total_tokens": 0},
    {"input_tokens": 1.0, "output_tokens": 1, "total_tokens": 2},
    {"input_tokens": "1", "output_tokens": 1, "total_tokens": 2},
    {"input_tokens": 1, "output_tokens": 1, "total_tokens": 3},
    {"input_tokens": 1, "output_tokens": 1},
    {},
])
async def test_invalid_usage_is_unknown_without_coercion_or_fabrication(usage):
    from langchain_core.outputs import LLMResult

    from src.adapters.langgraph_local_usage import prepare_local_invoke_usage

    _, observer = prepare_local_invoke_usage({})
    await observer.on_chat_model_start({}, [], run_id="model")
    message = AIMessage(content="").model_copy(update={"usage_metadata": usage})
    await observer.on_llm_end(LLMResult(generations=[[ChatGeneration(message=message)]]), run_id="model")
    assert observer.result()["coverage"] == "unknown"
    assert "observed_total" not in observer.result()


@pytest.mark.asyncio
@pytest.mark.parametrize("start", ["absent", "nonchat", "unfinished"])
async def test_unverified_model_lifecycle_cannot_claim_complete(start):
    from langchain_core.outputs import LLMResult

    from src.adapters.langgraph_local_usage import prepare_local_invoke_usage

    _, observer = prepare_local_invoke_usage({})
    if start == "nonchat":
        await observer.on_llm_start({}, [], run_id="model")
    elif start == "unfinished":
        await observer.on_chat_model_start({}, [], run_id="pending")
        await observer.on_chat_model_start({}, [], run_id="model")
    await observer.on_llm_end(LLMResult(generations=[[ChatGeneration(message=AIMessage(
        content="", usage_metadata=A,
    ))]]), run_id="model")
    assert observer.result()["coverage"] == "partial"


@pytest.mark.parametrize("missing", ["langchain_core", "sdk_transitive_fixture"])
def test_optional_sdk_boundary(missing):
    import builtins

    from src.adapters.langgraph_local_usage import prepare_local_invoke_usage

    original = builtins.__import__

    def importing(name, *args, **kwargs):
        if name == "langchain_core.callbacks.base":
            raise ModuleNotFoundError("synthetic", name=missing)
        return original(name, *args, **kwargs)

    config = {"callbacks": []}
    with patch("builtins.__import__", side_effect=importing):
        if missing == "langchain_core":
            assert prepare_local_invoke_usage(config) == (config, None)
        else:
            with pytest.raises(ModuleNotFoundError, match="synthetic"):
                prepare_local_invoke_usage(config)


def test_invalid_callback_configuration_is_explicit():
    from ai_gateway_core.exceptions import ValidationFailedError

    from src.adapters.langgraph_local_usage import prepare_local_invoke_usage

    with pytest.raises(ValidationFailedError, match="langgraph_callbacks_invalid"):
        prepare_local_invoke_usage({"callbacks": object()})


@pytest.mark.asyncio
async def test_multiple_generations_explicitly_scope_first_generation_only():
    from langchain_core.outputs import LLMResult

    from src.adapters.langgraph_local_usage import prepare_local_invoke_usage

    _, observer = prepare_local_invoke_usage({})
    await observer.on_chat_model_start({}, [], run_id="model")
    await observer.on_llm_end(LLMResult(generations=[[
        ChatGeneration(message=AIMessage(content="first", usage_metadata=A)),
        ChatGeneration(message=AIMessage(content="alternate", usage_metadata=B)),
    ]]), run_id="model")
    assert observer.result()["coverage_scope"] == "observed_chat_model_first_generations"
    assert observer.result()["observed_total"] == A["total_tokens"]


@pytest.mark.asyncio
@pytest.mark.parametrize("metadata", [None, [], "legacy"])
async def test_unrelated_adapter_metadata_keeps_existing_dispatch_behavior(metadata):
    from src.models.response import UnifiedResponse
    from tests.core.test_dispatcher_token_provenance import dispatcher, request

    adapter = MagicMock()
    adapter.invoke = AsyncMock(return_value=UnifiedResponse(
        request_id="fixture-request", status="success",
        outputs=[ContentItem(ContentType.TEXT, "Answer")], metadata=metadata,
    ))
    value = dispatcher(adapter)
    with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
        response = await value.invoke(request(), ["user"])
    assert response.metadata == metadata
    saved = value.session_manager.add_message.await_args.kwargs["metadata"]
    assert "local_model_usage" not in saved
