"""Text-only choice-zero QA provider outcomes; no transport, credentials or retry."""

import json
import math
from typing import NamedTuple

MAX_EVENT_BYTES = 4 * 1024 * 1024


def _reject_constant(_value: str):
    raise ValueError("qa_stream_invalid")


def _validate_values(value) -> None:
    # serde_json rejects unpaired escaped surrogates. Match it before publishing
    # any field; validating only answer text would leave malformed metadata valid.
    pending = [value]
    while pending:
        item = pending.pop()
        if isinstance(item, str):
            item.encode("utf-8")
        elif isinstance(item, (float, int)) and not math.isfinite(item):
            raise ValueError("qa_stream_invalid")
        elif isinstance(item, list):
            pending.extend(item)
        elif isinstance(item, dict):
            pending.extend(item.keys())
            pending.extend(item.values())


class ProviderEvent(NamedTuple):
    content: str | None = None
    tokens: int | None = None
    closed: bool = False


class QaProviderStream:
    def __init__(self) -> None:
        self._data: list[str] = []
        self._data_bytes = 0
        self._stopped = False
        self._closed = False

    def line(self, line: str) -> ProviderEvent | None:
        if self._closed:
            raise ValueError("qa_stream_after_terminal")
        if len(line.encode("utf-8")) > MAX_EVENT_BYTES:
            raise ValueError("qa_stream_event_too_large")
        if not line:
            return self._dispatch()
        if line.startswith(":"):
            return None
        if line.startswith("data:"):
            data = line[5:]
            if data.startswith(" "):
                data = data[1:]
            self._data_bytes += len(data.encode("utf-8")) + 1
            if self._data_bytes > MAX_EVENT_BYTES:
                raise ValueError("qa_stream_event_too_large")
            self._data.append(data)
            return None
        if line.startswith(("event:", "id:", "retry:")):
            return None
        if self._data:
            raise ValueError("qa_stream_invalid")
        return self._parse(line.strip())

    def finish(self) -> ProviderEvent | None:
        event = self._dispatch()
        if not self._stopped:
            raise ValueError("qa_stream_incomplete")
        return event

    def _dispatch(self) -> ProviderEvent | None:
        data = "\n".join(self._data).strip()
        self._data = []
        self._data_bytes = 0
        return self._parse(data) if data else None

    def _parse(self, raw: str) -> ProviderEvent:
        if raw == "[DONE]":
            if not self._stopped:
                raise ValueError("qa_stream_incomplete")
            self._closed = True
            return ProviderEvent(closed=True)
        try:
            value = json.loads(raw, parse_constant=_reject_constant)
            _validate_values(value)
        except (json.JSONDecodeError, ValueError, UnicodeEncodeError, OverflowError) as exc:
            raise ValueError("qa_stream_invalid") from exc
        if not isinstance(value, dict):
            raise ValueError("qa_stream_invalid")
        if value.get("error") is not None:
            raise ValueError("qa_stream_provider_error")
        usage = value.get("usage")
        tokens = None
        if usage is not None:
            if not isinstance(usage, dict):
                raise ValueError("qa_stream_invalid")
            tokens = usage.get("total_tokens")
            if tokens is not None and (type(tokens) is not int or not 0 <= tokens <= 2**64 - 1):
                raise ValueError("qa_stream_invalid")
        choices = value.get("choices")
        if not isinstance(choices, list):
            raise ValueError("qa_stream_invalid")
        if not choices:
            return ProviderEvent(tokens=tokens)
        if len(choices) != 1 or not isinstance(choices[0], dict):
            raise ValueError("qa_stream_invalid")
        choice = choices[0]
        if "index" in choice and (type(choice["index"]) is not int or choice["index"] != 0):
            raise ValueError("qa_stream_invalid")
        delta = choice.get("delta")
        if not isinstance(delta, dict):
            raise ValueError("qa_stream_invalid")
        if delta.get("tool_calls") is not None or delta.get("function_call") is not None:
            raise ValueError("qa_stream_unsupported_finish")
        content = delta.get("content")
        if content is not None and not isinstance(content, str):
            raise ValueError("qa_stream_invalid")
        if self._stopped and content:
            raise ValueError("qa_stream_after_terminal")
        reason = choice.get("finish_reason")
        if reason is not None:
            if reason != "stop":
                raise ValueError("qa_stream_unsupported_finish")
            self._stopped = True
        return ProviderEvent(content=content, tokens=tokens)
