from __future__ import annotations

import copy
import hashlib
import uuid
from dataclasses import replace
from typing import Any

from fastapi import FastAPI
from fastapi.testclient import TestClient

from src.api.deps import AuthContext, get_auth_context, get_user_context
from src.api.v1.skills import router
from src.core.auth.user_resolver import UserContext


def _skill_md(*, extra: str = "", instruction: str = "Follow the report template.") -> str:
    return f"""---
name: report-helper
title: Report Helper
description: Builds a concise report
version: 1.0.0
generated: false
enabled: true
{extra}---
# Instructions
{instruction}
"""


class InMemorySkillRepository:
    def __init__(self) -> None:
        self.records: dict[tuple[str, str, str], dict[str, Any]] = {}
        self.fail_writes = False

    async def create_version(self, **values: Any) -> dict[str, Any]:
        if self.fail_writes:
            raise RuntimeError("database unavailable")
        key = (values["tenant_id"], values["user_id"], values["manifest"].name)
        previous = self.records.get(key)
        skill_id = str(previous["skill_id"]) if previous else str(uuid.uuid4())
        version_id = str(uuid.uuid4())
        manifest = replace(
            values["manifest"],
            skill_id=skill_id,
            version_id=version_id,
            entrypoint=f"db://{skill_id}/{version_id}",
            content_hash=hashlib.sha256(
                values["content"].encode("utf-8")
            ).hexdigest(),
            artifact_type="tenant_instruction",
        )
        record = {
            **manifest.to_dict(),
            "content": values["content"],
            "revision": int(previous["revision"]) + 1 if previous else 1,
            "status": "active" if manifest.enabled else "disabled",
            "revoked": False,
        }
        self.records[key] = copy.deepcopy(record)
        return copy.deepcopy(record)

    async def list_for_actor(self, **values: Any) -> list[dict[str, Any]]:
        rows = [
            copy.deepcopy(record)
            for (tenant, user, _name), record in self.records.items()
            if tenant == values["tenant_id"]
            and user == values["user_id"]
            and (not values["enabled_only"] or record["enabled"])
        ]
        return sorted(rows, key=lambda row: row["name"])

    async def get_for_actor(self, **values: Any) -> dict[str, Any]:
        key = (values["tenant_id"], values["user_id"], values["name"])
        if key not in self.records:
            from ai_gateway_core.skills import SkillArtifactNotFoundError

            raise SkillArtifactNotFoundError("SKILL_NOT_FOUND")
        return copy.deepcopy(self.records[key])

    async def update_metadata(self, **values: Any) -> dict[str, Any]:
        current = await self.get_for_actor(**values)
        manifest_values = {
            key: current[key]
            for key in (
                "name",
                "title",
                "description",
                "entrypoint",
                "summary",
                "version",
                "tags",
                "permissions",
                "enabled",
                "instructions",
                "config",
                "source",
                "max_context_tokens",
                "author",
                "generated",
                "lifecycle_status",
                "review",
                "evaluation",
                "rollback",
                "artifact_type",
            )
            if key in current
        }
        from ai_gateway_core.skills import (
            SkillManifest,
            SkillSource,
            serialize_user_skill_md,
        )

        manifest_values["source"] = SkillSource.USER
        manifest = SkillManifest(**manifest_values)
        manifest = replace(manifest, **values["changes"])
        return await self.create_version(
            tenant_id=values["tenant_id"],
            user_id=values["user_id"],
            content=serialize_user_skill_md(manifest),
            manifest=manifest,
            created_by=values["updated_by"],
        )

    async def set_enabled(self, **values: Any) -> dict[str, Any]:
        if self.fail_writes:
            raise RuntimeError("database unavailable")
        row = await self.get_for_actor(**values)
        row["enabled"] = values["enabled"]
        row["status"] = "active" if values["enabled"] else "disabled"
        self.records[(values["tenant_id"], values["user_id"], values["name"])] = row
        return copy.deepcopy(row)

    async def delete(self, **values: Any) -> None:
        if self.fail_writes:
            raise RuntimeError("database unavailable")
        await self.get_for_actor(**values)
        del self.records[(values["tenant_id"], values["user_id"], values["name"])]


def _user(tenant_id: str = "tenant-a", user_id: str = "user-a") -> UserContext:
    return UserContext(
        tenant_id=tenant_id,
        user_id=user_id,
        is_authenticated=True,
        roles=["admin"],
    )


def _client(
    repository: InMemorySkillRepository | None = None,
    *,
    user: UserContext | None = None,
) -> tuple[TestClient, InMemorySkillRepository]:
    app = FastAPI()
    app.include_router(router)
    app.state.skill_artifact_repository = repository or InMemorySkillRepository()
    actor = user or _user()
    auth = AuthContext(
        user_id=actor.user_id,
        tenant_id=actor.tenant_id,
        roles=["admin"],
        permissions=["console:skills:view", "console:skills:edit"],
        is_authenticated=True,
    )
    app.dependency_overrides[get_user_context] = lambda: actor
    app.dependency_overrides[get_auth_context] = lambda: auth
    return TestClient(app), app.state.skill_artifact_repository


