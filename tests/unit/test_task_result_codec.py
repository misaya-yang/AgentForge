import json
from copy import deepcopy
from dataclasses import fields
from datetime import datetime, timedelta, timezone

import pytest
from ai_gateway_core.enums import ContentType

from src.api.schemas.response import UnifiedResponseSchema
from src.models.request import ContentItem
from src.models.response import UnifiedResponse
from src.services.task.task_result_codec import (
    RESULT_FORMAT_ABSENT,
    RESULT_FORMAT_KEY,
    TaskResultCodecError,
    decode_task_result,
    encode_task_result,
    project_task_result,
)


def response(data=b"\x00\xff", timestamp=None):
    return UnifiedResponse(
        request_id="fixture-request", status="success",
        outputs=[ContentItem(ContentType.IMAGE, data, "https://fixture.invalid/result", "image/png", {"item": [1]})],
        session_id="fixture-session", task_id="fixture-task",
        usage={"tokens": 4}, error={"code": "fixture"},
        metadata={"response": ["keep"], RESULT_FORMAT_KEY: {"user": "ordinary response metadata"}},
        timestamp=timestamp or datetime(2026, 10, 12, 12, 30, 1, 123456),
    )


@pytest.mark.parametrize("data", [None, "", "text", b"", b"\x00\xff", bytearray(b"\x00\xff"), [], [0, 255], {"nested": [False, 0, None]}, False, 0, 1.25])
def test_typed_roundtrip_and_actual_api_projection(data):
    original = response(data)
    stored, format = encode_task_result(original)
    restored = decode_task_result(json.loads(json.dumps(stored)), format)
    assert isinstance(restored, UnifiedResponse)
    assert encode_task_result(restored) == (stored, format)
    assert project_task_result(restored) == UnifiedResponseSchema.from_domain(original).model_dump(mode="json")
    assert {field.name for field in fields(restored)} == {field.name for field in fields(original)}
    assert "_output_data_types" not in project_task_result(restored)
    assert stored["metadata"] == original.metadata
    if isinstance(data, (bytes, bytearray)):
        assert stored["outputs"][0]["data"] == list(data)
        assert isinstance(restored.outputs[0].data, bytes)
    original.outputs[0].metadata["item"].append(2)
    assert stored["outputs"][0]["metadata"] == {"item": [1]}


@pytest.mark.parametrize("stamp", [
    datetime(2026, 10, 12), datetime(2026, 10, 12, 1, 2, 3, 1),
    datetime(2026, 10, 12, tzinfo=timezone.utc),
    datetime(2026, 10, 12, tzinfo=timezone(timedelta(hours=5, minutes=30))),
    datetime(2026, 10, 12, tzinfo=timezone(timedelta(hours=-4))),
    datetime(2026, 10, 12, tzinfo=timezone(timedelta(seconds=30))),
    datetime(2026, 10, 12, tzinfo=timezone(timedelta(hours=-5, seconds=-30))),
])
def test_timestamp_preservation_and_public_schema(stamp):
    original = response(timestamp=stamp)
    encoded, marker = encode_task_result(original)
    restored = decode_task_result(encoded, marker)
    assert restored.timestamp == stamp
    assert restored.timestamp.utcoffset() == stamp.utcoffset()
    assert project_task_result(restored) == UnifiedResponseSchema.from_domain(original).model_dump(mode="json")


@pytest.mark.parametrize("legacy", [None, False, 0, 0.0, "", "null", "[1]", [], [1, 2], {}, {"outputs": [], "request_id": "pretend", "status": "success"}, {RESULT_FORMAT_KEY: {"output_data_types": []}}])
def test_legacy_never_promoted(legacy):
    encoded, marker = encode_task_result(legacy)
    assert encoded == legacy and marker is None
    assert decode_task_result(encoded) == legacy
    assert decode_task_result(encoded, RESULT_FORMAT_ABSENT) == legacy
    assert project_task_result(legacy) == legacy
    assert not isinstance(decode_task_result(encoded), UnifiedResponse)


@pytest.mark.parametrize("kind,data", [("json", "text"), ("json", None), ("json", [0, 255]), ("json", False), ("text", "AP8="), ("none", None), ("bytes", [0, 255])])
def test_native_fixture_kinds_survive_decode_encode(kind, data):
    stored, _ = encode_task_result(response())
    stored["outputs"][0]["data"] = data
    format = {"output_data_types": [kind]}
    restored = decode_task_result(stored, format)
    assert encode_task_result(restored) == (stored, format)
    projected = project_task_result(restored)
    assert projected == UnifiedResponseSchema.from_domain(restored).model_dump(mode="json")
    assert projected["outputs"][0]["data"] == ("AP8=" if kind == "bytes" else data)


@pytest.mark.parametrize("marker", [None, {}, [], "v1", {"output_data_types": None}, {"output_data_types": []}, {"output_data_types": ["future"]}, {"output_data_types": [1]}, {"output_data_types": ["bytes", "bytes"]}])
def test_corrupt_marker_fails_closed(marker):
    stored, _ = encode_task_result(response())
    with pytest.raises(TaskResultCodecError, match="^invalid_task_result$"):
        decode_task_result(stored, marker)


@pytest.mark.parametrize("data", [[True], [False], [-1], [256], [1.0], ["1"], "AP8=", {}, None])
def test_bytes_are_strict_integer_arrays(data):
    stored, marker = encode_task_result(response())
    stored["outputs"][0]["data"] = data
    with pytest.raises(TaskResultCodecError):
        decode_task_result(stored, marker)


