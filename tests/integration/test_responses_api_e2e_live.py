"""Opt-in result-level checks for the public OpenAI Responses ingress.

This suite talks to the running Gateway and the configured real model provider.
It is intentionally skipped unless the operator supplies dedicated test-user
credentials and explicitly enables it.
"""

from __future__ import annotations

import contextlib
import json
import os
import time
from typing import Any

import httpx
import pytest

API_BASE_URL = os.getenv("ASSISTANT_E2E_BASE_URL", "http://127.0.0.1:8080").rstrip("/")
RUN_LIVE = os.getenv("RUN_RESPONSES_API_E2E", "0") == "1"
USER_EMAIL = os.getenv("ASSISTANT_E2E_USER1_EMAIL", "")
USER_PASSWORD = os.getenv("ASSISTANT_E2E_PASSWORD", "")
MODEL_ID = os.getenv("ASSISTANT_E2E_MODEL_ID", "qwen3.7-plus")


def _require_live() -> None:
    if not RUN_LIVE:
        pytest.skip("Set RUN_RESPONSES_API_E2E=1 to run live Responses validation")
    if not USER_EMAIL or not USER_PASSWORD:
        pytest.skip("Set dedicated Assistant E2E credentials for live Responses validation")


def _login(client: httpx.Client) -> str:
    response = client.post(
        f"{API_BASE_URL}/api/v1/auth/login",
        json={"email": USER_EMAIL, "password": USER_PASSWORD},
    )
    assert response.status_code == 200, response.text
    token = response.json().get("access_token")
    assert isinstance(token, str) and token
    return token


def _headers(token: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {token}"}


def _output_text(response: dict[str, Any]) -> str:
    chunks: list[str] = []
    for item in response.get("output") or []:
        if not isinstance(item, dict) or item.get("type") != "message":
            continue
        for part in item.get("content") or []:
            if isinstance(part, dict) and part.get("type") == "output_text":
                text = part.get("text")
                if isinstance(text, str):
                    chunks.append(text)
    return "".join(chunks)


@pytest.mark.integration
def test_responses_nonstream_real_provider_completes() -> None:
    _require_live()
    marker = f"LIVE-RESPONSES-{time.time_ns()}"
    with httpx.Client(timeout=240.0, trust_env=False) as client:
        token = _login(client)
        response = client.post(
            f"{API_BASE_URL}/v1/responses",
            headers=_headers(token),
            json={
                "model": MODEL_ID,
                "input": (
                    "Please include this diagnostic identifier in a one-sentence reply: "
                    f"{marker}"
                ),
                "store": False,
                "max_output_tokens": 128,
            },
        )

    assert response.status_code == 200, response.text
    payload = response.json()
    assert payload.get("status") == "completed", payload
    assert marker in _output_text(payload), payload
    assert payload.get("usage", {}).get("total_tokens", 0) > 0, payload


@pytest.mark.integration
def test_responses_stream_real_provider_has_monotonic_single_terminal() -> None:
    _require_live()
    marker = f"LIVE-RESPONSES-STREAM-{time.time_ns()}"
    events: list[dict[str, Any]] = []

    with httpx.Client(timeout=240.0, trust_env=False) as client:
        token = _login(client)
        with client.stream(
            "POST",
            f"{API_BASE_URL}/v1/responses",
            headers=_headers(token),
            json={
                "model": MODEL_ID,
                "input": (
                    "Please include this diagnostic identifier in a one-sentence reply: "
                    f"{marker}"
                ),
                "stream": True,
                "store": False,
                "max_output_tokens": 128,
            },
        ) as response:
            assert response.status_code == 200, response.text
            for line in response.iter_lines():
                if line.startswith(":"):
                    continue
                if not line.startswith("data:"):
                    continue
                with contextlib.suppress(json.JSONDecodeError):
                    event = json.loads(line.removeprefix("data:").strip())
                    if isinstance(event, dict):
                        events.append(event)

    assert events, "Responses stream emitted no JSON events"
    sequences = [event.get("sequence_number") for event in events]
    assert all(isinstance(sequence, int) for sequence in sequences), sequences
    assert sequences == list(range(len(sequences))), sequences
    terminals = [
        event
        for event in events
        if event.get("type") in {"response.completed", "response.failed"}
    ]
    assert len(terminals) == 1, [event.get("type") for event in events]
    terminal = terminals[0]
    assert terminal.get("type") == "response.completed", terminal
    terminal_response = terminal.get("response")
    assert isinstance(terminal_response, dict), terminal
    assert marker in _output_text(terminal_response), terminal_response
