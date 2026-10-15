from __future__ import annotations

import copy
import hashlib
import json

import pytest
from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.eval.agent_version_candidate import (
    AgentReleaseProfileUnavailableError,
    build_agent_version_candidate,
    evaluate_agent_version_candidate,
    require_available_release_profile,
    server_release_profile,
)


def _plain_hash(value: object) -> str:
    encoded = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode()).hexdigest()


def _candidate() -> dict:
    policy = {"attachments": False, "high_risk_tools": False, "allowed_origins": []}
    return build_agent_version_candidate(
        resolution={
            "agent": {
                "tenant_id": "tenant-a",
                "agent_id": "11111111-1111-4111-8111-111111111111",
            },
            "draft": {
                "draft_id": "22222222-2222-4222-8222-222222222222",
                "revision": 1,
                "spec_hash": "a" * 64,
            },
        },
        runtime_snapshot={
            "schema_version": "agent-runtime/v1",
            "tenant_id": "tenant-a",
            "agent_id": "11111111-1111-4111-8111-111111111111",
            "agent_version_id": None,
            "publication": {"id": None, "channel": "api", "auth_mode": "private"},
            "model": {"id": "qwen3.7-plus", "provider": "dashscope"},
            "instructions": {"prompt_hash": runtime_sha256("release prompt")},
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
        },
        channel="api",
        auth_mode="private",
        channel_policy=policy,
        dataset_id=None,
    )


def test_offline_profile_passes_only_release_integrity_and_is_truthful(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("AGENT_RELEASE_PROFILE", raising=False)

    gate = evaluate_agent_version_candidate(_candidate())

    assert gate["status"] == "passed"
    assert gate["execution_scope"] == "provider_free_release_integrity"
    assert gate["model_quality_evaluated"] is False
    assert gate["metrics"]["critical_pass_rate"] == 1.0
    assert gate["metrics"]["provider_cost_cents"] == 0.0
    assert any(
        item["code"] == "AGENT_EVAL_DATASET_NOT_SELECTED"
        for item in gate["non_blocking_findings"]
    )


def test_fingerprint_tampering_is_always_blocking() -> None:
    candidate = copy.deepcopy(_candidate())
    candidate["runtime_fingerprint"]["model_id"] = "forged"

    gate = evaluate_agent_version_candidate(candidate)

    assert gate["status"] == "failed"
    assert gate["metrics"]["critical_pass_rate"] == 0.0
    assert {item["code"] for item in gate["blocking_findings"]} == {
        "AGENT_EVAL_FINGERPRINT_HASH_MISMATCH"
    }


def test_missing_runtime_dimension_is_blocking() -> None:
    candidate = copy.deepcopy(_candidate())
    candidate["runtime_fingerprint"]["knowledge_revision"] = ""
    candidate["runtime_fingerprint_hash"] = _plain_hash(candidate["runtime_fingerprint"])

    gate = evaluate_agent_version_candidate(candidate)

    assert gate["status"] == "failed"
    assert gate["blocking_findings"][0]["field"] == (
        "runtime_fingerprint.knowledge_revision"
    )


def test_unconfigured_production_profile_fails_closed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AGENT_RELEASE_PROFILE", "production_v1")

    assert server_release_profile()["available"] is False
    with pytest.raises(
        AgentReleaseProfileUnavailableError,
        match="AGENT_RELEASE_PROFILE_UNAVAILABLE",
    ):
        require_available_release_profile()
