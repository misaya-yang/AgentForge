"""Task-owned native-compatible storage codec and public result projection.

The private format belongs to trusted task bookkeeping metadata, not result JSON.
Only a domain UnifiedResponse creates that format on writes. Database adapters
must parse JSON text once before decoding, and preserve all legacy JSON types.
"""
from __future__ import annotations

import base64
import math
import re
from datetime import datetime
from typing import Any

from ai_gateway_core.enums import ContentType
from pydantic import TypeAdapter

from ...models.request import ContentItem
from ...models.response import UnifiedResponse

RESULT_FORMAT_KEY = "gateway.native_task_result.v1"
RESULT_FORMAT_ABSENT = object()
_PUBLIC_TIMESTAMP = TypeAdapter(datetime)


class TaskResultCodecError(ValueError):
    """Invalid task result; the message deliberately excludes payload contents."""


class _DecodedTaskResponse(UnifiedResponse):
    # Not a dataclass field: private provenance must never enter public output.
    __slots__ = ("_output_data_types",)


def _invalid() -> TaskResultCodecError:
    return TaskResultCodecError("invalid_task_result")


def _json(value: Any) -> Any:
    """Validate and detach JSON without coercing bytes, keys or special floats."""
    if value is None or type(value) in (str, bool, int):
        return value
    if type(value) is float and math.isfinite(value):
        return value
    if type(value) is list:
        return [_json(item) for item in value]
    if type(value) is dict and all(type(key) is str for key in value):
        return {key: _json(item) for key, item in value.items()}
    raise _invalid()


def _string(value: Any, *, nullable: bool = False) -> str | None:
    if type(value) is str or (nullable and value is None):
        return value
    raise _invalid()


def _mapping(value: Any) -> dict[str, Any] | None:
    if value is None:
        return None
    if type(value) is not dict:
        raise _invalid()
    return _json(value)


def _kind(value: Any) -> str:
    if value is None:
        return "none"
    if type(value) is str:
        return "text"
    if isinstance(value, (bytes, bytearray)):
        return "bytes"
    _json(value)
    return "json"


def _encode_data(value: Any, kind: str, *, public: bool) -> Any:
    if kind == "none" and value is None:
        return None
    if kind == "text" and type(value) is str:
        return value
    if kind == "bytes" and isinstance(value, (bytes, bytearray)):
        return base64.b64encode(value).decode("ascii") if public else list(value)
    if kind == "json":
        return _json(value)
    raise _invalid()


def _encode_response(response: UnifiedResponse, *, public: bool) -> tuple[dict[str, Any], dict[str, Any]]:
    if not isinstance(response.timestamp, datetime) or type(response.outputs) is not list:
        raise _invalid()
    offset = response.timestamp.utcoffset()
    if offset is not None and offset.microseconds:
        # Native chrono FixedOffset cannot represent fractional-second offsets.
        raise _invalid()
    kinds = (
        response._output_data_types
        if isinstance(response, _DecodedTaskResponse)
        else tuple(_kind(item.data) for item in response.outputs)
    )
    if len(kinds) != len(response.outputs):
        raise _invalid()
    outputs = []
    for item, kind in zip(response.outputs, kinds, strict=True):
        if not isinstance(item, ContentItem):
            raise _invalid()
        outputs.append({
            "type": ContentType(item.type).value,
            "data": _encode_data(item.data, kind, public=public),
            "url": _string(item.url, nullable=True),
            "mime_type": _string(item.mime_type, nullable=True),
            "metadata": _mapping(item.metadata),
        })
    timestamp = response.timestamp.isoformat()
    if public:
        # Match the existing API serializer, including sub-minute UTC offsets.
        timestamp = _PUBLIC_TIMESTAMP.dump_python(response.timestamp, mode="json")
    return {
        "request_id": _string(response.request_id),
        "status": _string(response.status),
        "outputs": outputs,
        "session_id": _string(response.session_id, nullable=True),
        "task_id": _string(response.task_id, nullable=True),
        "usage": _mapping(response.usage),
        "error": _mapping(response.error),
        "metadata": _mapping(response.metadata),
        "timestamp": timestamp,
    }, {"output_data_types": list(kinds)}


