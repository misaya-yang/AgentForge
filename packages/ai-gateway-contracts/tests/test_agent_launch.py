from __future__ import annotations

import ast
from pathlib import Path

import pytest
from ai_gateway_contracts.agent_launch import (
    RESOLVED_AGENT_LAUNCH_SCHEMA_VERSION,
    ResolvedAgentLaunchError,
    ResolvedAgentLaunchV1,
)
from ai_gateway_contracts.agent_runtime import runtime_sha256


def _payload(*, entrypoint: str = "assistant") -> dict:
    identity = {
        "tenant_id": "tenant-a",
        "user_id": "user-a",
        "session_id": "session-a",
        "agent_id": "__builtin_assistant__",
        "agent_version_id": None,
        "draft_revision": None,
        "publication_id": None,
        "channel": "builtin",
        "entrypoint": entrypoint,
        "auth_mode": "private",
    }
    if entrypoint == "studio_preview":
        identity.update(
            agent_id="agent-a",
            agent_version_id="version-a",
            channel="preview",
        )
    elif entrypoint == "published_agent":
        identity.update(
            agent_id="agent-a",
            agent_version_id="version-a",
            publication_id="publication-a",
            channel="api",
            auth_mode="token",
        )
    model = {"id": "model-a", "provider": "provider-a", "parameters": {}}
    capabilities: list[dict] = []
    knowledge = {
        "datasets": ["dataset-a"],
        "retrieval": {"mode": "tool", "top_k": 3, "threshold": 0.5},
    }
    agent_spec = {
        "agentId": identity["agent_id"],
        "agentVersionId": identity["agent_version_id"],
        "channel": identity["channel"],
        "developerInstructions": "Answer with evidence.",
        "model": model,
        "knowledge": knowledge,
        "capabilities": capabilities,
        "memory": {"mode": "session"},
    }
    return {
        "schema_version": RESOLVED_AGENT_LAUNCH_SCHEMA_VERSION,
        "identity": identity,
        "agent_spec": agent_spec,
        "model": model,
        "model_profile": {"wire_protocols": {"preferred": "responses_v1"}},
        "capability_bindings": capabilities,
        "knowledge_bindings": knowledge,
        "memory_policy": {"mode": "auto", "profile": "basic"},
        "channel_policy": {
            "attachments": True,
            "high_risk_tools": True,
            "allowed_origins": [],
        },
        "runtime_inputs": {
            "readonly_capabilities": {
                "knowledge": {"dataset_ids": ["dataset-a"]}
            }
        },
        "turn_policy": {
            "reasoning_option": "auto",
            "legacy_thinking_level": None,
            "max_tokens": 1024,
            "temperature": 0.2,
            "style_guidance": None,
            "memory_mode": "auto",
            "memory_profile": "basic",
            "enable_dynamic_tools": True,
        },
        "fingerprints": {
            "spec": runtime_sha256(agent_spec),
            "tool_schema": runtime_sha256(capabilities),
            "skills": runtime_sha256([]),
            "knowledge_revision": runtime_sha256(knowledge),
        },
    }


@pytest.mark.parametrize(
    "entrypoint",
    ["assistant", "responses", "studio_preview", "published_agent"],
)
def test_all_entrypoints_share_one_launch_identity_contract(entrypoint: str) -> None:
    launch = ResolvedAgentLaunchV1.parse(_payload(entrypoint=entrypoint))

    assert launch.identity["entrypoint"] == entrypoint
    assert launch.to_control_snapshot()["agent_spec"] == launch.to_dict()["agent_spec"]


def test_launch_is_immutable_and_accessors_are_defensive() -> None:
    payload = _payload()
    launch = ResolvedAgentLaunchV1.parse(payload)
    payload["identity"]["tenant_id"] = "tampered"
    projected = launch.to_dict()
    projected["identity"]["tenant_id"] = "also-tampered"

    assert launch.identity["tenant_id"] == "tenant-a"
    assert launch.model["id"] == "model-a"


def test_launch_direct_constructor_cannot_bypass_validation() -> None:
    with pytest.raises(
        ResolvedAgentLaunchError, match="RESOLVED_AGENT_LAUNCH_INVALID"
    ):
        ResolvedAgentLaunchV1("{}")


