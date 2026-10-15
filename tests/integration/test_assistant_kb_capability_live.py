"""Result-level live Assistant×Knowledge lifecycle checks.

This test creates only temporary, private datasets and always attempts to
delete them. It is opt-in because it calls the configured embedding and chat
providers.
"""

from __future__ import annotations

import contextlib
import json
import os
import time
import uuid
from typing import Any

import httpx
import pytest

API_BASE_URL = os.getenv("ASSISTANT_E2E_BASE_URL", "http://127.0.0.1:8080").rstrip("/")
API_PREFIX = f"{API_BASE_URL}/api/v1"
RUN_LIVE = os.getenv("RUN_ASSISTANT_KB_CAPABILITY", "0") == "1"
LOGIN_PASSWORD = os.getenv("ASSISTANT_E2E_PASSWORD", "")
USER2_PASSWORD = os.getenv("ASSISTANT_E2E_USER2_PASSWORD", LOGIN_PASSWORD)
AUTH_EMAIL_DOMAIN = os.getenv("ASSISTANT_E2E_AUTH_EMAIL_DOMAIN", "example.com")
USER1_EMAIL = os.getenv("ASSISTANT_E2E_USER1_EMAIL", f"assistant.e2e.user1@{AUTH_EMAIL_DOMAIN}")
USER2_EMAIL = os.getenv("ASSISTANT_E2E_USER2_EMAIL", f"assistant.e2e.user2@{AUTH_EMAIL_DOMAIN}")
MODEL_ID = os.getenv("ASSISTANT_E2E_MODEL_ID", "qwen3.7-plus")
REPETITIONS = int(os.getenv("ASSISTANT_CAPABILITY_REPETITIONS", "3"))