@pytest.mark.parametrize("key,value", [
    ("request_id", None), ("status", 1), ("outputs", None),
    ("timestamp", None), ("timestamp", "not-a-time"), ("timestamp", "2026-02-30"),
    ("usage", []), ("metadata", "secret-payload"), ("session_id", 12),
])
def test_corrupt_response_fields_fail_redacted(key, value):
    stored, marker = encode_task_result(response())
    stored[key] = value
    with pytest.raises(TaskResultCodecError) as error:
        decode_task_result(stored, marker)
    assert str(error.value) == "invalid_task_result"


@pytest.mark.parametrize("field,value", [("type", "future"), ("url", 1), ("mime_type", []), ("metadata", [1])])
def test_corrupt_content_fields_fail(field, value):
    stored, marker = encode_task_result(response())
    stored["outputs"][0][field] = value
    with pytest.raises(TaskResultCodecError):
        decode_task_result(stored, marker)


@pytest.mark.parametrize("value", [b"raw", bytearray(b"raw"), datetime(2026, 1, 1), float("nan"), float("inf"), float("-inf"), {1: "bad-key"}, (1, 2), {"nested": b"bad"}])
def test_non_json_values_are_not_coerced(value):
    for action in (encode_task_result, decode_task_result, project_task_result):
        with pytest.raises(TaskResultCodecError):
            action(value)
    original = response()
    original.metadata = {"nested": value}
    with pytest.raises(TaskResultCodecError):
        encode_task_result(original)


def test_no_aliases_or_metadata_marker_authority():
    stored, marker = encode_task_result(response())
    stored[RESULT_FORMAT_KEY] = marker
    raw, raw_marker = encode_task_result(stored)
    assert raw_marker is None
    assert isinstance(decode_task_result(raw), dict)
    raw["outputs"][0]["metadata"]["item"].append(99)
    assert stored["outputs"][0]["metadata"]["item"] == [1]
    restored = decode_task_result(stored, marker)
    marker["output_data_types"][0] = "json"
    assert encode_task_result(restored)[1]["output_data_types"] == ["bytes"]


def test_private_provenance_is_checked_after_mutation_and_deepcopy():
    stored, marker = encode_task_result(response())
    restored = deepcopy(decode_task_result(stored, marker))
    assert encode_task_result(restored) == (stored, marker)
    restored.outputs[0].data = [0, 255]
    with pytest.raises(TaskResultCodecError):
        encode_task_result(restored)


def test_cycles_fail_redacted():
    value = []
    value.append(value)
    with pytest.raises(TaskResultCodecError, match="^invalid_task_result$"):
        encode_task_result(value)


def test_missing_required_fields_fail_without_invented_time():
    stored, marker = encode_task_result(response())
    for key in ("timestamp", "request_id", "status", "outputs"):
        invalid = dict(stored)
        invalid.pop(key)
        with pytest.raises(TaskResultCodecError):
            decode_task_result(invalid, marker)


def test_independent_native_fixture_and_no_network_callback_encoding():
    from pathlib import Path

    import httpx

    fixture = json.loads((Path(__file__).parent / "fixtures/task_result_native_v1.json").read_text())
    restored = decode_task_result(fixture["value"], fixture["format"])
    assert encode_task_result(restored) == (fixture["value"], fixture["format"])
    projected = project_task_result(restored)
    assert projected == UnifiedResponseSchema.from_domain(restored).model_dump(mode="json")
    request = httpx.Request("POST", "https://fixture.invalid/callback", json={"result": projected})
    assert json.loads(request.content)["result"]["outputs"][0]["data"] == "AP8="
    assert json.loads(request.content)["result"]["outputs"][1]["data"] == [0, 255]


def test_restored_response_works_with_actual_typed_only_manager():
    import asyncio

    from src.models.task import Task, TaskStatus
    from src.services.task.task_manager import MemoryTaskStorage, TaskManager
    from src.services.task.task_queue import MemoryTaskQueue

    async def check():
        stored, marker = encode_task_result(response())
        restored = decode_task_result(stored, marker)
        storage = MemoryTaskStorage()
        await storage.save(Task("fixture-task", "fixture-request", "fixture-service", TaskStatus.COMPLETED, result=restored))
        manager = TaskManager(storage, MemoryTaskQueue())
        result = await manager.get_result("fixture-task")
        assert project_task_result(result) == project_task_result(restored)

    asyncio.run(check())


@pytest.mark.parametrize("kind,data", [("none", "text"), ("text", None), ("text", []), ("json", b"bad")])
def test_kind_data_disagreement_rejected(kind, data):
    stored, _ = encode_task_result(response())
    stored["outputs"][0]["data"] = data
    with pytest.raises(TaskResultCodecError):
        decode_task_result(stored, {"output_data_types": [kind]})


@pytest.mark.parametrize("micros", [1, -1, 500000, -500000, 30500000, -30500000])
def test_fractional_second_timezone_offsets_rejected_without_changing_instant(micros):
    original = response(timestamp=datetime(2026, 10, 12, tzinfo=timezone(timedelta(microseconds=micros))))
    for action in (encode_task_result, project_task_result):
        with pytest.raises(TaskResultCodecError, match="^invalid_task_result$"):
            action(original)
    stored, marker = encode_task_result(response())
    stored["timestamp"] = original.timestamp.isoformat()
    with pytest.raises(TaskResultCodecError, match="^invalid_task_result$"):
        decode_task_result(stored, marker)
