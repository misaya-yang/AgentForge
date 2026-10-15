from __future__ import annotations

from pathlib import Path

import httpx
import pytest
from ai_gateway_core.auth.gateway_secret import (
    GatewaySecret,
    InMemoryReplayStore,
    InvalidGatewaySecret,
)
from ai_gateway_core.knowledge import proxy_client

from src.api.v1 import _proxy_utils
from src.services import knowledge_authz
from src.services.eval import kb_ragas_client


def test_every_gateway_to_knowledge_signer_uses_the_verifier_binding(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "gateway-knowledge-test-secret")
    monkeypatch.setattr(proxy_client, "_gateway_secret_signer", None)
    monkeypatch.setattr(kb_ragas_client, "_gateway_secret_signer", None)
    signers = (
        proxy_client._get_signer(),
        _proxy_utils._build_signer(),
        knowledge_authz._get_signer(),
        kb_ragas_client._get_signer(),
    )
    verifier = GatewaySecret(
        secret="gateway-knowledge-test-secret",
        caller_service="gateway",
        audience="knowledge-service",
        allowed_path_prefixes=("/api/v1",),
        replay_store=InMemoryReplayStore(),
    )

    for index, signer in enumerate(signers):
        assert signer is not None
        assert signer.caller_service == "gateway"
        assert signer.audience == "knowledge-service"
        assert signer.allowed_path_prefixes == ("/api/v1",)
        header = signer.sign(
            request_id=f"binding-{index}",
            method="GET",
            path="/api/v1/knowledge/datasets",
        )
        assert verifier.verify(
            header,
            method="GET",
            path="/api/v1/knowledge/datasets",
        ) == f"binding-{index}"
        with pytest.raises(InvalidGatewaySecret, match="path outside key scope"):
            signer.sign(method="GET", path="/health")


@pytest.mark.asyncio
async def test_knowledge_health_probe_remains_unsigned_outside_api_scope() -> None:
    seen_headers: dict[str, str] = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen_headers.update(dict(request.headers))
        return httpx.Response(200)

    client = proxy_client.KBProxyClient(
        base_url="http://knowledge-service.test",
        transport=httpx.MockTransport(handler),
        gateway_secret=GatewaySecret(
            secret="gateway-knowledge-test-secret",
            caller_service="gateway",
            audience="knowledge-service",
            allowed_path_prefixes=("/api/v1",),
        ),
    )
    try:
        assert await client.health_check() is True
    finally:
        await client.close()

    assert "x-gateway-secret" not in seen_headers


def test_knowledge_verifier_factory_is_bound_to_gateway_api_scope() -> None:
    source = (
        Path(__file__).resolve().parents[2]
        / "apps/knowledge-service/src/knowledge_service/main.py"
    ).read_text(encoding="utf-8")

    assert 'caller_service="gateway"' in source
    assert 'audience="knowledge-service"' in source
    assert 'allowed_path_prefixes=("/api/v1",)' in source
