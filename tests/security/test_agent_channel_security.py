from __future__ import annotations

import asyncio
import re
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient
from pydantic import ValidationError
from starlette.requests import Request

from src.api.schemas.agents import AgentChannelPolicy
from src.api.v1 import agent_public
from src.api.v1.agent_public import document_router
from src.api.v1.agent_runtime import RedisAgentChannelLimiter

ROOT = Path(__file__).resolve().parents[2]
PUBLIC_ID = "44444444-4444-4444-8444-444444444444"


class _DescriptorRepository:
    def __init__(self, origins: list[str] | None = None) -> None:
        self.origins = origins or ["https://allowed.example", "https://second.example:8443"]
        self.status = "active"
        self.channel = "embed"

    async def get_publication_channel(self, **_kwargs: Any) -> dict[str, Any]:
        if self.status != "active":
            from ai_gateway_core.persistence.repositories.agent_repository import (
                AgentRuntimeUnavailableError,
            )

            raise AgentRuntimeUnavailableError("PUBLICATION_DISABLED")
        return {
            "publication_id": "33333333-3333-4333-8333-333333333333",
            "public_id": PUBLIC_ID,
            "channel": self.channel,
            "auth_mode": "public",
            "status": self.status,
            "name": "Secure Support",
            "description": "Public-safe answers",
            "identity": {},
            "policy": {"allowed_origins": self.origins},
        }


@pytest.fixture
def embed_client(monkeypatch: pytest.MonkeyPatch) -> tuple[TestClient, _DescriptorRepository]:
    monkeypatch.setenv("AGENT_RUNTIME_TOKEN_SIGNING_KEY", "embed-test-secret-value")
    app = FastAPI()
    repository = _DescriptorRepository()
    app.state.agent_repository = repository

    @app.middleware("http")
    async def _headers(request: Request, call_next: Any):
        response = await call_next(request)
        if not request.url.path.startswith("/embed/agents/"):
            response.headers["X-Frame-Options"] = "DENY"
        else:
            if "X-Frame-Options" in response.headers:
                del response.headers["X-Frame-Options"]
        return response

    app.include_router(document_router)
    return TestClient(app), repository


def test_embed_document_uses_exact_publication_csp_and_no_x_frame_options(
    embed_client: tuple[TestClient, _DescriptorRepository],
) -> None:
    client, _repository = embed_client
    response = client.get(
        f"/embed/agents/{PUBLIC_ID}",
        headers={"Origin": "https://allowed.example"},
    )
    assert response.status_code == 200
    assert "X-Frame-Options" not in response.headers
    csp = response.headers["Content-Security-Policy"]
    assert (
        "frame-ancestors https://allowed.example https://second.example:8443" in csp
    )
    assert "default-src 'none'" in csp
    assert "object-src 'none'" in csp
    assert "data-embed-token" in response.text
    assert "agent-embed.js" in response.text
    assert "agent-embed.css" in response.text


def test_embed_rejects_missing_wrong_and_wildcard_origins(
    embed_client: tuple[TestClient, _DescriptorRepository],
) -> None:
    client, repository = embed_client
    missing = client.get(f"/embed/agents/{PUBLIC_ID}")
    wrong = client.get(
        f"/embed/agents/{PUBLIC_ID}", headers={"Origin": "https://evil.example"}
    )
    assert missing.status_code == wrong.status_code == 403
    assert wrong.json()["detail"]["code"] == "AGENT_EMBED_ORIGIN_FORBIDDEN"

    repository.origins = ["https://*.example.com"]
    wildcard = client.get(
        f"/embed/agents/{PUBLIC_ID}", headers={"Origin": "https://app.example.com"}
    )
    assert wildcard.status_code == 403


def _request() -> Request:
    return Request(
        {
            "type": "http",
            "method": "GET",
            "path": f"/embed/agents/{PUBLIC_ID}",
            "query_string": b"",
            "headers": [
                (b"x-agent-embed-origin", b"https://allowed.example"),
                (b"cookie", b"ag_embed_session=nonce-1"),
            ],
            "app": SimpleNamespace(state=SimpleNamespace()),
        }
    )


