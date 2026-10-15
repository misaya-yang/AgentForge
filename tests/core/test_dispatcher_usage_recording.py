from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import httpx
import pytest
from ai_gateway_core.comm.retry import UpstreamOutcomeUnknown
from ai_gateway_core.enums import (
    ContentType,
    InvocationMode,
    ServiceType,
    StreamEventType,
    TransportType,
)

from src.core.gateway.dispatcher import GatewayDispatcher
from src.models.request import ContentItem, UnifiedRequest
from src.models.response import StreamChunk, UnifiedResponse
from src.models.service import ServiceDefinition


def _build_service() -> ServiceDefinition:
    return ServiceDefinition(
        service_id="agent_service",
        name="Agent Service",
        service_type=ServiceType.LANGGRAPH,
        connector_type=TransportType.HTTP,
        supported_modes=[InvocationMode.SYNC, InvocationMode.STREAM],
        accepted_content_types=[ContentType.TEXT],
        output_content_types=[ContentType.TEXT],
        metadata={"provider": "langgraph"},
        connector_config={"assistant_id": "asst_agent_001"},
    )


def _build_dispatcher(adapter: MagicMock) -> GatewayDispatcher:
    registry = MagicMock()
    registry.get = AsyncMock(return_value=_build_service())
    registry.get_adapter = MagicMock(return_value=adapter)

    validator = MagicMock()
    validator.validate = AsyncMock()

    rate_limiter = MagicMock()
    rate_limiter.enforce = AsyncMock()

    task_manager = MagicMock()
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock()
    session_manager.add_message = AsyncMock()

    return GatewayDispatcher(
        registry=registry,
        validator=validator,
        rate_limiter=rate_limiter,
        rbac=MagicMock(),
        task_manager=task_manager,
        session_manager=session_manager,
    )


@pytest.mark.asyncio
async def test_mutation_unknown_is_recorded_without_replaying_adapter():
    adapter = MagicMock()
    adapter.invoke = AsyncMock(side_effect=httpx.ReadTimeout("private upstream diagnostics"))
    dispatcher = _build_dispatcher(adapter)
    request = UnifiedRequest(request_id="once", service_id="agent_service", tenant_id="tenant", user_id="user", inputs=[])
    recorder = AsyncMock()
    with (patch("src.services.metrics.get_usage_recorder", return_value=recorder),
          pytest.raises(UpstreamOutcomeUnknown, match="UPSTREAM_OUTCOME_UNKNOWN") as error):
        await dispatcher.invoke(request, roles=["user"])
    assert "private" not in str(error.value)
    assert adapter.invoke.await_count == 1
    assert recorder.record_usage.call_args.kwargs["status"] == "unknown"
    assert all(row["inflight"] == 0 for row in dispatcher.admission_controller.snapshot().values())


@pytest.mark.asyncio
async def test_stream_consumer_close_closes_producer_and_releases_admission():
    closed = []

    async def stream(request):
        try:
            yield StreamChunk(request_id=request.request_id, chunk_index=0,
                              content=ContentItem(type=ContentType.TEXT, data="part"))
            yield StreamChunk(request_id=request.request_id, chunk_index=1,
                              content=ContentItem(type=ContentType.TEXT, data="more"))
        finally:
            closed.append(True)

    adapter = MagicMock()
    adapter.stream = stream
    dispatcher = _build_dispatcher(adapter)
    request = UnifiedRequest(request_id="cancel", service_id="agent_service", tenant_id="tenant", user_id="user", inputs=[])
    recorder = AsyncMock()
    with patch("src.services.metrics.get_usage_recorder", return_value=recorder):
        chunks = dispatcher.stream(request, roles=["user"])
        await anext(chunks)
        await chunks.aclose()
    assert closed == [True]
    assert all(row["inflight"] == 0 for row in dispatcher.admission_controller.snapshot().values())