def encode_task_result(result: Any) -> tuple[Any, dict[str, Any] | None]:
    """Return detached storage JSON and format; None format means clear marker.

    An omitted result patch is the caller's separate concern: do not call this
    function to represent omission. Explicit JSON null is a supplied result.
    """
    try:
        if isinstance(result, UnifiedResponse):
            return _encode_response(result, public=False)
        return _json(result), None
    except (ValueError, TypeError, AttributeError, RecursionError, OverflowError):
        raise _invalid() from None


def _decode_data(value: Any, kind: Any) -> Any:
    if kind == "none" and value is None:
        return None
    if kind == "text" and type(value) is str:
        return value
    if (
        kind == "bytes"
        and type(value) is list
        and all(type(item) is int and 0 <= item <= 255 for item in value)
    ):
        return bytes(value)
    if kind == "json":
        return _json(value)
    raise _invalid()


def decode_task_result(value: Any, format: Any = RESULT_FORMAT_ABSENT) -> Any:
    """Decode already parsed JSON from an authoritative storage row.

    Omit format (or pass RESULT_FORMAT_ABSENT) for unmarked legacy JSON. An
    explicitly null marker is corruption, not absence. Never pass user result
    JSON as the format: it must come from trusted task bookkeeping metadata.
    """
    try:
        value = _json(value)
        if format is RESULT_FORMAT_ABSENT:
            return value
        if type(format) is not dict or type(value) is not dict:
            raise _invalid()
        kinds = format.get("output_data_types")
        raw_outputs = value.get("outputs")
        if type(kinds) is not list or type(raw_outputs) is not list or len(kinds) != len(raw_outputs):
            raise _invalid()
        timestamp = _string(value.get("timestamp"))
        # Python can silently turn +00:00:00.5 into UTC: reject before parsing.
        if re.search(r"[+-][0-9:]+[.,][0-9]+$", timestamp[10:]):
            raise _invalid()
        parsed_timestamp = datetime.fromisoformat(timestamp.replace("Z", "+00:00"))
        offset = parsed_timestamp.utcoffset()
        if offset is not None and offset.microseconds:
            raise _invalid()
        outputs = []
        for raw, kind in zip(raw_outputs, kinds, strict=True):
            if type(raw) is not dict or type(kind) is not str:
                raise _invalid()
            outputs.append(ContentItem(
                type=ContentType(raw.get("type")),
                data=_decode_data(raw.get("data"), kind),
                url=_string(raw.get("url"), nullable=True),
                mime_type=_string(raw.get("mime_type"), nullable=True),
                metadata=_mapping(raw.get("metadata")),
            ))
        response = _DecodedTaskResponse(
            request_id=_string(value.get("request_id")),
            status=_string(value.get("status")),
            outputs=outputs,
            session_id=_string(value.get("session_id"), nullable=True),
            task_id=_string(value.get("task_id"), nullable=True),
            usage=_mapping(value.get("usage")),
            error=_mapping(value.get("error")),
            metadata=_mapping(value.get("metadata")),
            timestamp=parsed_timestamp,
        )
        response._output_data_types = tuple(kinds)
        return response
    except (ValueError, TypeError, AttributeError, RecursionError, OverflowError):
        raise _invalid() from None


def project_task_result(result: Any) -> Any:
    """Produce public JSON, base64-encoding only typed binary output data."""
    try:
        if isinstance(result, UnifiedResponse):
            return _encode_response(result, public=True)[0]
        return _json(result)
    except (ValueError, TypeError, AttributeError, RecursionError, OverflowError):
        raise _invalid() from None
