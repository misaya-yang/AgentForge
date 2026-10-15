from __future__ import annotations

import copy
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import httpx
import pytest
from ai_gateway_core.enums import (
    ContentType,
    InvocationMode,
    ServiceType,
    TransportType,
)
from fastapi.encoders import jsonable_encoder

from src.adapters.langgraph import LangGraphAdapter
from src.api.schemas.response import StreamChunkSchema
from src.api.v1.sessions import get_history
from src.core.auth.user_resolver import UserContext
from src.core.gateway.dispatcher import GatewayDispatcher
from src.models.request import ContentItem, UnifiedRequest
from src.models.service import ServiceDefinition
from src.services.session.session_manager import SessionManager


class TrackedBody(httpx.AsyncByteStream):
    def __init__(self, wire, fragmented):
        self.wire = wire.encode("utf-8")
        self.fragmented = fragmented
        self.closed = False

    async def __aiter__(self):
        if self.fragmented:
            for byte in self.wire:
                yield bytes([byte])
        else:
            yield self.wire

    async def aclose(self):
        self.closed = True


async def run(case, mode="messages", fragmented=False):
    service = ServiceDefinition(
        service_id="fixture",
        name="Fixture",
        service_type=ServiceType.CUSTOM,
        connector_type=TransportType.HTTP,
        supported_modes=[InvocationMode.STREAM],
        accepted_content_types=[ContentType.TEXT],
        output_content_types=[ContentType.TEXT],
        session_enabled=True,
    )
    # Avoid external LangGraph thread creation; direct stream endpoint still carries scoped config.
    adapter = object.__new__(LangGraphAdapter)
    adapter.remote = True
    adapter.config = {}
    adapter.service = SimpleNamespace(session_enabled=False)
    adapter.assistant_id = "fixture-agent"
    adapter.stream_endpoint = "/runs/stream"
    adapter.forward_auth = True
    adapter.auth_user_id = None
    adapter.auth_user_type = "user"
    events = [
        {"event": "updates", "data": {"tools": {"messages": [m]}}}
        if mode == "updates" and m["type"] == "tool"
        else {"event": "messages", "data": [m, {}]}
        for m in case["messages"]
    ]
    wire = (
        "".join(
            "event: " + e["event"] + "\ndata: " + json.dumps(e["data"], ensure_ascii=False) + "\n\n"
            for e in events
        )
        + "event: end\ndata: {}\n\n"
    )
    body = TrackedBody(wire, fragmented)
    requests = []

    def transport(req):
        requests.append(req)
        assert req.url.path == "/runs/stream"
        return httpx.Response(
            200, headers={"content-type": "text/event-stream"}, stream=body
        )

    registry = MagicMock()
    registry.get = AsyncMock(return_value=service)
    registry.get_adapter.return_value = adapter
    validator = MagicMock()
    validator.validate = AsyncMock()
    limiter = MagicMock()
    limiter.enforce = AsyncMock()
    sessions = SessionManager()
    dispatcher = GatewayDispatcher(
        registry, validator, limiter, MagicMock(), MagicMock(), sessions
    )
    req = UnifiedRequest(
        request_id="request-" + case["name"],
        service_id="fixture",
        user_id="user",
        tenant_id="tenant",
        inputs=[ContentItem(ContentType.TEXT, "Question")],
    )
    async with httpx.AsyncClient(
        base_url="https://fixture.invalid", transport=httpx.MockTransport(transport)
    ) as client:
        adapter.connector = SimpleNamespace(_client=client)
        with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
            chunks = [
                json.loads(StreamChunkSchema.from_domain(c).model_dump_json())
                async for c in dispatcher.stream(req, ["user"])
            ]
    assert len(requests) == 1
    assert body.closed
    history = await get_history(
        req.session_id,
        limit=50,
        session_manager=sessions,
        user=UserContext(user_id="user", tenant_id="tenant"),
    )
    return {**case, "chunks": chunks, "session": {"history": jsonable_encoder(history)}}


