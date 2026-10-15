from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import httpx
import pytest
from ai_gateway_core.enums import ContentType, StreamEventType
from ai_gateway_core.exceptions import ValidationFailedError

from src.adapters.langgraph import LangGraphAdapter
from src.models.request import ContentItem, UnifiedRequest

CASES = json.loads((Path(__file__).parents[1] / "fixtures/adapters/langgraph_stream_terminal.json").read_text())


class TrackedBody(httpx.AsyncByteStream):
    def __init__(self, parts):
        self.parts = parts
        self.closed = False

    async def __aiter__(self):
        for part in self.parts:
            yield part

    async def aclose(self):
        self.closed = True


def adapter_for(client):
    # Keep the actual public stream and request-building implementation. Only the
    # connector and trusted service configuration are supplied by the fixture.
    adapter = object.__new__(LangGraphAdapter)
    adapter.remote = True
    adapter.config = {}
    adapter.service = SimpleNamespace(session_enabled=False)
    adapter.assistant_id = "fixture-agent"
    adapter.stream_endpoint = "/runs/stream"
    adapter.connector = SimpleNamespace(_client=client)
    adapter.forward_auth = True
    adapter.auth_user_id = None
    adapter.auth_user_type = "user"
    return adapter


@pytest.mark.asyncio
@pytest.mark.parametrize("case", CASES, ids=lambda case: case["name"])
@pytest.mark.parametrize("fragmented", [False, True], ids=["whole", "bytewise"])
async def test_langgraph_remote_terminal_errors(case, fragmented, caplog):
    wire = case["wire"].encode()
    body = TrackedBody([wire[i:i+1] for i in range(len(wire))] if fragmented else [wire])
    calls = []

    def handle(request):
        calls.append(request)
        assert request.method == "POST" and request.url.path == "/runs/stream"
        payload = json.loads(request.content)
        assert payload["assistant_id"] == "fixture-agent"
        assert payload["metadata"]["tenant_id"] == "fixture-tenant"
        return httpx.Response(200, headers={"content-type": "text/event-stream"}, stream=body)

    request = UnifiedRequest("request", "service", [ContentItem(ContentType.TEXT, "Question")],
                             user_id="fixture-user", tenant_id="fixture-tenant")
    async with httpx.AsyncClient(base_url="https://langgraph.invalid", transport=httpx.MockTransport(handle)) as client:
        stream = adapter_for(client).stream(request)
        chunks = []
        if case["error"]:
            with pytest.raises(ValidationFailedError, match="^langgraph_provider_error$"):
                async for chunk in stream:
                    chunks.append(chunk)
        else:
            chunks = [chunk async for chunk in stream]
        with pytest.raises(StopAsyncIteration):
            await anext(stream)
    assert len(calls) == 1
    assert body.closed
    assert "".join(c.content.data for c in chunks if c.event_type == StreamEventType.TEXT_DELTA) == case["text"]
    assert [c.chunk_index for c in chunks] == list(range(len(chunks)))
    finals = [c for c in chunks if c.is_final]
    if case["error"]:
        assert not finals
    else:
        assert len(finals) == 1
        assert (finals[0].metadata or {}).get("usage") == case.get("usage")
    assert "SYNTHETIC_PRIVATE_PROVIDER_DETAIL" not in caplog.text
    assert "SYNTHETIC_PRIVATE_PROVIDER_DETAIL" not in repr(chunks)


@pytest.mark.asyncio
async def test_langgraph_non_200_diagnostics_remain_status_specific():
    request = UnifiedRequest("request", "service", [ContentItem(ContentType.TEXT, "Question")],
                             user_id="fixture-user", tenant_id="fixture-tenant")
    async with httpx.AsyncClient(base_url="https://langgraph.invalid", transport=httpx.MockTransport(
        lambda request: httpx.Response(429, text="bounded synthetic rate limit")
    )) as client:
        with pytest.raises(ValidationFailedError, match=r"LangGraph stream failed \(429\)"):
            _ = [chunk async for chunk in adapter_for(client).stream(request)]
