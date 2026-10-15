from __future__ import annotations

import copy
from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_contracts.agent_runtime import (
    AgentRuntimeEnvelopeError,
    AgentRuntimeSigner,
    canonical_runtime_json,
    runtime_sha256,
)
from ai_gateway_contracts.replay import InMemoryReplayStore
from fastapi import HTTPException
from pydantic import ValidationError
from starlette.requests import Request

from src.api.schemas.agent_runtime import (
    AgentPreviewChatRequest,
    AgentPublishedChatRequest,
    AgentVersionPreviewChatRequest,
    InternalAgentRuntimeChatRequest,
)
from src.api.v1._agent_runtime_headers import reject_client_agent_forgery
from src.api.v1.agent_runtime import (
    _assert_attachments_allowed,
    _assert_existing_pin,
    _build_snapshot,
    _runtime_enabled,
)
from src.core.auth.user_resolver import UserContext
from src.services.llm.gateway_model_meta import GatewayModelMeta


def runtime_snapshot() -> dict[str, Any]:
    return {
        "schema_version": "agent-runtime/v1",
        "tenant_id": "tenant-a",
        "agent_id": "11111111-1111-4111-8111-111111111111",
        "agent_version_id": "22222222-2222-4222-8222-222222222222",
        "publication": {
            "id": "33333333-3333-4333-8333-333333333333",
            "channel": "api",
            "auth_mode": "tenant",
        },
        "model": {
            "id": "qwen3.7-plus",
            "provider": "dashscope",
            "parameters": {"temperature": 0.3, "max_tokens": 2048},
        },
        "instructions": {
            "agent": "Answer only from the approved Agent instructions.",
            "prompt_hash": "sha256:prompt",
        },
        "capabilities": [
            {
                "type": "platform",
                "id": "search_knowledge_base",
                "version": None,
                "schema_hash": "sha256:tool-schema",
                "risk": "low",
                "config": {},
            }
        ],
        "knowledge": {
            "datasets": ["44444444-4444-4444-8444-444444444444"],
            "retrieval": {"mode": "auto", "top_k": 5, "threshold": 0.4},
        },
        "memory": {"mode": "session"},
        "channel_policy": {
            "attachments": True,
            "high_risk_tools": False,
            "allowed_origins": [],
        },
        "fingerprints": {
            "spec": "sha256:spec",
            "tool_schema": "sha256:tools",
            "skills": "sha256:skills",
            "knowledge_revision": "sha256:knowledge",
        },
    }


def request_body() -> dict[str, Any]:
    return {
        "message": "hello",
        "session_id": "session-a",
        "history": [],
        "attachments": [],
    }


def gateway_request(**state: Any) -> Request:
    return Request(
        {
            "type": "http",
            "method": "POST",
            "path": "/api/v1/agent-runtime/test/chat/stream",
            "query_string": b"",
            "headers": [],
            "state": {},
            "app": SimpleNamespace(state=SimpleNamespace(**state)),
        }
    )


class _AuthorizedKnowledgeResolver:
    def resolve(self, **kwargs: Any) -> list[dict[str, Any]]:
        return list(kwargs["bindings"])


def runtime_resolution() -> dict[str, Any]:
    return {
        "agent": {
            "tenant_id": "tenant-a",
            "agent_id": "11111111-1111-4111-8111-111111111111",
        },
        "draft": {},
        "version": {
            "agent_version_id": "22222222-2222-4222-8222-222222222222",
            "spec_hash": "sha256:spec",
        },
        "publication": {
            "publication_id": "33333333-3333-4333-8333-333333333333",
            "channel": "api",
            "auth_mode": "tenant",
            "policy": {
                "attachments": False,
                "high_risk_tools": False,
                "allowed_origins": [],
            },
        },
        "spec": {
            "model": {
                "model_id": "qwen3.7-plus",
                "provider_id": "dashscope",
                "temperature": 0.2,
                "max_tokens": 1024,
            },
            "instructions": "Use only the immutable Agent instructions.",
            "memory": {"mode": "session"},
        },
        "capabilities": [
            {
                "capability_type": "native",
                "resource_id": "search_knowledge_base",
                "risk": "low",
            },
            {
                "capability_type": "mcp",
                "resource_id": "mcp-danger",
                "risk": "high",
            },
        ],
        "knowledge": [
            {
                "dataset_id": "dataset-a",
                "retrieval_config": {
                    "mode": "tool",
                    "top_k": 7,
                    "threshold": 0.55,
                },
            }
        ],
    }