@pytest.mark.asyncio
async def test_invoke_records_usage_for_registered_service():
    adapter = MagicMock()
    adapter.invoke = AsyncMock(
        return_value=UnifiedResponse(
            request_id="req_invoke_1",
            status="success",
            outputs=[ContentItem(type=ContentType.TEXT, data="ok")],
            usage={
                "prompt_tokens": 11,
                "completion_tokens": 9,
                "total_tokens": 20,
                "model": "gpt-4o-mini",
            },
        )
    )
    dispatcher = _build_dispatcher(adapter)
    request = UnifiedRequest(
        request_id="req_invoke_1",
        service_id="agent_service",
        user_id="user_123",
        tenant_id="default",
        inputs=[ContentItem(type=ContentType.TEXT, data="hello")],
    )

    recorder = AsyncMock()
    with patch("src.services.metrics.get_usage_recorder", return_value=recorder):
        await dispatcher.invoke(request, roles=["user"])

    recorder.record_usage.assert_awaited_once()
    kwargs = recorder.record_usage.await_args.kwargs
    assert kwargs["service_id"] == "agent_service"
    assert kwargs["user_id"] == "user_123"
    assert kwargs["assistant_id"] == "asst_agent_001"
    assert kwargs["input_tokens"] == 11
    assert kwargs["output_tokens"] == 9
    assert kwargs["request_type"] == "invoke"


@pytest.mark.asyncio
async def test_stream_records_usage_for_registered_service():
    async def _stream(_request: UnifiedRequest):
        yield StreamChunk(
            request_id=_request.request_id,
            chunk_index=0,
            content=ContentItem(type=ContentType.TEXT, data="answer"),
            event_type=StreamEventType.TEXT_DELTA,
        )
        yield StreamChunk(
            request_id=_request.request_id,
            chunk_index=1,
            content=ContentItem(type=ContentType.TEXT, data=""),
            is_final=True,
            event_type=StreamEventType.FINAL,
            metadata={
                "usage": {
                    "input_tokens": 5,
                    "output_tokens": 7,
                    "total_tokens": 12,
                    "model": "gpt-4o-mini",
                }
            },
        )

    adapter = MagicMock()
    adapter.stream = _stream

    dispatcher = _build_dispatcher(adapter)
    request = UnifiedRequest(
        request_id="req_stream_1",
        service_id="agent_service",
        user_id="user_456",
        tenant_id="default",
        inputs=[ContentItem(type=ContentType.TEXT, data="hello stream")],
    )

    recorder = AsyncMock()
    with patch("src.services.metrics.get_usage_recorder", return_value=recorder):
        chunks = []
        async for chunk in dispatcher.stream(request, roles=["user"]):
            chunks.append(chunk)

    assert any(chunk.event_type == StreamEventType.STREAM_END for chunk in chunks)
    recorder.record_usage.assert_awaited_once()
    kwargs = recorder.record_usage.await_args.kwargs
    assert kwargs["service_id"] == "agent_service"
    assert kwargs["request_type"] == "stream"
    assert kwargs["input_tokens"] == 5
    assert kwargs["output_tokens"] == 7


@pytest.mark.asyncio
async def test_stream_error_without_usage_records_zero_tokens():
    async def _stream(_request: UnifiedRequest):
        if False:
            yield  # pragma: no cover - keep async generator shape
        raise RuntimeError("upstream stream failed")

    adapter = MagicMock()
    adapter.stream = _stream

    dispatcher = _build_dispatcher(adapter)
    request = UnifiedRequest(
        request_id="req_stream_err_1",
        service_id="agent_service",
        user_id="user_789",
        tenant_id="default",
        inputs=[ContentItem(type=ContentType.TEXT, data="hello error")],
    )

    recorder = AsyncMock()
    with (
        patch("src.services.metrics.get_usage_recorder", return_value=recorder),
        pytest.raises(RuntimeError, match="upstream stream failed"),
    ):
        async for _ in dispatcher.stream(request, roles=["user"]):
            pass

    recorder.record_usage.assert_awaited_once()
    kwargs = recorder.record_usage.await_args.kwargs
    assert kwargs["service_id"] == "agent_service"
    assert kwargs["status"] == "error"
    assert kwargs["input_tokens"] == 0
    assert kwargs["output_tokens"] == 0
