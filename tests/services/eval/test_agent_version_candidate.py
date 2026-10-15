from __future__ import annotations

import copy

import pytest
from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.eval.agent_version_candidate import (
    AgentReleaseCandidateError,
    build_agent_version_candidate,
    build_model_authorization_evidence,
    structured_agent_release_diff,
)


def _resolution() -> dict:
    return {
        "agent": {
            "tenant_id": "tenant-a",
            "agent_id": "11111111-1111-4111-8111-111111111111",
        },
        "draft": {
            "draft_id": "22222222-2222-4222-8222-222222222222",
            "revision": 7,
            "spec_hash": "a" * 64,
        },
    }


def _snapshot() -> dict:
    policy = {"attachments": False, "high_risk_tools": False, "allowed_origins": []}
    return {
        "schema_version": "agent-runtime/v1",
        "tenant_id": "tenant-a",
        "agent_id": "11111111-1111-4111-8111-111111111111",
        "agent_version_id": None,
        "publication": {"id": None, "channel": "hosted", "auth_mode": "private"},
        "model": {
            "id": "qwen3.7-plus",
            "provider": "dashscope",
            "parameters": {"temperature": 0.2},
        },
        "instructions": {
            "agent": "Never copy this prompt into release evidence.",
            "prompt_hash": runtime_sha256("Never copy this prompt into release evidence."),
        },
        "capabilities": [],
        "knowledge": {"datasets": [], "retrieval": {}},
        "memory": {"mode": "session"},
        "channel_policy": policy,
        "fingerprints": {
            "spec": "sha256:" + "a" * 64,
            "tool_schema": runtime_sha256([]),
            "skills": runtime_sha256([]),
            "knowledge_revision": runtime_sha256({}),
        },
    }


def _candidate() -> dict:
    return build_agent_version_candidate(
        resolution=_resolution(),
        runtime_snapshot=_snapshot(),
        channel="hosted",
        auth_mode="private",
        channel_policy={
            "attachments": False,
            "high_risk_tools": False,
            "allowed_origins": [],
        },
        dataset_id=None,
    )


def test_candidate_is_exact_deterministic_and_prompt_free() -> None:
    first = _candidate()
    second = _candidate()

    assert first == second
    assert first["draft_revision"] == 7
    assert first["spec_hash"] == "a" * 64
    assert len(first["runtime_fingerprint_hash"]) == 64
    assert len(first["release_identity_hash"]) == 64
    assert "Never copy" not in repr(first)
    assert first["runtime_fingerprint"]["prompt_hash"].startswith("sha256:")


def test_channel_policy_changes_runtime_gate_but_not_version_identity() -> None:
    first = _candidate()
    snapshot = _snapshot()
    snapshot["channel_policy"] = {
        "attachments": True,
        "high_risk_tools": False,
        "allowed_origins": [],
    }
    second = build_agent_version_candidate(
        resolution=_resolution(),
        runtime_snapshot=snapshot,
        channel="hosted",
        auth_mode="private",
        channel_policy=snapshot["channel_policy"],
        dataset_id=None,
    )

    assert first["channel_policy_hash"] != second["channel_policy_hash"]
    assert first["runtime_fingerprint_hash"] != second["runtime_fingerprint_hash"]
    assert first["release_identity_hash"] == second["release_identity_hash"]