def signed_envelope(*, replay_store: Any | None = None) -> tuple[AgentRuntimeSigner, dict[str, Any]]:
    signer = AgentRuntimeSigner(
        secret="a" * 32,
        issuer="gateway-test",
        replay_store=replay_store or InMemoryReplayStore(),
        max_ttl_ms=60_000,
    )
    snapshot = runtime_snapshot()
    envelope = signer.sign(
        tenant_id="tenant-a",
        caller_principal="user-a",
        agent_id=snapshot["agent_id"],
        agent_version_id=snapshot["agent_version_id"],
        draft_revision=None,
        publication_id=snapshot["publication"]["id"],
        channel="api",
        session_id="session-a",
        resolved_snapshot=snapshot,
        request_body=request_body(),
        spec_hash="sha256:spec",
        issued_at_ms=1_000_000,
        expires_at_ms=1_030_000,
        nonce="nonce-a",
    )
    return signer, envelope


def test_canonical_runtime_json_and_hash_are_order_independent() -> None:
    first = {"b": [2, 1], "a": {"z": True, "x": None}}
    second = {"a": {"x": None, "z": True}, "b": [2, 1]}

    assert canonical_runtime_json(first) == canonical_runtime_json(second)
    assert runtime_sha256(first) == runtime_sha256(second)
    assert runtime_sha256(first).startswith("sha256:")


def test_envelope_verification_recalculates_hashes_and_consumes_nonce_once() -> None:
    signer, envelope = signed_envelope()

    verified = signer.verify(
        envelope,
        request_body=request_body(),
        expected_tenant_id="tenant-a",
        expected_caller_principal="user-a",
        expected_session_id="session-a",
        now_ms=1_010_000,
    )

    assert verified.agent_id == runtime_snapshot()["agent_id"]
    assert verified.runtime_fingerprint == envelope["snapshot_hash"]
    assert verified.capability_ids == frozenset({"search_knowledge_base"})

    with pytest.raises(AgentRuntimeEnvelopeError, match="AGENT_RUNTIME_REPLAYED"):
        signer.verify(
            envelope,
            request_body=request_body(),
            expected_tenant_id="tenant-a",
            expected_caller_principal="user-a",
            expected_session_id="session-a",
            now_ms=1_010_001,
        )


@pytest.mark.parametrize(
    ("mutation", "code"),
    [
        (lambda envelope: envelope.__setitem__("tenant_id", "tenant-b"), "SIGNATURE_INVALID"),
        (lambda envelope: envelope.__setitem__("session_id", "session-b"), "SIGNATURE_INVALID"),
        (
            lambda envelope: envelope["resolved_snapshot"]["model"].__setitem__(
                "id", "forged-model"
            ),
            "SNAPSHOT_HASH_MISMATCH",
        ),
        (lambda envelope: envelope.__setitem__("spec_hash", "sha256:forged"), "SIGNATURE_INVALID"),
        (lambda envelope: envelope.__setitem__("nonce", "nonce-forged"), "SIGNATURE_INVALID"),
    ],
)
def test_envelope_rejects_identity_snapshot_and_signature_mutation(
    mutation,
    code: str,
) -> None:
    signer, envelope = signed_envelope()
    forged = copy.deepcopy(envelope)
    mutation(forged)

    with pytest.raises(AgentRuntimeEnvelopeError, match=f"AGENT_RUNTIME_{code}"):
        signer.verify(
            forged,
            request_body=request_body(),
            expected_tenant_id="tenant-a",
            expected_caller_principal="user-a",
            expected_session_id="session-a",
            now_ms=1_010_000,
        )


def test_envelope_rejects_body_substitution_session_substitution_and_expiry() -> None:
    signer, envelope = signed_envelope()
    changed_body = {**request_body(), "message": "substituted"}

    with pytest.raises(AgentRuntimeEnvelopeError, match="AGENT_RUNTIME_BODY_HASH_MISMATCH"):
        signer.verify(
            envelope,
            request_body=changed_body,
            expected_tenant_id="tenant-a",
            expected_caller_principal="user-a",
            expected_session_id="session-a",
            now_ms=1_010_000,
        )

    with pytest.raises(AgentRuntimeEnvelopeError, match="AGENT_RUNTIME_SESSION_MISMATCH"):
        signer.verify(
            envelope,
            request_body=request_body(),
            expected_tenant_id="tenant-a",
            expected_caller_principal="user-a",
            expected_session_id="session-b",
            now_ms=1_010_000,
        )

    with pytest.raises(AgentRuntimeEnvelopeError, match="AGENT_RUNTIME_EXPIRED"):
        signer.verify(
            envelope,
            request_body=request_body(),
            expected_tenant_id="tenant-a",
            expected_caller_principal="user-a",
            expected_session_id="session-a",
            now_ms=1_030_001,
        )