def test_embed_token_is_signed_short_lived_and_bound_to_publication_and_origin(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AGENT_RUNTIME_TOKEN_SIGNING_KEY", "embed-test-secret-value")
    monkeypatch.setattr(agent_public.time, "time", lambda: 1_000_000)
    request = _request()
    token = agent_public._issue_embed_token(
        request,
        public_id=PUBLIC_ID,
        origin="https://allowed.example",
        nonce="nonce-1",
    )
    claim = agent_public._verify_embed_token(request, token=token, public_id=PUBLIC_ID)
    assert claim["origin"] == "https://allowed.example"
    assert claim["exp"] == 1_000_000 + agent_public._EMBED_TOKEN_TTL_SECONDS

    monkeypatch.setattr(agent_public.time, "time", lambda: 1_000_001)
    renewed = agent_public._issue_embed_token(
        request,
        public_id=PUBLIC_ID,
        origin="https://allowed.example",
        nonce="nonce-1",
    )
    renewed_claim = agent_public._verify_embed_token(
        request,
        token=renewed,
        public_id=PUBLIC_ID,
    )
    assert renewed_claim["sub"] == claim["sub"]

    with pytest.raises(HTTPException) as wrong_publication:
        agent_public._verify_embed_token(
            request,
            token=token,
            public_id="99999999-9999-4999-8999-999999999999",
        )
    assert wrong_publication.value.status_code == 401

    prefix, body, signature = token.split(".")
    tampered = f"{prefix}.{body[:-1]}x.{signature}"
    with pytest.raises(HTTPException):
        agent_public._verify_embed_token(request, token=tampered, public_id=PUBLIC_ID)

    monkeypatch.setattr(agent_public.time, "time", lambda: 1_001_000)
    with pytest.raises(HTTPException) as expired:
        agent_public._verify_embed_token(request, token=token, public_id=PUBLIC_ID)
    assert expired.value.detail["code"] == "AGENT_EMBED_TOKEN_INVALID"


class _SharedRedisEval:
    def __init__(self) -> None:
        self.counts: dict[str, int] = {}
        self.lock = asyncio.Lock()

    async def eval(self, _script: str, key_count: int, *values: Any) -> int:
        keys = [str(value) for value in values[:key_count]]
        arguments = [int(value) for value in values[key_count:]]
        limits = arguments[::2]
        async with self.lock:
            for index, (key, limit) in enumerate(zip(keys, limits, strict=True), start=1):
                if self.counts.get(key, 0) >= limit:
                    return index
            for key in keys:
                self.counts[key] = self.counts.get(key, 0) + 1
        return 0


def test_channel_limiter_is_atomic_and_shared_across_gateway_workers() -> None:
    async def exercise() -> None:
        backend = _SharedRedisEval()
        workers = (RedisAgentChannelLimiter(backend), RedisAgentChannelLimiter(backend))
        limits = (100, 1000, 100, 1000, 1, 1000)
        results = await asyncio.gather(*(
            workers[index % 2].consume(
                publication_id="publication-a",
                principal_id=f"principal-{index}",
                client_ip=f"192.0.2.{index}",
                limits=limits,
            )
            for index in range(8)
        ))
        assert results.count(0) == 1
        assert all(result in {0, 5} for result in results)

        ip_backend = _SharedRedisEval()
        ip_workers = (
            RedisAgentChannelLimiter(ip_backend),
            RedisAgentChannelLimiter(ip_backend),
        )
        ip_limits = (100, 1000, 1, 1000, 100, 1000)
        assert await ip_workers[0].consume(
            publication_id="publication-b",
            principal_id="first-token",
            client_ip="198.51.100.10",
            limits=ip_limits,
        ) == 0
        assert await ip_workers[1].consume(
            publication_id="publication-b",
            principal_id="rotated-token",
            client_ip="198.51.100.10",
            limits=ip_limits,
        ) == 3

    asyncio.run(exercise())


@pytest.mark.parametrize(
    "origin",
    [
        "https://*.example.com",
        "https://user:password@example.com",
        "https://example.com/path",
        "https://example.com?next=evil",
        "null",
        "javascript:alert(1)",
    ],
)
def test_channel_policy_rejects_non_exact_or_credentialed_origins(origin: str) -> None:
    with pytest.raises(ValidationError):
        AgentChannelPolicy(allowed_origins=[origin])


def test_widget_validates_postmessage_origin_source_version_and_uses_no_storage_token() -> None:
    iframe_source = (ROOT / "web" / "public" / "agent-embed.js").read_text(encoding="utf-8")
    loader_source = (ROOT / "web" / "public" / "agent-widget.js").read_text(encoding="utf-8")
    for source in (iframe_source, loader_source):
        assert "event.origin" in source
        assert "event.source" in source
        assert "agent-embed/v1" in source
        assert "postMessage" in source
        assert "localStorage" not in source
        assert "sessionStorage" not in source
        assert "agt_" not in source
        assert not re.search(r"postMessage\([^\n]+,\s*[\"']\*[\"']", source)
    assert 'iframe.sandbox = "allow-scripts allow-forms allow-same-origin"' in loader_source
    assert "launcher?.focus()" in loader_source


def test_embed_document_contains_no_reusable_runtime_token_or_internal_snapshot(
    embed_client: tuple[TestClient, _DescriptorRepository],
) -> None:
    client, _repository = embed_client
    response = client.get(
        f"/embed/agents/{PUBLIC_ID}",
        headers={"Referer": "https://allowed.example/page"},
    )
    assert response.status_code == 200
    assert "agt_" not in response.text
    assert "resolved_spec" not in response.text
    assert "instructions" not in response.text
    assert "GATEWAY_ASSISTANT_SHARED_SECRET" not in response.text
