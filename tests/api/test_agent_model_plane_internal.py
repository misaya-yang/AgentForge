from __future__ import annotations

import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import Request
from starlette.requests import ClientDisconnect

from src.api.internal.agent_model_plane import (
    _OwnedModelStreamResponse,
    _turn_metadata_header,
    responses,
)
from src.core.gateway.admission import CapacityRejected


def _request(headers: list[tuple[bytes, bytes]]) -> Request:
    return Request({"type": "http", "method": "POST", "path": "/", "headers": headers})


def test_turn_metadata_header_prefers_platform_name() -> None:
    request = _request(
        [
            (b"x-runtime-turn-metadata", b'{"source":"runtime"}'),
            (b"x-agent-turn-metadata", b'{"source":"platform"}'),
        ]
    )

    assert _turn_metadata_header(request) == '{"source":"platform"}'


def test_turn_metadata_header_accepts_one_runtime_alias() -> None:
    request = _request([(b"x-runtime-turn-metadata", b'{"lease":"signed"}')])

    assert _turn_metadata_header(request) == '{"lease":"signed"}'


def test_turn_metadata_header_rejects_ambiguous_runtime_aliases() -> None:
    request = _request(
        [
            (b"x-first-turn-metadata", b'{"lease":"one"}'),
            (b"x-second-turn-metadata", b'{"lease":"two"}'),
        ]
    )

    assert _turn_metadata_header(request) == ""


@pytest.mark.asyncio
async def test_capacity_rejection_is_503_before_streaming_headers():
    closed = []

    async def stream(**_kwargs):
        try:
            raise CapacityRejected(budget_key="provider", status_code=503)
            yield b""
        finally:
            closed.append(True)

    plane = SimpleNamespace(authorize_and_reserve=AsyncMock(return_value=object()), stream=stream)
    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(agent_model_plane_internal_token="fixture", agent_model_plane=plane)),
        headers={"authorization": "Bearer fixture", "x-agent-turn-metadata": "{}"},
        body=AsyncMock(return_value=json.dumps({"stream": True}).encode()),
    )
    response = await responses(request)
    assert response.status_code == 503
    assert response.headers["retry-after"] == "1"
    assert closed == [True]


@pytest.mark.asyncio
async def test_asgi_header_send_failure_closes_primed_provider():
    closed = []

    async def producer():
        try:
            yield b"first"
            yield b"second"
        finally:
            closed.append(True)

    source = producer()
    response = _OwnedModelStreamResponse(source, await anext(source))

    async def failed_send(_message):
        raise OSError("client disconnected before body")

    with pytest.raises(ClientDisconnect):
        await response({"type": "http", "asgi": {"spec_version": "2.4"}}, AsyncMock(), failed_send)
    assert closed == [True]
