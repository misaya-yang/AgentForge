from __future__ import annotations

import pytest

from src.services.assistant_entry.launch_resolution import (
    AgentLaunchResolutionError,
    resolve_agent_launch,
)


class _ModelService:
    async def get_model(self, _tenant_id: str, model_id: str):
        return {
            "model_id": model_id,
            "provider_id": "provider-a",
            "is_enabled": True,
            "effective_capabilities": {
                "wire_protocols": {
                    "preferred": "responses_v1",
                    "supported": ["responses_v1"],
                }
            },
        }


@pytest.mark.asyncio
async def test_assistant_and_responses_share_resolver_identity_and_payload() -> None:
    common = {
        "tenant_id": "tenant-a",
        "user_id": "user-a",
        "session_id": "session-a",
        "model_id": "model-a",
        "model_service": _ModelService(),
        "readonly_capabilities": {"knowledge": {"dataset_ids": ["dataset-a"]}},
        "reasoning_option": "auto",
        "max_tokens": 1024,
        "temperature": 0.2,
        "memory_mode": "off",
        "memory_profile": "off",
    }
    assistant = await resolve_agent_launch(entrypoint="assistant", **common)
    responses = await resolve_agent_launch(entrypoint="responses", **common)
    assistant_payload = assistant.to_dict()
    responses_payload = responses.to_dict()

    assert assistant.identity["agent_id"] == "__builtin_assistant__"
    assert responses.identity["agent_id"] == "__builtin_assistant__"
    assistant_payload["identity"].pop("entrypoint")
    responses_payload["identity"].pop("entrypoint")
    assert assistant_payload == responses_payload


@pytest.mark.asyncio
async def test_resolver_rejects_model_identity_drift() -> None:
    class WrongModel:
        async def get_model(self, _tenant_id: str, _model_id: str):
            return {
                "model_id": "other-model",
                "provider_id": "provider-a",
                "is_enabled": True,
                "effective_capabilities": {},
            }

    with pytest.raises(AgentLaunchResolutionError, match="MODEL_MISMATCH"):
        await resolve_agent_launch(
            entrypoint="assistant",
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            model_id="model-a",
            model_service=WrongModel(),
        )


@pytest.mark.asyncio
async def test_provider_and_frozen_model_ref_are_checked_before_launch():
    calls = []

    class Models:
        async def get_model(self, tenant, model, *, provider_id):
            calls.append((tenant, model, provider_id))
            return {"model_id": model, "provider_id": provider_id, "is_enabled": True,
                    "effective_capabilities": {}, "capability_revision": 4,
                    "pricing_snapshot": {"version": "price-v1"}}

    ref = {"tenant_id": "a", "model_id": "m", "provider_id": "p2",
           "capability_revision": 4, "price_version": "price-v1"}
    params = {"entrypoint": "responses", "tenant_id": "a", "user_id": "u", "session_id": "s",
              "model_id": "m", "provider_id": "p2", "expected_model_ref": ref, "model_service": Models()}
    launch = await resolve_agent_launch(**params)
    assert launch.model["provider"] == "p2"
    assert calls == [("a", "m", "p2")]
    with pytest.raises(AgentLaunchResolutionError, match="EVAL_MODEL_REF_CHANGED"):
        await resolve_agent_launch(**{**params, "expected_model_ref": {**ref, "capability_revision": 3}})