FIXTURE = Path(__file__).parents[1] / "fixtures/adapters/langchain_tool_roundtrip.json"
CASES = json.loads(FIXTURE.read_text())["cases"]


@pytest.mark.asyncio
@pytest.mark.parametrize("case", CASES, ids=lambda case: case["name"])
@pytest.mark.parametrize("mode", ["messages", "updates"])
async def test_langchain_tool_roundtrip(case, mode):
    result = await run(case, mode)
    expected = "error" if case["status"] == "error" else "completed"
    message = next(m for m in case["messages"] if m["type"] == "tool")
    tool_chunk = next(c for c in result["chunks"] if c["event_type"] == "tool_result")
    assert tool_chunk["tool_call"]["status"] == expected
    assert tool_chunk["content"]["data"] == message["content"]
    assistants = [m for m in result["session"]["history"] if m["role"] == "assistant"]
    assert len(assistants) == 1
    tool = assistants[0]["metadata"]["tool_calls"][0]
    assert tool["status"] == expected
    assert tool["result"] == message["content"]
    if case["name"].startswith(("tool_only", "result_only")):
        assert assistants[0]["content"] == ""
    assert result["chunks"][-1]["event_type"] == "stream_end"


@pytest.mark.parametrize("status", ["success", "error"])
def test_local_tool_message_outcome(status):
    adapter = object.__new__(LangGraphAdapter)
    output = SimpleNamespace(status=status, content="")
    event = adapter._extract_stream_event(
        {
            "event": "on_tool_end",
            "run_id": "run",
            "name": "lookup",
            "data": {"output": output},
        },
        set(),
    )
    assert event.tool_call.status == ("error" if status == "error" else "completed")
    assert event.text == ""


@pytest.mark.asyncio
async def test_absent_tool_result_stays_distinct_from_empty_success():
    case = {**CASES[0], "messages": CASES[0]["messages"][:1]}
    result = await run(case)
    assistant = next(
        m for m in result["session"]["history"] if m["role"] == "assistant"
    )
    tool = assistant["metadata"]["tool_calls"][0]
    assert tool["result"] is None
    assert tool["status"] == "running"
    assert assistant["content"] == ""


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["messages", "updates"])
@pytest.mark.parametrize("fragmented", [False, True], ids=["whole", "bytewise"])
@pytest.mark.parametrize("case_index", [0, 1], ids=["success", "handled_tool_error"])
async def test_unicode_tool_roundtrip_survives_http_fragmentation(mode, fragmented, case_index):
    # Reuse official LangChain-generated message shapes; only fixture data changes.
    case = copy.deepcopy(CASES[case_index])
    query = "查天气 café 🚀 e\u0301"
    result_text = "北京晴天 🌤️ café e\u0301"
    final_text = "已查询：北京 🌍 café e\u0301"
    case["messages"][0]["tool_calls"][0]["args"]["query"] = query
    case["messages"][1]["content"] = result_text
    case["messages"][2]["content"] = final_text
    partial = copy.deepcopy(case["messages"][2])
    partial["content"] = "已查询："
    case["messages"].insert(2, partial)
    result = await run(case, mode, fragmented)
    chunks = result["chunks"]
    expected_status = "error" if case["status"] == "error" else "completed"
    tool = next(c for c in chunks if c["event_type"] == "tool_result")
    assert tool["content"]["data"] == result_text
    assert tool["tool_call"]["status"] == expected_status
    assert "".join(c["content"]["data"] for c in chunks if c["event_type"] == "text_delta") == final_text
    assistants = [m for m in result["session"]["history"] if m["role"] == "assistant"]
    assert len(assistants) == 1
    assert assistants[0]["content"] == final_text
    saved = assistants[0]["metadata"]["tool_calls"][0]
    assert saved["result"] == result_text
    assert json.loads(saved["arguments"]) == {"query": query}
    assert saved["status"] == expected_status
    assert sum(c["is_final"] for c in chunks) == 1
    assert chunks[-1]["event_type"] == "stream_end"
