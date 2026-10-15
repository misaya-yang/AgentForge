from __future__ import annotations

import json
import uuid
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_gateway_contracts.agent_runtime_lease import RuntimeModelLeaseSigner

from src.services.agent_runtime.control_plane import AgentRuntimeControlPlane


@pytest.mark.asyncio
async def test_thread_terminal_closes_gateway_ledger_before_yield() -> None:
    run_id = str(uuid.uuid4())
    runtime_thread_id = str(uuid.uuid4())
    terminal = {
        "schema_version": "assistant-turn-contract/v1",
        "sequence": 7,
        "event_type": "run_finished",
        "data": {"run_id": run_id, "status": "succeeded"},
        "timestamp": "2026-08-31T00:00:00Z",
    }

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path.endswith(f"/{runtime_thread_id}/events")
        return httpx.Response(
            200,
            request=request,
            headers={"content-type": "text/event-stream"},
            content=f"event: run_finished\ndata: {json.dumps(terminal)}\n\n".encode(),
        )

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    plane = AgentRuntimeControlPlane(
        database=SimpleNamespace(fetchrow=AsyncMock(return_value={"run_id": "scoped"})),
        model_service=SimpleNamespace(),
        provider_service=SimpleNamespace(),
        assignment_store=SimpleNamespace(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=client,
    )
    completed: list[tuple[uuid.UUID, str]] = []

    async def complete(completed_run_id: uuid.UUID, status: str) -> None:
        completed.append((completed_run_id, status))

    plane._complete_run = complete  # type: ignore[method-assign]
    stream = plane.stream_thread_events(
        runtime_thread_id=runtime_thread_id,
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        turn_id=run_id,
    )
    try:
        event = await anext(stream)
        assert event["event_type"] == "run_finished"
        assert completed == [(uuid.UUID(run_id), "succeeded")]
    finally:
        await stream.aclose()
        await client.aclose()


@pytest.mark.asyncio
async def test_whole_thread_stream_continues_after_each_turn_terminal() -> None:
    runtime_thread_id = str(uuid.uuid4())
    first_run_id = str(uuid.uuid4())
    second_run_id = str(uuid.uuid4())
    events = [
        {
            "schema_version": "assistant-turn-contract/v1",
            "sequence": 1,
            "event_type": "run_finished",
            "data": {"run_id": first_run_id, "status": "succeeded"},
            "timestamp": "2026-08-31T00:00:00Z",
        },
        {
            "schema_version": "assistant-turn-contract/v1",
            "sequence": 2,
            "event_type": "run_started",
            "data": {"run_id": second_run_id, "status": "running"},
            "timestamp": "2026-08-31T00:00:01Z",
        },
        {
            "schema_version": "assistant-turn-contract/v1",
            "sequence": 3,
            "event_type": "run_finished",
            "data": {"run_id": second_run_id, "status": "succeeded"},
            "timestamp": "2026-08-31T00:00:02Z",
        },
    ]

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path.endswith(f"/{runtime_thread_id}/events")
        content = "".join(
            f"event: {event['event_type']}\ndata: {json.dumps(event)}\n\n" for event in events
        )
        return httpx.Response(
            200,
            request=request,
            headers={"content-type": "text/event-stream"},
            content=content.encode(),
        )

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    plane = AgentRuntimeControlPlane(
        database=SimpleNamespace(fetchrow=AsyncMock(return_value={"run_id": "scoped"})),
        model_service=SimpleNamespace(),
        provider_service=SimpleNamespace(),
        assignment_store=SimpleNamespace(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=client,
    )
    completed: list[tuple[uuid.UUID, str]] = []

    async def complete(completed_run_id: uuid.UUID, status: str) -> None:
        completed.append((completed_run_id, status))

    plane._complete_run = complete  # type: ignore[method-assign]
    stream = plane.stream_thread_events(
        runtime_thread_id=runtime_thread_id,
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        after_sequence=0,
        turn_id=None,
    )
    try:
        observed = [event async for event in stream]
        assert [event["sequence"] for event in observed] == [1, 2, 3]
        assert [event["data"]["run_id"] for event in observed] == [
            first_run_id,
            second_run_id,
            second_run_id,
        ]
        assert completed == [(uuid.UUID(first_run_id), "succeeded"), (uuid.UUID(second_run_id), "succeeded")]
    finally:
        await stream.aclose()
        await client.aclose()


@pytest.mark.parametrize("event_type,status", [("run_finished", "succeeded"), ("cancelled", "cancelled")])
async def test_v1_parent_terminal_is_durable_before_client_disconnect(event_type: str, status: str) -> None:
    from src.services.agent_runtime.control.types import AgentTurn

    run_id, child_id, thread_id = (str(uuid.uuid4()) for _ in range(3))
    child = {"event_type": "run_finished", "data": {"run_id": child_id, "status": "succeeded"}}
    parent = {"event_type": event_type, "data": {"run_id": run_id, "status": status}}
    content = "".join(f"event: {event['event_type']}\ndata: {json.dumps(event)}\n\n" for event in (child, parent))
    client = httpx.AsyncClient(transport=httpx.MockTransport(lambda request: httpx.Response(200, request=request, content=content)))
    complete = AsyncMock()
    plane = SimpleNamespace(runtime_url="http://runtime.test", runtime_internal_token="runtime-token",
                            http_client=client, _complete_run=complete)
    turn = AgentTurn(thread_id, run_id, str(uuid.uuid4()), str(uuid.uuid4()), 0, "auto", "auto", "adapter", 1, None)
    from src.services.agent_runtime.control.event_stream import stream_events
    stream = stream_events(plane, turn=turn, tenant_id="tenant", user_id="user", session_id="session")
    try:
        await anext(stream)
        complete.assert_not_awaited()
        await anext(stream)
        complete.assert_awaited_once_with(uuid.UUID(run_id), status)
        await stream.aclose()
        complete.assert_awaited_once()
    finally:
        await stream.aclose()
        await client.aclose()