def test_envelope_fails_closed_when_atomic_nonce_store_is_unavailable() -> None:
    class FailingReplayStore:
        def seen_or_record(self, request_id: str, ttl_ms: int) -> bool:
            raise RuntimeError(f"store unavailable: {request_id}:{ttl_ms}")

    signer, envelope = signed_envelope(replay_store=FailingReplayStore())

    with pytest.raises(AgentRuntimeEnvelopeError, match="AGENT_RUNTIME_REPLAY_STORE_UNAVAILABLE"):
        signer.verify(
            envelope,
            request_body=request_body(),
            expected_tenant_id="tenant-a",
            expected_caller_principal="user-a",
            expected_session_id="session-a",
            now_ms=1_010_000,
        )


@pytest.mark.parametrize(
    "forged_field",
    [
        "model_id",
        "system_prompt",
        "capabilities",
        "resolved_snapshot",
        "runtime_envelope",
        "agent_version_id",
        "publication_id",
    ],
)
def test_external_runtime_schemas_forbid_trusted_agent_overrides(forged_field: str) -> None:
    payload = {
        "message": "hello",
        "session_id": "session-a",
        "draft_revision": 3,
        forged_field: "forged",
    }

    with pytest.raises(ValidationError):
        AgentPreviewChatRequest.model_validate(payload)

    payload.pop("draft_revision")
    with pytest.raises(ValidationError):
        AgentPublishedChatRequest.model_validate(payload)


def test_version_preview_resume_identity_is_paired_and_part_of_internal_verification_body() -> None:
    for partial in (
        {"resume_run_id": "run-a"},
        {"resume_approval_id": "approval-a"},
    ):
        with pytest.raises(ValidationError, match="resume_run_id.*resume_approval_id"):
            AgentVersionPreviewChatRequest.model_validate(
                {"message": "continue", "session_id": "session-a", **partial}
            )

    preview = AgentVersionPreviewChatRequest.model_validate(
        {
            "message": "continue",
            "session_id": "session-a",
            "resume_run_id": "run-a",
            "resume_approval_id": "approval-a",
        }
    )
    assert preview.resume_run_id == "run-a"
    assert preview.resume_approval_id == "approval-a"

    published = AgentPublishedChatRequest.model_validate(
        {
            "message": "continue",
            "session_id": "session-a",
            "resume_run_id": "run-a",
            "resume_approval_id": "approval-a",
        }
    )
    assert published.resume_run_id == "run-a"
    assert published.resume_approval_id == "approval-a"

    draft = AgentPreviewChatRequest.model_validate(
        {
            "message": "continue",
            "session_id": "session-a",
            "draft_revision": 1,
            "resume_run_id": "run-a",
            "resume_approval_id": "approval-a",
        }
    )
    assert draft.resume_run_id == "run-a"
    assert draft.resume_approval_id == "approval-a"

    with pytest.raises(ValidationError, match="resume_run_id.*resume_approval_id"):
        AgentPublishedChatRequest.model_validate(
            {"message": "continue", "session_id": "session-a", "resume_run_id": "run-a"}
        )

    with pytest.raises(ValidationError, match="resume_run_id.*resume_approval_id"):
        InternalAgentRuntimeChatRequest.model_validate(
            {
                "message": "continue",
                "session_id": "session-a",
                "history": None,
                "attachments": [],
                "resume_run_id": "run-a",
                "runtime_envelope": {},
            }
        )

    internal = InternalAgentRuntimeChatRequest.model_validate(
        {
            "message": "continue",
            "session_id": "session-a",
            "history": None,
            "attachments": [],
            "resume_run_id": "run-a",
            "resume_approval_id": "approval-a",
            "runtime_envelope": {"signature": "signed"},
        }
    )
    assert internal.verification_body() == {
        "message": "continue",
        "session_id": "session-a",
        "history": None,
        "attachments": [],
        "resume_run_id": "run-a",
        "resume_approval_id": "approval-a",
    }


def test_generic_gateway_rejects_agent_runtime_headers_and_body_fields() -> None:
    request = Request(
        {
            "type": "http",
            "method": "POST",
            "path": "/api/v1/assistant/chat",
            "query_string": b"",
            "headers": [(b"x-agent-id", b"forged")],
        }
    )
    with pytest.raises(HTTPException) as header_error:
        reject_client_agent_forgery(request)
    assert header_error.value.status_code == 400

    clean_request = Request(
        {
            "type": "http",
            "method": "POST",
            "path": "/api/v1/assistant/chat",
            "query_string": b"",
            "headers": [],
        }
    )
    with pytest.raises(HTTPException) as body_error:
        reject_client_agent_forgery(clean_request, {"resolved_snapshot": {}})
    assert body_error.value.status_code == 422