def test_public_expiry_is_bound_to_release_fingerprint_and_normalized_to_utc() -> None:
    snapshot = _snapshot()
    snapshot["publication"]["auth_mode"] = "public"
    snapshot["channel_policy"]["expires_at"] = "2027-01-01T05:00:00+05:00"
    first = build_agent_version_candidate(
        resolution=_resolution(),
        runtime_snapshot=snapshot,
        channel="hosted",
        auth_mode="public",
        channel_policy=snapshot["channel_policy"],
        dataset_id=None,
    )
    normalized = copy.deepcopy(snapshot)
    normalized["channel_policy"]["expires_at"] = "2027-01-01T00:00:00Z"
    second = build_agent_version_candidate(
        resolution=_resolution(),
        runtime_snapshot=normalized,
        channel="hosted",
        auth_mode="public",
        channel_policy=normalized["channel_policy"],
        dataset_id=None,
    )
    assert first["channel_policy_hash"] == second["channel_policy_hash"]
    assert first["channel_policy"]["expires_at"] == "2027-01-01T00:00:00+00:00"

    later = copy.deepcopy(snapshot)
    later["channel_policy"]["expires_at"] = "2027-01-02T00:00:00Z"
    third = build_agent_version_candidate(
        resolution=_resolution(),
        runtime_snapshot=later,
        channel="hosted",
        auth_mode="public",
        channel_policy=later["channel_policy"],
        dataset_id=None,
    )
    assert first["runtime_fingerprint_hash"] != third["runtime_fingerprint_hash"]


def test_eval_dataset_content_and_model_authorization_bind_release_identity() -> None:
    model_authorization = build_model_authorization_evidence(
        source="database",
        model_id="qwen3.7-plus",
        provider_id="dashscope",
        access_level="public",
        model_enabled=True,
        provider_enabled=True,
        runtime_provider_configured=True,
        model_updated_at="2026-07-19T08:00:00+00:00",
        provider_updated_at="2026-07-19T08:00:00+00:00",
    )
    dataset = {
        "dataset_id": "33333333-3333-4333-8333-333333333333",
        "tenant_id": "tenant-a",
        "version": "release-v3",
        "manifest_hash": "7" * 64,
    }
    first = build_agent_version_candidate(
        resolution=_resolution(),
        runtime_snapshot=_snapshot(),
        channel="hosted",
        auth_mode="private",
        channel_policy=_snapshot()["channel_policy"],
        dataset_id=dataset["dataset_id"],
        dataset_snapshot=dataset,
        model_authorization=model_authorization,
    )
    changed_dataset = {**dataset, "manifest_hash": "8" * 64}
    second = build_agent_version_candidate(
        resolution=_resolution(),
        runtime_snapshot=_snapshot(),
        channel="hosted",
        auth_mode="private",
        channel_policy=_snapshot()["channel_policy"],
        dataset_id=dataset["dataset_id"],
        dataset_snapshot=changed_dataset,
        model_authorization=model_authorization,
    )
    changed_model_authorization = build_model_authorization_evidence(
        source="database",
        model_id="qwen3.7-plus",
        provider_id="dashscope",
        access_level="premium",
        model_enabled=True,
        provider_enabled=True,
        runtime_provider_configured=True,
        model_updated_at="2026-07-19T09:00:00+00:00",
        provider_updated_at="2026-07-19T08:00:00+00:00",
    )
    third = build_agent_version_candidate(
        resolution=_resolution(),
        runtime_snapshot=_snapshot(),
        channel="hosted",
        auth_mode="private",
        channel_policy=_snapshot()["channel_policy"],
        dataset_id=dataset["dataset_id"],
        dataset_snapshot=dataset,
        model_authorization=changed_model_authorization,
    )

    assert first["dataset_version"] == "release-v3"
    assert first["dataset_manifest_hash"] == "7" * 64
    assert first["runtime_fingerprint_hash"] != second["runtime_fingerprint_hash"]
    assert first["release_identity_hash"] != second["release_identity_hash"]
    assert first["evaluation_identity_hash"] != second["evaluation_identity_hash"]
    assert first["runtime_fingerprint_hash"] != third["runtime_fingerprint_hash"]
    assert first["release_identity_hash"] != third["release_identity_hash"]


