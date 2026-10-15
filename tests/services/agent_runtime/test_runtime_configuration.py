from __future__ import annotations

import pytest

from src.services.agent_runtime.runtime_configuration import (
    RuntimePlatformConfigError,
    build_runtime_platform_config,
    runtime_platform_config_hash,
)


def _snapshot() -> dict:
    return {
        "agent_spec": {"developerInstructions": "stable platform instructions"},
        "capabilities": [
            {
                "type": "skill",
                "id": "skill.audit",
                "version": "v3",
                "schema_hash": "sha256:" + "a" * 64,
                "config": {
                    "content_hash": "b" * 64,
                    "permissions": ["read:ledger"],
                    "manifest": {"name": "audit", "entrypoint": "db://skill/v3"},
                },
            },
            {
                "type": "connector",
                "id": "docs.read",
                "config": {
                    "provider": "docs",
                    "grant_id": "grant-1",
                    "principal_type": "user_delegated",
                },
            },
        ],
    }


def test_platform_config_freezes_skill_grant_plugin_and_attachment_identity() -> None:
    config = build_runtime_platform_config(
        _snapshot(),
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        attachment_refs=["att-2", "att-1", "att-1"],
    )
    assert [(item["id"], item["version"], item["content_hash"]) for item in config["skills"]] == [
        ("skill.audit", "v3", "sha256:" + "b" * 64)
    ]
    assert config["tenant_grants"][0]["grant_id"] == "grant-1"
    assert [item["ref"] for item in config["attachments"]] == ["att-1", "att-2"]
    assert config["instructions"]["dynamic_data_after_instructions"] is True
    assert runtime_platform_config_hash(config) == runtime_platform_config_hash(dict(config))


def test_platform_config_rejects_secret_metadata_and_missing_skill_hash() -> None:
    bad = _snapshot()
    bad["capabilities"][1]["config"]["api_token"] = "secret"
    with pytest.raises(RuntimePlatformConfigError):
        build_runtime_platform_config(
            bad, tenant_id="tenant-a", user_id="user-a", session_id="session-a"
        )
    missing = _snapshot()
    del missing["capabilities"][0]["schema_hash"]
    del missing["capabilities"][0]["config"]["content_hash"]
    with pytest.raises(RuntimePlatformConfigError):
        build_runtime_platform_config(
            missing, tenant_id="tenant-a", user_id="user-a", session_id="session-a"
        )
