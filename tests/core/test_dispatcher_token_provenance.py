"""Real dispatcher presentation checks with in-memory adapter/session/usage boundaries."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from ai_gateway_core.enums import (
    ContentType,
    InvocationMode,
    ServiceType,
    StreamEventType,
    TransportType,
)

from src.api.schemas.response import StreamChunkSchema
from src.core.gateway.dispatcher import GatewayDispatcher
from src.models.request import ContentItem, UnifiedRequest
from src.models.response import StreamChunk, UnifiedResponse
from src.models.service import ServiceDefinition

FIXTURE = json.loads(
    (Path(__file__).resolve().parents[1] / "fixtures/gateway/token_provenance_v1.json").read_text()
)
FIELDS = ("input_tokens", "output_tokens", "total_tokens")


def dispatcher(adapter):
    service = ServiceDefinition(
        service_id="fixture",
        name="Fixture",
        service_type=ServiceType.CUSTOM,
        connector_type=TransportType.HTTP,
        supported_modes=[InvocationMode.SYNC, InvocationMode.STREAM],
        accepted_content_types=[ContentType.TEXT],
        output_content_types=[ContentType.TEXT],
        session_enabled=True,
    )
    registry = MagicMock()
    registry.get = AsyncMock(return_value=service)
    registry.get_adapter.return_value = adapter
    validator = MagicMock()
    validator.validate = AsyncMock()
    limiter = MagicMock()
    limiter.enforce = AsyncMock()
    sessions = MagicMock()
    sessions.get_or_create = AsyncMock(return_value=MagicMock(session_id="fixture-session"))
    sessions.add_message = AsyncMock()
    return GatewayDispatcher(registry, validator, limiter, MagicMock(), MagicMock(), sessions)


def request():
    return UnifiedRequest(
        request_id="fixture-request",
        service_id="fixture",
        user_id="fixture-user",
        tenant_id="fixture-tenant",
        inputs=[ContentItem(type=ContentType.TEXT, data=FIXTURE["input_text"])],
    )


def counts(stats):
    return {key: stats[key] for key in FIELDS if key in stats}


def assistant_stats(value):
    return [
        call.kwargs["metadata"]["stats"]
        for call in value.session_manager.add_message.await_args_list
        if call.args[1] == "assistant"
    ][-1]


async def run_case(case, mode, cls=GatewayDispatcher):
    adapter = MagicMock()

    async def producer(req):
        yield StreamChunk(
            request_id=req.request_id,
            chunk_index=0,
            content=ContentItem(type=ContentType.TEXT, data=FIXTURE["output_text"]),
            event_type=StreamEventType.TEXT_DELTA,
        )
        yield StreamChunk(
            request_id=req.request_id,
            chunk_index=1,
            content=ContentItem(type=ContentType.TEXT, data=""),
            event_type=StreamEventType.FINAL,
            is_final=True,
            metadata={"usage": case["usage"]} if case["usage"] is not None else None,
        )

    adapter.stream = producer
    adapter.invoke = AsyncMock(
        return_value=UnifiedResponse(
            request_id="fixture-request",
            status="success",
            outputs=[ContentItem(type=ContentType.TEXT, data=FIXTURE["output_text"])],
            usage=case["usage"],
        )
    )
    value = dispatcher(adapter)
    value.__class__ = cls
    recorder = AsyncMock()
    with patch("src.services.metrics.get_usage_recorder", return_value=recorder):
        if mode == "stream":
            chunks = [chunk async for chunk in value.stream(request(), ["user"])]
            terminal = chunks[-1].metadata["usage"]
            wire = json.loads(StreamChunkSchema.from_domain(chunks[-1]).model_dump_json())
            assert wire["metadata"]["usage"] == terminal
        else:
            await value.invoke(request(), ["user"])
            terminal = None
    history = assistant_stats(value)
    assert json.loads(json.dumps(history)) == history
    return terminal, history, recorder.record_usage.await_args.kwargs


@pytest.mark.parametrize("case", FIXTURE["cases"], ids=lambda case: case["name"])
@pytest.mark.parametrize("mode", ["sync", "stream"])
async def test_actual_dispatcher_provenance(case, mode):
    terminal, history, _ = await run_case(case, mode)
    assert counts(history) == case[f"{mode}_counts"]
    assert history["token_provenance"] == case[f"{mode}_provenance"]
    if terminal is not None:
        assert counts(terminal) == counts(history)
        assert terminal["token_provenance"] == history["token_provenance"]


async def test_later_value_clears_stale_marker_without_erasing_other_fields():
    async def producer(req):
        for index, usage in enumerate(
            [
                {
                    "input_tokens": 2,
                    "output_tokens": 3,
                    "token_provenance": {"input_tokens": "estimated", "output_tokens": "estimated"},
                },
                {"input_tokens": 5},
                {"token_provenance": {"output_tokens": "upstream"}},
            ]
        ):
            yield StreamChunk(
                request_id=req.request_id,
                chunk_index=index,
                content=ContentItem(type=ContentType.TEXT, data=""),
                metadata={"usage": usage},
            )

    adapter = MagicMock()
    adapter.stream = producer
    value = dispatcher(adapter)
    with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
        chunks = [chunk async for chunk in value.stream(request(), ["user"])]
    stats = chunks[-1].metadata["usage"]
    assert counts(stats) == {"input_tokens": 5, "output_tokens": 3}
    assert stats["token_provenance"] == {"input_tokens": "upstream", "output_tokens": "estimated"}


async def test_error_placeholder_zero_remains_unknown():
    async def producer(_req):
        if False:
            yield
        raise RuntimeError("synthetic")

    adapter = MagicMock()
    adapter.stream = producer
    value = dispatcher(adapter)
    value._record_usage_event = AsyncMock()
    with pytest.raises(RuntimeError, match="synthetic"):
        _ = [chunk async for chunk in value.stream(request(), ["user"])]
    stats = value._record_usage_event.await_args.kwargs["payload"]["usage"]
    assert counts(stats) == dict.fromkeys(FIELDS, 0)
    assert "token_provenance" not in stats


async def test_cancelled_partial_stream_keeps_estimates_distinct():
    async def producer(req):
        yield StreamChunk(
            request_id=req.request_id,
            chunk_index=0,
            content=ContentItem(type=ContentType.TEXT, data=FIXTURE["output_text"]),
        )

    adapter = MagicMock()
    adapter.stream = producer
    value = dispatcher(adapter)
    value._record_usage_event = AsyncMock()
    stream = value.stream(request(), ["user"])
    await anext(stream)
    await stream.aclose()
    stats = value._record_usage_event.await_args.kwargs["payload"]["usage"]
    assert counts(stats) == {"input_tokens": 4, "output_tokens": 3, "total_tokens": 7}
    assert "token_provenance" not in stats
    assert assistant_stats(value)["token_provenance"] == {
        "input_tokens": "estimated",
        "output_tokens": "estimated",
        "total_tokens": "derived",
    }
    assert value._record_usage_event.await_args.kwargs["status"] == "unknown"


def test_invalid_metadata_cannot_raise_during_provenance_only_processing():
    from src.core.gateway.dispatcher import _observe_token_provenance

    for label in [None, [], {}, True, 3.4, "measured"]:
        assert _observe_token_provenance(
            {"input_tokens": 2, "token_provenance": {"input_tokens": label}}
        ) == {"input_tokens": "unknown"}


@pytest.mark.parametrize("mode", ["sync", "stream"])
@pytest.mark.parametrize("invalid", [None, [], {}, "invalid"])
async def test_invalid_arithmetic_retains_existing_failure(mode, invalid):
    case = {"usage": {"input_tokens": invalid}}
    with pytest.raises(TypeError):
        await run_case(case, mode)


def test_sdk_provenance_annotation_is_not_added_or_reinterpreted():
    from src.core.gateway.dispatcher import _annotate_token_provenance

    for stats in [
        {"sdk_usage": {"state": "unavailable"}},
        {
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0,
            "sdk_usage": {"state": "reported"},
        },
    ]:
        original = json.loads(json.dumps(stats))
        _annotate_token_provenance(stats, {}, estimated=FIELDS[:2], derived_total=True)
        assert stats == original


@pytest.mark.parametrize("cancel", [False, True])
async def test_fallback_accounting_preserves_reserved_field_collision(cancel):
    collision = {
        "input_tokens": 100,
        "output_tokens": 200,
        "total_tokens": 300,
        "model": "nested-model",
        "provider": "nested-provider",
        "assistant_id": "nested-assistant",
    }

    async def producer(req):
        yield StreamChunk(
            request_id=req.request_id,
            chunk_index=0,
            content=ContentItem(type=ContentType.TEXT, data=FIXTURE["output_text"]),
            metadata={
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 2,
                    "total_tokens": 3,
                    "token_provenance": collision,
                }
            },
        )
        raise RuntimeError("synthetic")

    adapter = MagicMock()
    adapter.stream = producer
    value = dispatcher(adapter)
    recorder = AsyncMock()
    with patch("src.services.metrics.get_usage_recorder", return_value=recorder):
        stream = value.stream(request(), ["user"])
        await anext(stream)
        if cancel:
            await stream.aclose()
        else:
            with pytest.raises(RuntimeError, match="synthetic"):
                await anext(stream)
    recorded = recorder.record_usage.await_args.kwargs
    assert (recorded["input_tokens"], recorded["output_tokens"]) == (100, 200)
    assert (recorded["model"], recorded["provider"], recorded["assistant_id"]) == (
        "nested-model",
        "nested-provider",
        "nested-assistant",
    )
    assert assistant_stats(value)["token_provenance"] == dict.fromkeys(FIELDS, "unknown")
