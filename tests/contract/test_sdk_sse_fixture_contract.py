from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / "sdk" / "fixtures" / "sse_inner_envelopes.json"


def test_shared_sdk_sse_fixture_defines_inner_envelope_and_terminals() -> None:
    fixture = json.loads(FIXTURE.read_text(encoding="utf-8"))

    assert fixture["text_delta"]["sse"] == (
        'data: {"event_type":"text_delta","data":"Hi"}\n\n'
    )
    assert fixture["text_delta"]["text"] == "Hi"
    assert [
        fixture[name]["event_type"]
        for name in ("done", "error", "cancelled", "run_finished", "run_error")
    ] == [
        "done",
        "error",
        "cancelled",
        "run_finished",
        "run_error",
    ]
    assert {
        name: fixture[name]["value"]
        for name in ("null_data", "number_data", "boolean_data", "array_data")
    } == {
        "null_data": None,
        "number_data": 42,
        "boolean_data": True,
        "array_data": [1, 2],
    }


def test_java_and_dart_native_contracts_consume_shared_fixture() -> None:
    java_test = (
        ROOT / "sdk/java/src/test/java/com/aigateway/ai/SSEParserTest.java"
    ).read_text(encoding="utf-8")
    java_parser = (
        ROOT / "sdk/java/src/main/java/com/aigateway/ai/SSEParser.java"
    ).read_text(encoding="utf-8")
    java_client = (
        ROOT / "sdk/java/src/main/java/com/aigateway/ai/ChatClient.java"
    ).read_text(encoding="utf-8")
    dart_test = (
        ROOT / "sdk/dart/ai_gateway_sdk/test/streaming_test.dart"
    ).read_text(encoding="utf-8")
    dart_parser = (
        ROOT / "sdk/dart/ai_gateway_sdk/lib/src/streaming.dart"
    ).read_text(encoding="utf-8")

    assert "sse_inner_envelopes.json" in java_test
    assert 'payload.remove("data")' in java_parser
    assert "event.isTerminal()" in java_parser
    assert "EventType.ERROR, EventType.RUN_ERROR" in java_client
    assert "if (!unsuccessfulTerminal[0])" in java_client
    assert "sse_inner_envelopes.json" in dart_test
    assert "payload.remove('data')" in dart_parser
    assert "event.isTerminal" in dart_parser
