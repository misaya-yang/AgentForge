"""An unavailable event observer must not terminate the observed execution."""

from __future__ import annotations

import uuid
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest

from src.services.agent_runtime.control.event_stream import stream_events
from src.services.agent_runtime.control.types import AgentRuntimeControlError


@pytest.mark.asyncio
@pytest.mark.parametrize("status", [403, 404, 429, 500, 503])
async def test_event_observer_rejection_does_not_fail_execution(status: int) -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "GET"
        return httpx.Response(status, request=request)

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        plane = SimpleNamespace(
            runtime_url="http://runtime.test",
            runtime_internal_token="fixture-only",
            http_client=client,
            _fail_run=AsyncMock(),
            _complete_run=AsyncMock(),
        )
        turn = SimpleNamespace(
            runtime_thread_id=str(uuid.uuid4()),
            run_id=str(uuid.uuid4()),
            snapshot_id=str(uuid.uuid4()),
            after_sequence=0,
        )
        with pytest.raises(AgentRuntimeControlError) as error:
            async for _ in stream_events(
                plane,
                turn=turn,
                tenant_id="tenant-a",
                user_id="user-a",
                session_id="session-a",
            ):
                pytest.fail("Rejected observer must not emit a frame")
        assert error.value.status_code == 503
        plane._fail_run.assert_not_awaited()
        plane._complete_run.assert_not_awaited()
