from __future__ import annotations

import time
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from ai_gateway_contracts.capability_proof import sign_capability_proof
from fastapi import HTTPException
from knowledge_service.api.routes.capability_plane import (
    CapabilityRetrieveRequest,
    _internal_authorized,
    _text_result,
    retrieve_capability,
)
from knowledge_service.core.auth.user_resolver import UserContext
from starlette.datastructures import Headers


@pytest.fixture(autouse=True)
def actor_lookup(monkeypatch):
    lookup = AsyncMock(return_value=UserContext(user_id="user-a", tenant_id="tenant-a", user_tier="admin", roles=["admin", "platform_admin"]))
    monkeypatch.setattr("knowledge_service.api.routes.capability_plane.resolve_runtime_actor", lookup)
    return lookup


PROOF_SECRET = "p" * 32


def _request(
    payload: CapabilityRetrieveRequest | None = None,
    token: str = "internal-token",
    **overrides: str,
) -> SimpleNamespace:
    payload = payload or CapabilityRetrieveRequest(query="hello")
    scope = {
        "tenant_id": "tenant-a",
        "user_id": "user-a",
        "session_id": "session-a",
        "execution_id": "11111111-1111-4111-8111-111111111111",
        "run_id": "run-a",
    }
    now = int(time.time())
    proof = sign_capability_proof(
        PROOF_SECRET,
        method="POST",
        path="/internal/v2/capabilities/knowledge/ds-a/retrieve",
        body=payload.model_dump(mode="json"),
        **scope,
        nonce="nonce-a",
        now=now,
    )
    return SimpleNamespace(
        headers=Headers(
            {
                "x-ai-platform-internal-token": token,
                "x-ai-tenant-id": scope["tenant_id"],
                "x-ai-user-id": scope["user_id"],
                "x-ai-session-id": scope["session_id"],
                "x-ai-capability-proof": proof,
                "x-ai-execution-id": scope["execution_id"],
                "x-ai-run-id": scope["run_id"],
                **overrides,
            }
        ),
        body=AsyncMock(return_value=b"{}"),
    )


def test_internal_token_is_constant_time_and_missing_token_fails(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "internal-token")
    assert _internal_authorized(_request()) is True
    assert _internal_authorized(_request(token="wrong")) is False
    assert _internal_authorized(_request(token="")) is False


@pytest.mark.asyncio
async def test_route_calls_authoritative_retrieval_and_preserves_shape(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "internal-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", PROOF_SECRET)
    payload = CapabilityRetrieveRequest(query="hello", top_k=3, threshold=0.4)
    svc = SimpleNamespace(
        db=SimpleNamespace(
            get_user=AsyncMock(
                return_value={
                    "user_id": "user-a",
                    "tenant_id": "tenant-a",
                    "tier": "admin",
                    "roles": ["admin", "platform_admin"],
                    "status": "active",
                }
            )
        ),
        require_dataset_access=AsyncMock(return_value={"dataset_id": "ds-a"}),
        retrieve=AsyncMock(
            return_value=(
                [
                    SimpleNamespace(
                        segment_id="seg-a",
                        document_id="doc-a",
                        score=0.9,
                        text="answer",
                        metadata={"source_type": "upload"},
                        content_type="text",
                    )
                ],
                {"retrieved_count": 1},
            )
        ),
    )
    result = await retrieve_capability("ds-a", _request(payload), payload, svc)
    assert result["results"][0]["text"] == "answer"
    assert result["metadata"] == {"retrieved_count": 1}
    svc.require_dataset_access.assert_awaited_once()
    resolved_user = svc.require_dataset_access.await_args.args[0]
    assert resolved_user.tier == "admin"
    assert resolved_user.roles == ["admin", "platform_admin"]
    svc.db.get_user.assert_not_awaited()
    svc.retrieve.assert_awaited_once_with(
        user=svc.require_dataset_access.await_args.args[0],
        dataset_id="ds-a",
        query="hello",
        top_k=3,
        score_threshold=0.4,
    )


@pytest.mark.asyncio
async def test_acl_failure_does_not_reveal_dataset_existence(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "internal-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", PROOF_SECRET)
    svc = SimpleNamespace(require_dataset_access=AsyncMock(side_effect=ValueError("missing")))
    with pytest.raises(HTTPException) as error:
        await retrieve_capability("ds-a", _request(), CapabilityRetrieveRequest(query="hello"), svc)
    assert error.value.status_code == 404
    assert error.value.detail == "Dataset not found"


def test_non_text_result_is_rejected() -> None:
    with pytest.raises(Exception, match="text-only"):
        _text_result(SimpleNamespace(content_type="image", metadata={}))


@pytest.mark.asyncio
async def test_proof_binds_dataset_body_and_scope(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "internal-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", PROOF_SECRET)
    svc = SimpleNamespace(
        require_dataset_access=AsyncMock(return_value={}), retrieve=AsyncMock(return_value=([], {}))
    )
    payload = CapabilityRetrieveRequest(query="hello")
    with pytest.raises(HTTPException) as body_error:
        await retrieve_capability(
            "ds-a", _request(), CapabilityRetrieveRequest(query="changed"), svc
        )
    assert body_error.value.status_code == 401
    with pytest.raises(HTTPException) as path_error:
        await retrieve_capability("ds-b", _request(payload), payload, svc)
    assert path_error.value.status_code == 401
    with pytest.raises(HTTPException) as scope_error:
        await retrieve_capability(
            "ds-a", _request(payload, **{"x-ai-tenant-id": "tenant-b"}), payload, svc
        )
    assert scope_error.value.status_code == 401