def test_legacy_snapshot_adapter_preserves_control_projection() -> None:
    original = ResolvedAgentLaunchV1.parse(_payload(entrypoint="published_agent"))
    snapshot = original.to_control_snapshot()
    restored = ResolvedAgentLaunchV1.from_legacy_snapshot(
        snapshot,
        user_id="user-a",
        session_id="session-a",
        entrypoint="published_agent",
        model_profile=original.model_profile,
        readonly_capabilities=original.runtime_inputs["readonly_capabilities"],
        turn_policy=original.turn_policy,
    )

    assert restored.to_control_snapshot() == snapshot


def test_publication_expiry_is_gateway_metadata_outside_the_runtime_wire():
    payload = _payload(entrypoint="published_agent")
    payload["identity"].update(channel="hosted", auth_mode="public")
    payload["agent_spec"]["channel"] = "hosted"
    payload["fingerprints"]["spec"] = runtime_sha256(payload["agent_spec"])
    original = ResolvedAgentLaunchV1.parse(payload)
    snapshot = original.to_control_snapshot()
    snapshot["channel_policy"]["expires_at"] = "2027-01-01T00:00:00+00:00"
    restored = ResolvedAgentLaunchV1.from_legacy_snapshot(
        snapshot, user_id="user-a", session_id="session-a", entrypoint="published_agent",
        model_profile=original.model_profile,
        readonly_capabilities=original.runtime_inputs["readonly_capabilities"],
        turn_policy=original.turn_policy,
    )
    assert restored.to_control_snapshot()["channel_policy"] == payload["channel_policy"]
    assert snapshot["channel_policy"]["expires_at"] == "2027-01-01T00:00:00+00:00"
    snapshot["channel_policy"]["unexpected_permission"] = True
    with pytest.raises(ResolvedAgentLaunchError, match="RESOLVED_AGENT_LAUNCH_CHANNEL_INVALID"):
        ResolvedAgentLaunchV1.from_legacy_snapshot(
            snapshot, user_id="user-a", session_id="session-a", entrypoint="published_agent",
            model_profile=original.model_profile, readonly_capabilities={}, turn_policy=original.turn_policy,
        )
    payload["channel_policy"]["expires_at"] = "2027-01-01T00:00:00+00:00"
    with pytest.raises(ResolvedAgentLaunchError, match="RESOLVED_AGENT_LAUNCH_CHANNEL_INVALID"):
        ResolvedAgentLaunchV1.parse(payload)


@pytest.mark.parametrize(
    ("mutation", "code"),
    [
        (
            lambda value: value.update(extra=True),
            "RESOLVED_AGENT_LAUNCH_INVALID",
        ),
        (
            lambda value: value["identity"].update(agent_version_id="version-a"),
            "RESOLVED_AGENT_LAUNCH_IDENTITY_INVALID",
        ),
        (
            lambda value: value["agent_spec"].update(agentId="other-agent"),
            "RESOLVED_AGENT_LAUNCH_AGENT_SPEC_INVALID",
        ),
        (
            lambda value: value["runtime_inputs"].update(
                readonly_capabilities={"api_key": "forbidden"}
            ),
            "RESOLVED_AGENT_LAUNCH_SECRET_FORBIDDEN",
        ),
        (
            lambda value: value["fingerprints"].update(spec="sha256:abc"),
            "RESOLVED_AGENT_LAUNCH_FINGERPRINT_INVALID",
        ),
    ],
)
def test_launch_rejects_unknown_scope_and_secret_mutations(mutation, code: str) -> None:
    payload = _payload()
    mutation(payload)

    with pytest.raises(ResolvedAgentLaunchError, match=code):
        ResolvedAgentLaunchV1.parse(payload)


def test_launch_contract_module_has_only_pure_imports() -> None:
    source = Path(__file__).parents[1] / "src/ai_gateway_contracts/agent_launch.py"
    tree = ast.parse(source.read_text(encoding="utf-8"))
    forbidden = {
        "asyncpg",
        "fastapi",
        "httpx",
        "redis",
        "requests",
        "sqlalchemy",
        "starlette",
    }
    imported = {
        alias.name.split(".")[0]
        for node in ast.walk(tree)
        if isinstance(node, ast.Import)
        for alias in node.names
    }
    imported.update(
        str(node.module or "").split(".")[0]
        for node in ast.walk(tree)
        if isinstance(node, ast.ImportFrom) and node.level == 0
    )

    assert imported.isdisjoint(forbidden)