def test_selected_eval_dataset_requires_matching_tenant_snapshot() -> None:
    with pytest.raises(AgentReleaseCandidateError, match="DATASET_TENANT_MISMATCH"):
        build_agent_version_candidate(
            resolution=_resolution(),
            runtime_snapshot=_snapshot(),
            channel="hosted",
            auth_mode="private",
            channel_policy=_snapshot()["channel_policy"],
            dataset_id="33333333-3333-4333-8333-333333333333",
            dataset_snapshot={
                "dataset_id": "33333333-3333-4333-8333-333333333333",
                "tenant_id": "tenant-b",
                "version": "v1",
                "manifest_hash": "7" * 64,
            },
        )


@pytest.mark.parametrize(
    ("mutation", "code"),
    [
        (lambda value: value["fingerprints"].update(spec="sha256:" + "b" * 64), "SPEC_HASH"),
        (lambda value: value.update(agent_id="33333333-3333-4333-8333-333333333333"), "AGENT_ID"),
        (lambda value: value.update(tenant_id="tenant-b"), "TENANT_ID"),
        (lambda value: value["publication"].update(channel="api"), "CHANNEL_MISMATCH"),
        (lambda value: value["publication"].update(auth_mode="token"), "AUTH_MODE"),
        (lambda value: value.update(schema_version="agent-runtime/v0"), "RUNTIME_VERSION"),
    ],
)
def test_candidate_rejects_detached_runtime_identity(mutation, code: str) -> None:
    snapshot = copy.deepcopy(_snapshot())
    mutation(snapshot)

    with pytest.raises(AgentReleaseCandidateError, match=code):
        build_agent_version_candidate(
            resolution=_resolution(),
            runtime_snapshot=snapshot,
            channel="hosted",
            auth_mode="private",
            channel_policy=snapshot["channel_policy"],
            dataset_id=None,
        )


def test_candidate_rejects_runtime_that_silently_drops_an_enabled_capability() -> None:
    resolution = _resolution()
    resolution["capabilities"] = [
        {
            "capability_type": "native",
            "resource_id": "web_fetch",
            "resource_version": "1",
            "schema_hash": "b" * 64,
            "config": {"risk": "low"},
        }
    ]

    with pytest.raises(AgentReleaseCandidateError, match="CAPABILITY_SET_MISMATCH"):
        build_agent_version_candidate(
            resolution=resolution,
            runtime_snapshot=_snapshot(),
            channel="hosted",
            auth_mode="private",
            channel_policy={
                "attachments": False,
                "high_risk_tools": False,
                "allowed_origins": [],
            },
            dataset_id=None,
        )


def test_candidate_allows_channel_policy_to_disable_a_bound_high_risk_tool() -> None:
    resolution = _resolution()
    resolution["capabilities"] = [
        {
            "capability_type": "mcp",
            "resource_id": "write_ticket",
            "resource_version": "1",
            "schema_hash": "c" * 64,
            "config": {"risk": "high"},
        }
    ]

    candidate = build_agent_version_candidate(
        resolution=resolution,
        runtime_snapshot=_snapshot(),
        channel="hosted",
        auth_mode="private",
        channel_policy={
            "attachments": False,
            "high_risk_tools": False,
            "allowed_origins": [],
        },
        dataset_id=None,
    )

    assert candidate["channel"] == "hosted"


def test_structured_diff_never_returns_prompt_body() -> None:
    before = {
        "instructions": "old secret-shaped prompt body",
        "model": {"model_id": "old", "provider_id": "dashscope"},
        "capabilities": [],
        "knowledge": [],
    }
    after = {
        "instructions": "new secret-shaped prompt body",
        "model": {"model_id": "new", "provider_id": "dashscope"},
        "capabilities": [
            {"type": "native", "resource_id": "web_fetch", "config": {"risk": "low"}}
        ],
        "knowledge": [],
    }

    diff = structured_agent_release_diff(before, after)

    assert set(diff["changed_sections"]) >= {"prompt", "model", "capabilities"}
    assert diff["sections"]["prompt"]["before_length"] == len(before["instructions"])
    assert "secret-shaped" not in repr(diff)
