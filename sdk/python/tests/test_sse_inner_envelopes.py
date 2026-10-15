from __future__ import annotations

import json
from pathlib import Path

import pytest

from ai_assistant.models.events import EventType
from ai_assistant.streaming.sse_parser import SSEParser

FIXTURE_PATH = Path(__file__).resolve().parents[2] / "fixtures" / "sse_inner_envelopes.json"


class _Response:
    def __init__(self, sse: str) -> None:
        self._lines = sse.splitlines()

    async def aiter_lines(self):
        for line in self._lines:
            yield line


async def _parse(sse: str):
    return [event async for event in SSEParser().parse(_Response(sse))]


@pytest.mark.asyncio
async def test_shared_inner_envelope_fixture() -> None:
    fixture = json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))

    text = (await _parse(fixture["text_delta"]["sse"]))[0]
    assert text.event_type == EventType.TEXT_DELTA
    assert text.text == fixture["text_delta"]["text"] == "Hi"

    done = (await _parse(fixture["done"]["sse"]))[0]
    assert done.is_done() and done.is_terminal()

    error = (await _parse(fixture["error"]["sse"]))[0]
    assert error.is_error() and error.is_terminal()
    assert error.data["message"] == fixture["error"]["message"]

    cancelled = (await _parse(fixture["cancelled"]["sse"]))[0]
    assert cancelled.is_cancelled() and cancelled.is_terminal()
    assert cancelled.data["reason"] == fixture["cancelled"]["reason"]

    run_finished = (await _parse(fixture["run_finished"]["sse"]))[0]
    assert run_finished.is_done() and run_finished.is_terminal()

    run_error = (await _parse(fixture["run_error"]["sse"]))[0]
    assert run_error.is_error() and run_error.is_terminal()
    assert run_error.data["message"] == fixture["run_error"]["message"]

    for name in ("null_data", "number_data", "boolean_data", "array_data"):
        event = (await _parse(fixture[name]["sse"]))[0]
        assert event.data == {"value": fixture[name]["value"]}

    v2 = (await _parse(fixture["agent_v2_item"]["sse"]))[0]
    assert v2.event_type == fixture["agent_v2_item"]["event_type"]
    assert v2.data["sequence"] == fixture["agent_v2_item"]["sequence"]
    assert v2.data["event"]["payload"]["payload"]["content"][0]["text"] == "hello"