def test_forgery_guard_allows_embed_grant_headers_but_not_forged_agent_headers() -> None:
    # Both headers are verified together with the nonce cookie downstream.
    embed_request = Request(
        {
            "type": "http",
            "method": "POST",
            "path": "/api/v1/public/agents/pub-1/chat/stream",
            "query_string": b"",
            "headers": [
                (b"x-agent-embed-token", b"e1.signed-token"),
                (b"x-agent-embed-origin", b"https://console.example.test"),
            ],
        }
    )
    reject_client_agent_forgery(embed_request)  # must not raise

    forged_request = Request(
        {
            "type": "http",
            "method": "POST",
            "path": "/api/v1/public/agents/pub-1/chat/stream",
            "query_string": b"",
            "headers": [
                (b"x-agent-embed-token", b"e1.signed-token"),
                (b"x-agent-embed-origin", b"https://console.example.test"),
                (b"x-agent-id", b"forged"),
            ],
        }
    )
    with pytest.raises(HTTPException) as error:
        reject_client_agent_forgery(forged_request)
    assert error.value.status_code == 400


def test_preview_session_pin_rejects_cross_version_reuse() -> None:
    request = gateway_request()
    user = UserContext(
        user_id="user-a",
        tenant_id="tenant-a",
        is_authenticated=True,
    )
    existing = SimpleNamespace(
        user_id="user-a",
        tenant_id="tenant-a",
        channel="preview",
        agent_id="agent-a",
        agent_version_id="version-a",
        publication_id=None,
        agent_draft_revision=None,
    )

    with pytest.raises(HTTPException) as error:
        _assert_existing_pin(
            request,
            user,
            existing,
            agent_id="agent-a",
            agent_version_id="version-b",
            publication_id=None,
            channel="preview",
            draft_revision=None,
        )

    assert error.value.status_code == 404
    assert error.value.detail["code"] == "AGENT_RUNTIME_SESSION_NOT_FOUND"


@pytest.mark.asyncio
async def test_gateway_snapshot_intersects_model_capability_and_knowledge_policy() -> None:
    class ModelResolver:
        def resolve(self, **kwargs: Any) -> dict[str, Any]:
            assert kwargs["tenant_id"] == "tenant-a"
            assert kwargs["model"]["model_id"] == "qwen3.7-plus"
            return {"id": "qwen3.7-plus", "provider": "dashscope"}

    class CapabilityResolver:
        def resolve(self, **kwargs: Any) -> list[dict[str, Any]]:
            return [
                *kwargs["bindings"],
                {
                    "capability_type": "mcp",
                    "resource_id": "forged-expansion",
                    "risk": "low",
                },
            ]

    class KnowledgeResolver:
        def resolve(self, **kwargs: Any) -> list[str]:
            assert kwargs["user_id"] == "user-a"
            return ["dataset-a", "forged-dataset"]

    request = gateway_request(
        agent_runtime_model_resolver=ModelResolver(),
        agent_runtime_capability_resolver=CapabilityResolver(),
        agent_runtime_knowledge_resolver=KnowledgeResolver(),
    )
    user = UserContext(
        user_id="user-a",
        tenant_id="tenant-a",
        is_authenticated=True,
    )

    snapshot = await _build_snapshot(
        request,
        runtime_resolution(),
        user,
        channel="api",
    )

    assert snapshot["model"] == {
        "id": "qwen3.7-plus",
        "provider": "dashscope",
        "parameters": {"temperature": 0.2, "max_tokens": 1024},
    }
    assert [item["id"] for item in snapshot["capabilities"]] == [
        "search_knowledge_base"
    ]
    assert snapshot["knowledge"]["datasets"] == ["dataset-a"]
    assert snapshot["knowledge"]["retrieval"]["mode"] == "tool"


@pytest.mark.asyncio
async def test_gateway_snapshot_compiles_outcome_before_detailed_instructions() -> None:
    class ModelResolver:
        def resolve(self, **_kwargs: Any) -> dict[str, Any]:
            return {"id": "qwen3.7-plus", "provider": "dashscope"}

    class CapabilityResolver:
        def resolve(self, **kwargs: Any) -> list[dict[str, Any]]:
            return list(kwargs["bindings"])

    resolution = runtime_resolution()
    resolution["spec"]["outcome"] = {
        "goal": "Classify the request and make ownership explicit.",
        "triggers": ["A support request arrives"],
        "inputs": ["Request text"],
        "human_boundaries": ["Do not issue refunds without approval"],
        "success_criteria": ["Queue and owner are named"],
    }
    request = gateway_request(
        agent_runtime_model_resolver=ModelResolver(),
        agent_runtime_capability_resolver=CapabilityResolver(),
        agent_runtime_knowledge_resolver=_AuthorizedKnowledgeResolver(),
    )
    user = UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)
    snapshot = await _build_snapshot(request, resolution, user, channel="api")
    instructions = snapshot["instructions"]["agent"]
    assert instructions.startswith("[Agent outcome contract]\nGoal: Classify the request")
    assert "[Detailed instructions]\nUse only the immutable Agent instructions." in instructions