def _headers(token: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {token}"}


def _login(client: httpx.Client, email: str, password: str = LOGIN_PASSWORD) -> str:
    response = client.post(
        f"{API_PREFIX}/auth/login",
        json={"email": email, "password": password},
    )
    assert response.status_code == 200, response.text
    token = response.json().get("access_token")
    assert isinstance(token, str) and token
    return token


def _create_session(client: httpx.Client, token: str) -> str:
    response = client.post(
        f"{API_PREFIX}/assistant/sessions",
        headers=_headers(token),
        json={},
    )
    assert response.status_code == 200, response.text
    session_id = response.json().get("session_id")
    assert isinstance(session_id, str) and session_id
    return session_id


def _stream_chat(
    client: httpx.Client,
    token: str,
    session_id: str,
    message: str,
    dataset_id: str,
) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    with client.stream(
        "POST",
        f"{API_PREFIX}/assistant/chat/stream",
        headers=_headers(token),
        json={
            "session_id": session_id,
            "message": message,
            "model_id": MODEL_ID,
            "execution_profile": "safe",
            "memory_mode": "strict",
            "kb_mode": "auto",
            "kb_dataset_ids": [dataset_id],
            "kb_top_k": 3,
            "kb_score_threshold": 0.0,
        },
        timeout=240.0,
    ) as response:
        assert response.status_code == 200, response.text
        for line in response.iter_lines():
            if not line or not line.startswith("data: "):
                continue
            with contextlib.suppress(json.JSONDecodeError):
                event = json.loads(line[6:].strip())
                if isinstance(event, dict):
                    events.append(event)
                    # ``done`` is only the model-transport terminal. Keep the
                    # connection open until the durable run reaches its
                    # authoritative terminal so persistence failures cannot be
                    # mistaken for a successful KB answer.
                    if event.get("event_type") in {"run_finished", "run_error"}:
                        break
    return events


def _stream_text(events: list[dict[str, Any]]) -> str:
    chunks: list[str] = []
    for event in events:
        if event.get("event_type") not in {"text_delta", "text_message_content"}:
            continue
        data = event.get("data")
        if isinstance(data, str):
            chunks.append(data)
        elif isinstance(data, dict):
            for key in ("content", "message", "delta"):
                value = data.get(key)
                if isinstance(value, str):
                    chunks.append(value)
                    break
    return "".join(chunks).strip()


def _context_chunks(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    chunks: list[dict[str, Any]] = []
    for event in events:
        if event.get("event_type") != "context_retrieved":
            continue
        data = event.get("data")
        if not isinstance(data, dict):
            continue
        chunks.extend(chunk for chunk in data.get("chunks") or [] if isinstance(chunk, dict))
    return chunks


def _documents(payload: Any) -> list[dict[str, Any]]:
    if isinstance(payload, list):
        return [item for item in payload if isinstance(item, dict)]
    if isinstance(payload, dict) and isinstance(payload.get("documents"), list):
        return [item for item in payload["documents"] if isinstance(item, dict)]
    return []


def _wait_for_document(
    client: httpx.Client,
    token: str,
    dataset_id: str,
    document_id: str,
    *,
    timeout_seconds: float = 120.0,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout_seconds
    last_document: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        response = client.get(
            f"{API_PREFIX}/knowledge/{dataset_id}/documents",
            headers=_headers(token),
        )
        if response.status_code == 409:
            time.sleep(0.25)
            continue
        assert response.status_code == 200, response.text
        last_document = next(
            (
                document
                for document in _documents(response.json())
                if str(document.get("document_id")) == document_id
            ),
            None,
        )
        if last_document and last_document.get("status") == "completed":
            return last_document
        if last_document and last_document.get("status") == "failed":
            raise AssertionError(f"knowledge ingestion failed: {last_document!r}")
        time.sleep(1.0)
    raise AssertionError(f"knowledge ingestion timed out: {last_document!r}")


@pytest.mark.integration
@pytest.mark.parametrize("trial", range(1, REPETITIONS + 1))
def test_assistant_kb_lifecycle_is_grounded_and_tenant_isolated(trial: int) -> None:
    if not RUN_LIVE:
        pytest.skip("Set RUN_ASSISTANT_KB_CAPABILITY=1 to run live Assistant×KB validation")
    if not LOGIN_PASSWORD:
        pytest.skip("Set ASSISTANT_E2E_PASSWORD to run live Assistant×KB validation")
    assert 1 <= REPETITIONS <= 10

    dataset_id = f"capability-{uuid.uuid4().hex[:20]}"
    marker = f"AURORA-REF-{trial}-{uuid.uuid4().hex[:10]}"
    document_id = ""
    with httpx.Client(timeout=60.0) as client:
        token1 = _login(client, USER1_EMAIL)
        token2 = _login(client, USER2_EMAIL, USER2_PASSWORD)
        created = False
        try:
            create_response = client.post(
                f"{API_PREFIX}/knowledge/datasets",
                headers=_headers(token1),
                json={
                    "dataset_id": dataset_id,
                    "name": f"Capability dataset {trial}",
                    "description": "Temporary result-level Assistant KB validation",
                    "visibility": "private",
                },
            )
            assert create_response.status_code == 200, create_response.text
            assert str(create_response.json().get("dataset_id")) == dataset_id
            created = True

            document_response = client.post(
                f"{API_PREFIX}/knowledge/{dataset_id}/documents/text",
                headers=_headers(token1),
                json={
                    "title": "Aurora quarterly logistics record",
                    "content": (
                        "Aurora quarterly logistics record for Q3 2026. The policy reference ID "
                        f"is {marker}. The approved assembly location is Cedar Atrium, and the "
                        "local escalation time is 07:40."
                    ),
                    "metadata": {"source_document": "aurora-policy.md", "capability_trial": trial},
                },
            )
            assert document_response.status_code == 200, document_response.text
            document_id = str(document_response.json().get("document_id") or "")
            assert document_id
            _wait_for_document(client, token1, dataset_id, document_id)

            direct_retrieval = client.post(
                f"{API_PREFIX}/knowledge/{dataset_id}/retrieve",
                headers=_headers(token1),
                json={
                    "query": "What is the policy reference ID in the Aurora Q3 logistics record?",
                    "top_k": 3,
                    "mode": "hybrid",
                    "score_threshold": 0.0,
                },
            )
            assert direct_retrieval.status_code == 200, direct_retrieval.text
            direct_results = direct_retrieval.json().get("results") or []
            assert direct_results, direct_retrieval.json()
            grounded = next(
                (
                    result
                    for result in direct_results
                    if isinstance(result, dict) and marker in str(result.get("text") or "")
                ),
                None,
            )
            assert grounded is not None, direct_results
            assert str(grounded.get("document_id")) == document_id
            segment_id = str(grounded.get("segment_id") or "")
            assert segment_id

            forbidden = client.post(
                f"{API_PREFIX}/knowledge/{dataset_id}/retrieve",
                headers=_headers(token2),
                json={"query": marker, "top_k": 3, "mode": "hybrid"},
            )
            assert forbidden.status_code == 403, forbidden.text

            session_id = _create_session(client, token1)
            first_events = _stream_chat(
                client,
                token1,
                session_id,
                "According to the selected Aurora quarterly logistics record, what is the policy "
                "reference ID? Reply with the exact ID only.",
                dataset_id,
            )
            assert all(event.get("event_type") != "run_error" for event in first_events), (
                first_events
            )
            assert sum(
                event.get("event_type") == "run_finished" for event in first_events
            ) == 1, first_events
            assert _stream_text(first_events) == marker, first_events
            cited_chunks = _context_chunks(first_events)
            matching_chunk = next(
                (
                    chunk
                    for chunk in cited_chunks
                    if chunk.get("dataset_id") == dataset_id
                    and str(chunk.get("document_id")) == document_id
                    and str(chunk.get("segment_id")) == segment_id
                    and marker in str(chunk.get("content") or "")
                ),
                None,
            )
            assert matching_chunk is not None, cited_chunks

            follow_events = _stream_chat(
                client,
                token1,
                session_id,
                "What was the policy reference ID in that same Aurora record? Repeat only the "
                "exact ID.",
                dataset_id,
            )
            assert all(event.get("event_type") != "run_error" for event in follow_events), (
                follow_events
            )
            assert sum(
                event.get("event_type") == "run_finished" for event in follow_events
            ) == 1, follow_events
            assert _stream_text(follow_events) == marker, follow_events
        finally:
            if created:
                delete_response = client.request(
                    "DELETE",
                    f"{API_PREFIX}/knowledge/datasets/{dataset_id}",
                    headers=_headers(token1),
                    json={
                        "password": LOGIN_PASSWORD,
                        "reason": "Assistant KB capability test cleanup",
                    },
                    timeout=120.0,
                )
                assert delete_response.status_code == 200, delete_response.text
                assert delete_response.json().get("status") == "success"
                read_back = client.get(
                    f"{API_PREFIX}/knowledge/datasets/{dataset_id}",
                    headers=_headers(token1),
                )
                assert read_back.status_code in {403, 404}, read_back.text
                retrieve_after_delete = client.post(
                    f"{API_PREFIX}/knowledge/{dataset_id}/retrieve",
                    headers=_headers(token1),
                    json={"query": marker, "top_k": 3, "mode": "hybrid"},
                )
                assert retrieve_after_delete.status_code in {400, 403, 404}, (
                    retrieve_after_delete.text
                )
