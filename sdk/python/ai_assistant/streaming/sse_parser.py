"""
Server-Sent Events (SSE) parser.

Converts the raw byte stream from an ``httpx`` streaming response into a
sequence of ``StreamEvent`` objects.  Handles:

* ``data: {json}\\n\\n`` — standard single-line payloads
* Multi-line ``data:`` fields (concatenated with ``\\n``)
* ``[DONE]`` sentinel signalling end-of-stream
* ``event:`` field (mapped to ``event_type``)
* Empty keep-alive lines (silently skipped)
"""

from __future__ import annotations

import json
import time
from datetime import datetime
from typing import TYPE_CHECKING

from ai_assistant.exceptions import StreamError
from ai_assistant.models.events import EventType, StreamEvent

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

    import httpx


class SSEParser:
    """Stateless SSE parser that yields ``StreamEvent`` instances."""

    async def parse(self, response: httpx.Response) -> AsyncIterator[StreamEvent]:
        """Iterate over a streaming ``httpx.Response`` and yield events.

        The caller is responsible for closing the response when done.
        """
        buf_event: str | None = None
        buf_data_lines: list[str] = []

        try:
            async for raw_line in response.aiter_lines():
                line = raw_line.rstrip("\r\n")

                # Empty line = event boundary
                if not line:
                    if buf_data_lines:
                        event = self._flush(buf_event, buf_data_lines)
                        if event is not None:
                            yield event
                    buf_event = None
                    buf_data_lines = []
                    continue

                # Field parsing
                if line.startswith("data:"):
                    value = line[5:].lstrip(" ")
                    buf_data_lines.append(value)
                elif line.startswith("event:"):
                    buf_event = line[6:].strip()
                elif line.startswith(":"):
                    # SSE comment / keep-alive — skip
                    continue

            # Flush any trailing event that was not terminated by a blank line
            if buf_data_lines:
                event = self._flush(buf_event, buf_data_lines)
                if event is not None:
                    yield event

        except Exception as exc:
            raise StreamError(
                f"SSE parse failure: {exc}",
                status_code=None,
            ) from exc

    # -- internal ------------------------------------------------------------

    @staticmethod
    def _flush(event_field: str | None, data_lines: list[str]) -> StreamEvent | None:
        """Parse buffered data lines into a ``StreamEvent``, or ``None`` for sentinels."""
        raw = "\n".join(data_lines)

        # [DONE] sentinel
        if raw.strip() == "[DONE]":
            return StreamEvent(event_type=EventType.DONE, data={}, timestamp=time.time())

        # Parse JSON payload
        try:
            payload: dict = json.loads(raw)
        except json.JSONDecodeError:
            # Non-JSON data line — wrap it
            payload = {"raw": raw}

        # Determine event type: explicit ``event:`` field > payload ``event_type`` > "message"
        event_type = (
            event_field
            or payload.pop("event_type", None)
            or payload.pop("event", None)
            or "message"
        )

        timestamp = payload.pop("timestamp", None)
        if timestamp is None:
            timestamp = time.time()

        # Unwrap nested "data" field from gateway's SSE format:
        # {"event_type": "...", "data": {actual_payload}, "timestamp": ...}
        inner = payload.pop("data", payload)
        if isinstance(inner, str):
            data = {"content": inner}
        elif isinstance(inner, dict):
            data = inner
        else:
            data = {"value": inner}

        if isinstance(timestamp, str):
            try:
                timestamp = datetime.fromisoformat(timestamp.replace("Z", "+00:00")).timestamp()
            except ValueError:
                timestamp = time.time()
        return StreamEvent(
            event_type=event_type,
            data=data,
            timestamp=float(timestamp),
        )