@pytest.mark.asyncio
async def test_gateway_capability_resolver_cannot_mutate_bound_metadata() -> None:
    resolution = runtime_resolution()
    resolution["capabilities"][0].update(
        {
            "resource_version": "bound-search-v1",
            "schema_hash": "sha256:bound-search-schema",
            "config": {"scope": "read"},
        }
    )
    resolution["capabilities"][1].update(
        {
            "resource_version": "bound-danger-v1",
            "schema_hash": "sha256:bound-danger-schema",
            "config": {"scope": "admin"},
        }
    )

    class ModelResolver:
        def resolve(self, **_kwargs: Any) -> dict[str, Any]:
            return {"id": "qwen3.7-plus", "provider": "dashscope"}

    class CapabilityResolver:
        def resolve(self, **_kwargs: Any) -> list[dict[str, Any]]:
            return [
                {
                    "capability_type": "native",
                    "resource_id": "search_knowledge_base",
                    "resource_version": "forged-search-v9",
                    "schema_hash": "sha256:forged-search-schema",
                    "risk": "low",
                    "config": {"scope": "write"},
                },
                {
                    "capability_type": "mcp",
                    "resource_id": "mcp-danger",
                    "resource_version": "forged-danger-v9",
                    "schema_hash": "sha256:forged-danger-schema",
                    "risk": "low",
                    "config": {"resolver_injected": True},
                },
            ]

    snapshot = await _build_snapshot(
        gateway_request(
            agent_runtime_model_resolver=ModelResolver(),
            agent_runtime_capability_resolver=CapabilityResolver(),
            agent_runtime_knowledge_resolver=_AuthorizedKnowledgeResolver(),
        ),
        resolution,
        UserContext(
            user_id="user-a",
            tenant_id="tenant-a",
            is_authenticated=True,
        ),
        channel="api",
    )

    assert snapshot["channel_policy"]["high_risk_tools"] is False
    assert snapshot["capabilities"] == [
        {
            "type": "platform",
            "id": "search_knowledge_base",
            "version": "bound-search-v1",
            "schema_hash": "sha256:bound-search-schema",
            "risk": "low",
            "config": {"scope": "read"},
        }
    ]


@pytest.mark.asyncio
async def test_preview_snapshot_enables_high_risk_tools_without_misleading_pin() -> None:
    class ModelResolver:
        def resolve(self, **_kwargs: Any) -> dict[str, Any]:
            return {"id": "qwen3.7-plus", "provider": "dashscope"}

    resolution = runtime_resolution()
    resolution["publication"] = None
    resolution["capabilities"] = [
        {
            "capability_type": "native",
            "resource_id": "dangerous-native",
            "risk": "high",
            "config": {},
        }
    ]

    snapshot = await _build_snapshot(
        gateway_request(
            agent_runtime_model_resolver=ModelResolver(),
            agent_runtime_capability_resolver=_AuthorizedKnowledgeResolver(),
            agent_runtime_knowledge_resolver=_AuthorizedKnowledgeResolver(),
        ),
        resolution,
        UserContext(
            user_id="user-a",
            tenant_id="tenant-a",
            is_authenticated=True,
        ),
        channel="preview",
    )

    assert snapshot["channel_policy"]["high_risk_tools"] is True
    # The gateway cannot resolve the live tool definition, so no
    # confirmation pin is fabricated into the snapshot; runtime validation
    # stays fail-closed against the real definition.
    assert snapshot["capabilities"] == [
        {
            "type": "platform",
            "id": "dangerous-native",
            "version": None,
            "schema_hash": None,
            "risk": "high",
            "config": {},
        }
    ]


@pytest.mark.asyncio
async def test_gateway_snapshot_fails_closed_when_model_readiness_is_unknown() -> None:
    request = gateway_request()
    user = UserContext(
        user_id="user-a",
        tenant_id="tenant-a",
        is_authenticated=True,
    )

    with pytest.raises(HTTPException) as error:
        await _build_snapshot(
            request,
            runtime_resolution(),
            user,
            channel="api",
        )

    assert error.value.status_code == 503
    assert error.value.detail["code"] == "AGENT_RUNTIME_MODEL_UNAVAILABLE"


