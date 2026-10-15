"""Network-free embedding provider payload contract tests.

Live provider probes require user-supplied credentials and belong in an
explicitly invoked operational check, not the regular pytest suite.  These
tests retain the provider request/response coverage without logging keys,
making network calls, or changing process-wide TLS configuration.
"""

from __future__ import annotations

import asyncio
import json
import ssl
from types import SimpleNamespace
from typing import Any

import httpx
import pytest
from dashscope import TextEmbedding


def _dashscope_embedding_dimension(response: Any) -> int:
    assert response.status_code == 200, f"DashScope returned {response.status_code}"

    output = getattr(response, "output", None)
    if isinstance(output, dict):
        embeddings = output.get("embeddings", [])
        vector = embeddings[0].get("embedding", []) if embeddings else []
    else:
        embeddings = getattr(output, "embeddings", [])
        vector = embeddings[0].embedding if embeddings else []

    assert vector, "DashScope response did not contain an embedding vector"
    return len(vector)


def _gemini_embedding_dimension(response: httpx.Response) -> int:
    assert response.status_code == 200, f"Gemini returned {response.status_code}"
    vector = response.json().get("embedding", {}).get("values", [])
    assert vector, "Gemini response did not contain an embedding vector"
    return len(vector)


@pytest.mark.asyncio
async def test_dashscope_embedding_payload_is_valid_without_tls_side_effect(monkeypatch) -> None:
    observed: dict[str, object] = {}
    original_ssl_context = ssl._create_default_https_context

    def fake_call(**kwargs: object) -> SimpleNamespace:
        observed.update(kwargs)
        return SimpleNamespace(
            status_code=200,
            output={"embeddings": [{"embedding": [0.1, 0.2, 0.3]}]},
        )

    monkeypatch.setattr(TextEmbedding, "call", fake_call)

    response = await asyncio.to_thread(
        TextEmbedding.call,
        model="text-embedding-v3",
        input=["测试文本 test text"],
        api_key="test-key",
    )

    assert _dashscope_embedding_dimension(response) == 3
    assert observed == {
        "model": "text-embedding-v3",
        "input": ["测试文本 test text"],
        "api_key": "test-key",
    }
    object_response = SimpleNamespace(
        status_code=200,
        output=SimpleNamespace(embeddings=[SimpleNamespace(embedding=[0.4, 0.5])]),
    )
    assert _dashscope_embedding_dimension(object_response) == 2
    assert ssl._create_default_https_context is original_ssl_context


@pytest.mark.asyncio
async def test_gemini_embedding_request_and_payload_are_local() -> None:
    observed: dict[str, object] = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        observed["host"] = request.url.host
        observed["path"] = request.url.path
        observed["api_key"] = request.url.params.get("key")
        observed["body"] = json.loads(request.content)
        return httpx.Response(200, json={"embedding": {"values": [0.1, 0.2, 0.3]}})

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(transport=transport, timeout=5.0) as client:
        response = await client.post(
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-001:embedContent",
            params={"key": "test-key"},
            json={
                "model": "models/gemini-embedding-001",
                "content": {"parts": [{"text": "测试文本 test text"}]},
                "task_type": "RETRIEVAL_DOCUMENT",
            },
        )

    assert _gemini_embedding_dimension(response) == 3
    assert observed == {
        "host": "generativelanguage.googleapis.com",
        "path": "/v1beta/models/gemini-embedding-001:embedContent",
        "api_key": "test-key",
        "body": {
            "model": "models/gemini-embedding-001",
            "content": {"parts": [{"text": "测试文本 test text"}]},
            "task_type": "RETRIEVAL_DOCUMENT",
        },
    }


def test_embedding_payload_validation_rejects_error_responses() -> None:
    with pytest.raises(AssertionError, match="DashScope returned 401"):
        _dashscope_embedding_dimension(SimpleNamespace(status_code=401, output={}))

    with pytest.raises(AssertionError, match="Gemini returned 429"):
        _gemini_embedding_dimension(httpx.Response(429, json={"error": "quota"}))