def _upload(client: TestClient, content: str | None = None):
    return client.post(
        "/skills/upload",
        files={"file": ("SKILL.md", content or _skill_md(), "text/markdown")},
    )


def _listed_names(client: TestClient) -> list[str]:
    return [item["name"] for item in client.get("/skills").json()["skills"]]


def test_valid_upload_round_trips_full_content_with_server_owned_identity() -> None:
    client, _repository = _client()
    response = _upload(client)

    assert response.status_code == 201, response.text
    artifact = response.json()
    assert artifact["entrypoint"] == (
        f"db://{artifact['skill_id']}/{artifact['version_id']}"
    )
    assert "Follow the report template." in artifact["content"]
    assert "Follow the report template." in artifact["instructions"]
    assert artifact["source"] == "user"
    assert artifact["artifact_type"] == "tenant_instruction"


def test_persistence_failure_is_reported_and_does_not_create_catalog_entry() -> None:
    client, repository = _client()
    repository.fail_writes = True

    response = _upload(client)

    assert response.status_code == 503
    assert response.json()["detail"]["code"] == "SKILL_STORAGE_UNAVAILABLE"
    assert repository.records == {}


def test_list_get_and_delete_are_tenant_user_scoped() -> None:
    repository = InMemorySkillRepository()
    owner, _ = _client(repository)
    other_tenant, _ = _client(
        repository,
        user=_user("tenant-b", "user-b"),
    )
    assert _upload(owner).status_code == 201
    assert _listed_names(other_tenant) == ["skill-create"]
    assert other_tenant.get("/skills/report-helper").status_code == 404

    detail = owner.get("/skills/report-helper")
    assert detail.status_code == 200
    assert "# Instructions" in detail.json()["content"]
    assert owner.delete("/skills/report-helper").status_code == 200
    assert owner.get("/skills/report-helper").status_code == 404


def test_update_and_disable_are_persisted_as_honest_state() -> None:
    client, _repository = _client()
    artifact = _upload(client).json()

    updated = client.patch(
        "/skills/report-helper",
        json={"title": "Updated Title"},
    )
    assert updated.status_code == 200
    assert updated.json()["title"] == "Updated Title"
    assert updated.json()["version_id"] != artifact["version_id"]
    assert updated.json()["content_hash"] != artifact["content_hash"]
    assert "title: Updated Title" in updated.json()["content"]

    disabled = client.post("/skills/report-helper/disable")
    assert disabled.status_code == 200
    assert disabled.json()["enabled"] is False
    assert _listed_names(client) == ["skill-create"]

    reenabled = client.patch(
        "/skills/report-helper",
        json={"title": "Re-enabled Version", "enabled": True},
    )
    assert reenabled.status_code == 200
    assert reenabled.json()["enabled"] is True
    assert reenabled.json()["status"] == "active"
    assert _listed_names(client) == ["skill-create", "report-helper"]


def test_update_cannot_bypass_permission_policy_or_store_null_metadata() -> None:
    client, _repository = _client()
    original = _upload(client).json()

    dangerous = client.patch(
        "/skills/report-helper",
        json={"permissions": ["exec:shell"]},
    )
    invalid_null = client.patch(
        "/skills/report-helper",
        json={"description": None},
    )

    assert dangerous.status_code == 422
    assert dangerous.json()["detail"]["code"] == "SKILL_MANIFEST_INVALID"
    assert invalid_null.status_code == 422
    detail = client.get("/skills/report-helper").json()
    assert detail["version_id"] == original["version_id"]
    assert detail["permissions"] == []


def test_catalog_list_omits_full_instruction_payload_but_detail_keeps_it() -> None:
    client, _repository = _client()
    assert _upload(client).status_code == 201

    listed = next(
        item
        for item in client.get("/skills").json()["skills"]
        if item["name"] == "report-helper"
    )
    assert "content" not in listed
    assert "instructions" not in listed
    detail = client.get("/skills/report-helper").json()
    assert detail["content"]
    assert detail["instructions"]


def test_builtin_skill_create_remains_listed_readable_and_testable() -> None:
    client, _repository = _client()

    listed = client.get("/skills")
    assert listed.status_code == 200
    platform = next(
        item for item in listed.json()["skills"] if item["name"] == "skill-create"
    )
    assert platform["source"] == "builtin"
    assert platform["artifact_type"] == "bundled"
    assert "instructions" not in platform

    detail = client.get("/skills/skill-create")
    assert detail.status_code == 200
    assert "propose-review-test-enable" in detail.json()["instructions"]

    tested = client.post(
        "/skills/skill-create/test",
        json={"input": "Create a release-note Skill"},
    )
    assert tested.status_code == 200
    assert tested.json()["success"] is True
    assert "release-note" in tested.json()["result"]


def test_builtin_skill_create_is_read_only_through_tenant_artifact_api() -> None:
    client, repository = _client()

    responses = [
        client.patch("/skills/skill-create", json={"title": "forged"}),
        client.post("/skills/skill-create/disable"),
        client.post("/skills/skill-create/enable"),
        client.delete("/skills/skill-create"),
    ]

    assert [response.status_code for response in responses] == [404, 404, 404, 404]
    assert repository.records == {}
