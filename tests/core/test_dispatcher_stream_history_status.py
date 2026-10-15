"""Persist local stream outcomes through the real dispatcher and history route."""

from unittest.mock import AsyncMock, MagicMock, patch

import httpx
import pytest
from ai_gateway_core.enums import ContentType, InvocationMode, StreamEventType
from ai_gateway_core.exceptions import ValidationFailedError
from fastapi.encoders import jsonable_encoder

from src.api.v1.sessions import get_history
from src.core.auth.user_resolver import UserContext
from src.core.gateway.dispatcher import GatewayDispatcher
from src.models.request import ContentItem, UnifiedRequest
from src.models.response import StreamChunk, ToolCall
from src.models.service import ServiceDefinition
from src.services.session.session_manager import SessionManager


async def case(ending):
    service = ServiceDefinition(
        service_id="fixture",
        name="Fixture",
        supported_modes=[InvocationMode.STREAM],
        session_enabled=True,
    )
    sessions = SessionManager()

    async def stream(req):
        yield StreamChunk(
            request_id=req.request_id,
            chunk_index=0,
            content=ContentItem(
                type=ContentType.TEXT,
                data="" if ending == "tool_failed" else "partial answer",
            ),
            event_type=StreamEventType.TOOL_CALL_START
            if ending == "tool_failed"
            else StreamEventType.TEXT_DELTA,
            tool_call=ToolCall("tool", "fixture", "{}", "running")
            if ending == "tool_failed"
            else None,
        )
        if ending in {"failed", "tool_failed"}:
            raise ValidationFailedError("fixture_provider_error")
        if ending == "transport_unknown":
            raise httpx.ReadTimeout("fixture_transport_interrupted")
        yield StreamChunk(
            request_id=req.request_id,
            chunk_index=1,
            content=ContentItem(type=ContentType.TEXT, data=""),
            event_type=StreamEventType.FINAL,
            is_final=True,
        )

    adapter = MagicMock()
    adapter.stream = stream
    registry = MagicMock()
    registry.get = AsyncMock(return_value=service)
    registry.get_adapter.return_value = adapter
    validator = MagicMock()
    validator.validate = AsyncMock()
    limiter = MagicMock()
    limiter.enforce = AsyncMock()
    gateway = GatewayDispatcher(
        registry, validator, limiter, MagicMock(), MagicMock(), sessions
    )
    request = UnifiedRequest(
        request_id="fixture",
        service_id="fixture",
        user_id="owner",
        tenant_id="tenant",
        inputs=[ContentItem(type=ContentType.TEXT, data="input")],
    )
    recorder = AsyncMock()
    with patch("src.services.metrics.get_usage_recorder", return_value=recorder):
        producer = gateway.stream(request, ["user"])
        if ending == "interrupted":
            await anext(producer)
            await producer.aclose()
        elif ending == "closed_after_terminal":
            while not (await anext(producer)).is_final:
                pass
            await producer.aclose()
        else:
            try:
                result = [v async for v in producer]
            except ValidationFailedError:
                assert ending in {"failed", "tool_failed"}
            except httpx.ReadTimeout:
                assert ending == "transport_unknown"
            else:
                assert ending == "completed" and result[-1].is_final
    history = await get_history(
        request.session_id,
        200,
        sessions,
        UserContext(user_id="owner", tenant_id="tenant", roles=["user"]),
    )
    assistants = [v for v in history if v["role"] == "assistant"]
    assert len(assistants) == 1
    assert assistants[0]["content"] == (
        "" if ending == "tool_failed" else "partial answer"
    )
    if ending == "tool_failed":
        assert assistants[0]["metadata"]["tool_calls"][0]["tool_call_id"] == "tool"
    assert all(
        row["inflight"] == 0 for row in gateway.admission_controller.snapshot().values()
    )
    return {"name": ending, "expected": ending, "history": jsonable_encoder(history)}


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("ending", "expected"),
    [
        ("completed", "completed"),
        ("failed", "failed"),
        ("interrupted", "interrupted"),
        ("transport_unknown", "interrupted"),
        ("closed_after_terminal", "interrupted"),
        ("tool_failed", "failed"),
    ],
)
async def test_stream_history_preserves_local_terminal_observation(ending, expected):
    receipt = await case(ending)
    assert receipt["history"][-1]["metadata"]["gateway_stream_status"] == expected