@pytest.mark.asyncio
async def test_historical_empty_model_snapshot_uses_server_default_without_provider_pin() -> None:
    class ModelResolver:
        def __init__(self) -> None:
            self.models: list[dict[str, Any]] = []

        def resolve(self, **kwargs: Any) -> dict[str, Any]:
            self.models.append(dict(kwargs["model"]))
            return {"id": kwargs["model"]["model_id"], "provider": "resolved-provider"}

    resolver = ModelResolver()
    request = gateway_request(
        settings=SimpleNamespace(default_model="deployment-default-model"),
        agent_runtime_model_resolver=resolver,
        agent_runtime_knowledge_resolver=_AuthorizedKnowledgeResolver(),
    )
    resolution = runtime_resolution()
    resolution["spec"]["model"] = {
        "model_id": "",
        "provider_id": "stale-ui-placeholder",
        "temperature": 0.2,
    }
    user = UserContext(
        user_id="user-a",
        tenant_id="tenant-a",
        is_authenticated=True,
    )

    snapshot = await _build_snapshot(request, resolution, user, channel="api")

    assert resolver.models == [
        {"model_id": "deployment-default-model", "temperature": 0.2}
    ]
    assert snapshot["model"] == {
        "id": "deployment-default-model",
        "provider": "resolved-provider",
        "parameters": {"temperature": 0.2},
    }


@pytest.mark.asyncio
async def test_gateway_snapshot_accepts_enabled_database_provider_with_saved_key() -> None:
    class ModelService:
        async def get_model(self, tenant_id: str, model_id: str) -> dict[str, Any]:
            assert tenant_id == "tenant-a"
            assert model_id == "qwen3.7-plus"
            return {
                "model_id": model_id,
                "provider_id": "dashscope",
                "is_enabled": True,
                "access_level": "public",
            }

    class ProviderService:
        async def get_provider(self, tenant_id: str, provider_id: str) -> dict[str, Any]:
            assert tenant_id == "tenant-a"
            assert provider_id == "dashscope"
            return {
                "provider_id": provider_id,
                "is_enabled": True,
                # Assistant resolves this encrypted key per request.
                "has_api_key": True,
            }

    model_meta = GatewayModelMeta(ModelService(), ProviderService())
    request = gateway_request(model_meta=model_meta)
    user = UserContext(
        user_id="user-a",
        tenant_id="tenant-a",
        is_authenticated=True,
    )

    request.app.state.agent_runtime_knowledge_resolver = _AuthorizedKnowledgeResolver()
    snapshot = await _build_snapshot(request, runtime_resolution(), user, channel="api")

    assert snapshot["model"]["id"] == "qwen3.7-plus"
    assert snapshot["model"]["provider"] == "dashscope"


@pytest.mark.asyncio
async def test_gateway_snapshot_accepts_provider_configured_in_assistant_runtime() -> None:
    class ModelService:
        async def get_model(self, tenant_id: str, model_id: str) -> dict[str, Any]:
            assert tenant_id == "tenant-a"
            return {
                "model_id": model_id,
                "provider_id": "dashscope",
                "is_enabled": True,
                "access_level": "public",
            }

    class ProviderService:
        async def get_provider(self, tenant_id: str, provider_id: str) -> dict[str, Any]:
            assert tenant_id == "tenant-a"
            return {"provider_id": provider_id, "is_enabled": True}

    model_meta = GatewayModelMeta(
        ModelService(),
        ProviderService(),
        runtime_configured_providers={"dashscope"},
    )
    user = UserContext(
        user_id="user-a",
        tenant_id="tenant-a",
        is_authenticated=True,
    )

    snapshot = await _build_snapshot(
        gateway_request(
            model_meta=model_meta,
            agent_runtime_knowledge_resolver=_AuthorizedKnowledgeResolver(),
        ),
        runtime_resolution(),
        user,
        channel="api",
    )

    assert snapshot["model"]["id"] == "qwen3.7-plus"
    assert snapshot["model"]["provider"] == "dashscope"


@pytest.mark.asyncio
async def test_gateway_snapshot_allows_explicit_offline_e2e_model_stub(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("ASSISTANT_E2E_STUB_LLM", "true")
    user = UserContext(
        user_id="user-a",
        tenant_id="tenant-a",
        is_authenticated=True,
    )

    snapshot = await _build_snapshot(
        gateway_request(
            agent_runtime_knowledge_resolver=_AuthorizedKnowledgeResolver()
        ),
        runtime_resolution(),
        user,
        channel="api",
    )

    assert snapshot["model"]["id"] == "qwen3.7-plus"
    assert snapshot["model"]["provider"] == "dashscope"


def test_gateway_snapshot_rejects_attachments_when_channel_policy_disables_them() -> None:
    snapshot = runtime_snapshot()
    snapshot["channel_policy"]["attachments"] = False

    with pytest.raises(HTTPException) as error:
        _assert_attachments_allowed(
            gateway_request(),
            snapshot,
            [{"file_id": "forged-file"}],
        )

    assert error.value.status_code == 422
    assert error.value.detail["code"] == "AGENT_RUNTIME_ATTACHMENTS_FORBIDDEN"


def test_runtime_feature_flag_can_disable_agent_routes_without_touching_assistant(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AGENT_STUDIO_RUNTIME_ENABLED", "false")
    assert _runtime_enabled() is False
    monkeypatch.setenv("AGENT_STUDIO_RUNTIME_ENABLED", "true")
    assert _runtime_enabled() is True


def test_preview_and_published_envelope_identity_shapes_cannot_be_mixed() -> None:
    signer = AgentRuntimeSigner(
        secret="a" * 32,
        issuer="gateway-test",
        replay_store=InMemoryReplayStore(),
    )
    preview = runtime_snapshot()
    preview["agent_version_id"] = None
    preview["publication"] = {
        "id": None,
        "channel": "preview",
        "auth_mode": "private",
    }
    signed = signer.sign(
        tenant_id="tenant-a",
        caller_principal="user-a",
        agent_id=preview["agent_id"],
        agent_version_id=None,
        draft_revision=3,
        publication_id=None,
        channel="preview",
        session_id="session-a",
        resolved_snapshot=preview,
        request_body=request_body(),
        spec_hash="sha256:spec",
    )
    assert signed["draft_revision"] == 3

    with pytest.raises(AgentRuntimeEnvelopeError, match="AGENT_RUNTIME_ENVELOPE_INVALID"):
        signer.sign(
            tenant_id="tenant-a",
            caller_principal="user-a",
            agent_id=runtime_snapshot()["agent_id"],
            agent_version_id=runtime_snapshot()["agent_version_id"],
            draft_revision=3,
            publication_id=runtime_snapshot()["publication"]["id"],
            channel="api",
            session_id="session-a",
            resolved_snapshot=runtime_snapshot(),
            request_body=request_body(),
            spec_hash="sha256:spec",
        )


def test_preview_envelope_requires_exactly_one_signed_draft_or_version_pin() -> None:
    signer = AgentRuntimeSigner(
        secret="a" * 32,
        issuer="gateway-test",
        replay_store=InMemoryReplayStore(),
    )
    version_preview = runtime_snapshot()
    version_preview["publication"] = {
        "id": None,
        "channel": "preview",
        "auth_mode": "private",
    }
    version_id = str(version_preview["agent_version_id"])

    signed_version = signer.sign(
        tenant_id="tenant-a",
        caller_principal="user-a",
        agent_id=version_preview["agent_id"],
        agent_version_id=version_id,
        draft_revision=None,
        publication_id=None,
        channel="preview",
        session_id="session-version",
        resolved_snapshot=version_preview,
        request_body=request_body(),
        spec_hash="sha256:spec",
    )
    assert signed_version["agent_version_id"] == version_id
    assert signed_version["draft_revision"] is None

    for agent_version_id, draft_revision in ((None, None), (version_id, 3)):
        with pytest.raises(AgentRuntimeEnvelopeError, match="AGENT_RUNTIME_ENVELOPE_INVALID"):
            signer.sign(
                tenant_id="tenant-a",
                caller_principal="user-a",
                agent_id=version_preview["agent_id"],
                agent_version_id=agent_version_id,
                draft_revision=draft_revision,
                publication_id=None,
                channel="preview",
                session_id="session-invalid",
                resolved_snapshot=version_preview,
                request_body=request_body(),
                spec_hash="sha256:spec",
            )

    forged_version = copy.deepcopy(signed_version)
    forged_version["agent_version_id"] = "99999999-9999-4999-8999-999999999999"
    with pytest.raises(AgentRuntimeEnvelopeError, match="AGENT_RUNTIME_SIGNATURE_INVALID"):
        signer.verify(
            forged_version,
            request_body=request_body(),
            expected_tenant_id="tenant-a",
            expected_caller_principal="user-a",
            expected_session_id="session-version",
        )

    draft_preview = copy.deepcopy(version_preview)
    draft_preview["agent_version_id"] = None
    signed_draft = signer.sign(
        tenant_id="tenant-a",
        caller_principal="user-a",
        agent_id=draft_preview["agent_id"],
        agent_version_id=None,
        draft_revision=3,
        publication_id=None,
        channel="preview",
        session_id="session-draft",
        resolved_snapshot=draft_preview,
        request_body=request_body(),
        spec_hash="sha256:spec",
    )
    forged_draft = copy.deepcopy(signed_draft)
    forged_draft["draft_revision"] = 4
    with pytest.raises(AgentRuntimeEnvelopeError, match="AGENT_RUNTIME_SIGNATURE_INVALID"):
        signer.verify(
            forged_draft,
            request_body=request_body(),
            expected_tenant_id="tenant-a",
            expected_caller_principal="user-a",
            expected_session_id="session-draft",
        )


def test_runtime_snapshot_rejects_credential_shaped_configuration() -> None:
    signer = AgentRuntimeSigner(
        secret="a" * 32,
        issuer="gateway-test",
        replay_store=InMemoryReplayStore(),
    )
    snapshot = runtime_snapshot()
    snapshot["capabilities"][0]["config"] = {"secretRef": "forbidden"}
    with pytest.raises(AgentRuntimeEnvelopeError, match="AGENT_RUNTIME_SECRET_FORBIDDEN"):
        signer.sign(
            tenant_id="tenant-a",
            caller_principal="user-a",
            agent_id=snapshot["agent_id"],
            agent_version_id=snapshot["agent_version_id"],
            draft_revision=None,
            publication_id=snapshot["publication"]["id"],
            channel="api",
            session_id="session-a",
            resolved_snapshot=snapshot,
            request_body=request_body(),
            spec_hash="sha256:spec",
        )


def _unpinned_resolution(capability_ids: list[str]) -> dict[str, Any]:
    resolution = runtime_resolution()
    resolution["publication"]["policy"]["high_risk_tools"] = True
    resolution["capabilities"] = [
        {"capability_type": "native", "resource_id": capability_id, "risk": "high"}
        for capability_id in capability_ids
    ]
    return resolution


@pytest.mark.asyncio
async def test_unpinned_high_risk_warning_map_stays_bounded(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The one-shot warning map stays bounded without changing authorization.

    Eviction may repeat a warning, but capability projection remains intact.
    """
    from src.api.v1 import agent_runtime as runtime_module
    from src.api.v1._agent_runtime_routes import snapshot as snapshot_module

    # Patch the real snapshot use-case map, not the facade's copied reference.
    monkeypatch.setattr(snapshot_module, "_unpinned_high_risk_platform", {})

    class ModelResolver:
        def resolve(self, **_kwargs: Any) -> dict[str, Any]:
            return {"id": "qwen3.7-plus", "provider": "dashscope"}

    overflow = runtime_module._UNPINNED_WARNING_CAP + 20
    capability_ids = [f"cap-{i}" for i in range(overflow)]
    request = gateway_request(
        agent_runtime_model_resolver=ModelResolver(),
        agent_runtime_capability_resolver=_AuthorizedKnowledgeResolver(),
        agent_runtime_knowledge_resolver=_AuthorizedKnowledgeResolver(),
    )
    user = UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)

    snapshot = await _build_snapshot(
        request,
        _unpinned_resolution(capability_ids),
        user,
        channel="api",
    )

    tracked = snapshot_module._unpinned_high_risk_platform
    assert len(tracked) == runtime_module._UNPINNED_WARNING_CAP
    assert f"cap-{overflow - 1}" in tracked  # newest id kept
    assert "cap-0" not in tracked  # oldest id evicted first
    # Authorization is unaffected: every bound capability remains projected.
    assert len(snapshot["capabilities"]) == overflow


@pytest.mark.asyncio
async def test_unpinned_high_risk_warning_is_recorded_once_per_capability(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from src.api.v1._agent_runtime_routes import snapshot as snapshot_module

    # ARC-01B: patch the map at its real home (see the bounded-map test above).
    monkeypatch.setattr(snapshot_module, "_unpinned_high_risk_platform", {})

    class ModelResolver:
        def resolve(self, **_kwargs: Any) -> dict[str, Any]:
            return {"id": "qwen3.7-plus", "provider": "dashscope"}

    request = gateway_request(
        agent_runtime_model_resolver=ModelResolver(),
        agent_runtime_capability_resolver=_AuthorizedKnowledgeResolver(),
        agent_runtime_knowledge_resolver=_AuthorizedKnowledgeResolver(),
    )
    user = UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)

    for _ in range(3):
        await _build_snapshot(
            request,
            _unpinned_resolution(["cap-repeat"]),
            user,
            channel="api",
        )

    assert snapshot_module._unpinned_high_risk_platform == {"cap-repeat": None}
