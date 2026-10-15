from __future__ import annotations

import copy
import hashlib
import json
import uuid
from datetime import datetime, timezone
from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentArchivedError,
    AgentDraftConflictError,
    AgentLastOwnerError,
    AgentNotFoundError,
    AgentRepositoryError,
    AgentRuntimeUnavailableError,
    AgentValidationError,
    hash_agent_spec,
    sanitize_agent_copy_spec,
    validate_agent_spec,
)
from fastapi import FastAPI
from fastapi.testclient import TestClient

from src.api.deps import get_user_context
from src.api.v1.agent_runtime import router as runtime_router
from src.api.v1.agents import router
from src.core.auth.user_resolver import UserContext

ROLE_RANK = {"viewer": 1, "editor": 2, "owner": 3}


def valid_spec(instructions: str = "Answer clearly") -> dict[str, Any]:
    return {
        "schema_version": "agent-spec/v1",
        "identity": {
            "welcome_message": "Hello",
            "suggested_prompts": ["Help me"],
        },
        "instructions": instructions,
        "model": {"model_id": "qwen3.7-plus", "max_tokens": 2048},
        "capabilities": [],
        "knowledge": [],
        "memory": {},
    }


def make_user(
    user_id: str = "owner-a",
    tenant_id: str = "tenant-a",
    *roles: str,
    authenticated: bool = True,
) -> UserContext:
    return UserContext(
        user_id=user_id,
        tenant_id=tenant_id,
        tier="admin" if "admin" in roles else "normal",
        is_authenticated=authenticated,
        roles=list(roles) or ["user"],
        ip="127.0.0.1",
    )


class InMemoryAgentRepository:
    """Behavioral test double preserving the production ACL/error contract."""

    def __init__(self) -> None:
        self.records: dict[tuple[str, str], dict[str, Any]] = {}
        self.next_draft_error: AgentRepositoryError | AgentValidationError | None = None

    @staticmethod
    def _now() -> str:
        return datetime.now(timezone.utc).isoformat()

    @staticmethod
    def _admin(kwargs: dict[str, Any]) -> bool:
        return bool(kwargs.get("is_tenant_admin"))

    def _record(self, kwargs: dict[str, Any], required: str) -> tuple[dict[str, Any], str]:
        key = (kwargs["tenant_id"], str(kwargs["agent_id"]))
        record = self.records.get(key)
        if not record or record["agent"].get("deleted_at"):
            raise AgentNotFoundError("AGENT_NOT_FOUND")
        role = "owner" if self._admin(kwargs) else record["members"].get(kwargs["user_id"])
        if ROLE_RANK.get(role or "", 0) < ROLE_RANK[required]:
            raise AgentNotFoundError("AGENT_NOT_FOUND")
        return record, role or "viewer"

    @staticmethod
    def _summary(record: dict[str, Any], role: str) -> dict[str, Any]:
        result = copy.deepcopy(record["agent"])
        result["caller_role"] = role
        result["draft_revision"] = record["draft"]["revision"]
        result["published_channels"] = sorted(
            {
                publication.get("channel")
                for publication in record["publications"]
                if publication.get("status") == "active" and publication.get("channel")
            }
        )
        return result

    async def create_agent(self, **kwargs: Any) -> dict[str, Any]:
        agent_id = str(uuid.uuid4())
        draft_id = str(uuid.uuid4())
        timestamp = self._now()
        spec = copy.deepcopy(kwargs["spec"])
        slug = (kwargs.get("slug") or kwargs["name"]).lower().replace(" ", "-")
        if any(
            tenant_id == kwargs["tenant_id"]
            and record["agent"]["slug"] == slug
            and not record["agent"].get("deleted_at")
            for (tenant_id, _), record in self.records.items()
        ):
            exc = RuntimeError("duplicate slug")
            exc.sqlstate = "23505"  # type: ignore[attr-defined]
            raise exc
        record = {
            "agent": {
                "tenant_id": kwargs["tenant_id"],
                "agent_id": agent_id,
                "slug": slug,
                "name": kwargs["name"],
                "description": kwargs["description"],
                "owner_id": kwargs["user_id"],
                "status": "draft",
                "current_draft_id": draft_id,
                "archived_at": None,
                "deleted_at": None,
                "created_at": timestamp,
                "updated_at": timestamp,
            },
            "draft": {
                "tenant_id": kwargs["tenant_id"],
                "draft_id": draft_id,
                "agent_id": agent_id,
                "revision": 1,
                "schema_version": "agent-spec/v1",
                "spec": spec,
                "spec_hash": hash_agent_spec(spec),
                "updated_by": kwargs["user_id"],
                "created_at": timestamp,
                "updated_at": timestamp,
            },
            "members": {kwargs["user_id"]: "owner"},
            "member_created_by": {kwargs["user_id"]: kwargs["user_id"]},
            "versions": [],
            "publications": [],
            "tokens": [],
            "sessions": [],
            "memory": [],
        }
        self.records[(kwargs["tenant_id"], agent_id)] = record
        return self._summary(record, "owner")

    async def list_agents(self, **kwargs: Any) -> dict[str, Any]:
        items = []
        for (tenant_id, _), record in self.records.items():
            if tenant_id != kwargs["tenant_id"] or record["agent"].get("deleted_at"):
                continue
            role = "owner" if self._admin(kwargs) else record["members"].get(kwargs["user_id"])
            if not role:
                continue
            if kwargs.get("status") and record["agent"]["status"] != kwargs["status"]:
                continue
            if kwargs.get("owner_id") and record["agent"]["owner_id"] != kwargs["owner_id"]:
                continue
            if kwargs.get("search"):
                query = kwargs["search"].lower()
                if (
                    query not in record["agent"]["name"].lower()
                    and query not in record["agent"]["slug"]
                ):
                    continue
            items.append(self._summary(record, role))
        items.sort(key=lambda item: (item["updated_at"], item["agent_id"]), reverse=True)
        offset = int(kwargs.get("cursor") or 0)
        limit = kwargs["limit"]
        page = items[offset : offset + limit]
        next_cursor = str(offset + limit) if offset + limit < len(items) else None
        return {"items": page, "next_cursor": next_cursor}

    async def get_agent(self, **kwargs: Any) -> dict[str, Any]:
        record, role = self._record(kwargs, "viewer")
        result = self._summary(record, role)
        result["draft"] = {
            key: record["draft"][key]
            for key in ("revision", "schema_version", "spec_hash", "updated_at")
        }
        return result

    async def update_agent(self, **kwargs: Any) -> dict[str, Any]:
        record, role = self._record(kwargs, "editor")
        record["agent"].update(copy.deepcopy(kwargs["changes"]))
        record["agent"]["updated_at"] = self._now()
        return self._summary(record, role)

    async def get_draft(self, **kwargs: Any) -> dict[str, Any]:
        record, _ = self._record(kwargs, "viewer")
        return copy.deepcopy(record["draft"])

    async def update_draft(self, **kwargs: Any) -> dict[str, Any]:
        record, _ = self._record(kwargs, "editor")
        if record["agent"]["status"] == "archived":
            raise AgentArchivedError("AGENT_ARCHIVED")
        current = record["draft"]["revision"]
        if kwargs["expected_revision"] != current:
            raise AgentDraftConflictError(current)
        if self.next_draft_error is not None:
            error = self.next_draft_error
            self.next_draft_error = None
            raise error
        timestamp = self._now()
        agent_changes = {
            key: copy.deepcopy(kwargs.get("agent_changes", {})[key])
            for key in ("name", "description")
            if key in kwargs.get("agent_changes", {})
        }
        record["draft"]["revision"] += 1
        record["draft"]["spec"] = copy.deepcopy(kwargs["spec"])
        record["draft"]["spec_hash"] = hash_agent_spec(kwargs["spec"])
        record["draft"]["updated_by"] = kwargs["user_id"]
        record["draft"]["updated_at"] = timestamp
        record["agent"].update(agent_changes)
        record["agent"]["updated_at"] = timestamp
        return copy.deepcopy(record["draft"])

    async def validate_draft(self, **kwargs: Any) -> dict[str, Any]:
        draft = await self.get_draft(**kwargs)
        errors = validate_agent_spec(draft["spec"])
        return {
            "valid": not errors,
            "revision": draft["revision"],
            "spec_hash": draft["spec_hash"],
            "errors": errors,
        }

    async def create_version(self, **kwargs: Any) -> dict[str, Any]:
        record, _ = self._record(kwargs, "owner")
        current = record["draft"]["revision"]
        if kwargs["expected_revision"] != current:
            raise AgentDraftConflictError(current)
        errors = validate_agent_spec(record["draft"]["spec"])
        if errors:
            raise AgentValidationError(errors)
        timestamp = self._now()
        version = {
            "tenant_id": kwargs["tenant_id"],
            "agent_version_id": str(uuid.uuid4()),
            "agent_id": kwargs["agent_id"],
            "version_number": len(record["versions"]) + 1,
            "schema_version": "agent-spec/v1",
            "resolved_spec": copy.deepcopy(record["draft"]["spec"]),
            "spec_hash": record["draft"]["spec_hash"],
            "source_draft_id": record["draft"]["draft_id"],
            "source_draft_revision": current,
            "created_by": kwargs["user_id"],
            "created_at": timestamp,
        }
        record["versions"].append(version)
        return copy.deepcopy(version)

    async def list_versions(self, **kwargs: Any) -> list[dict[str, Any]]:
        record, _ = self._record(kwargs, "viewer")
        return [copy.deepcopy(version) for version in reversed(record["versions"])]

    async def resolve_version_runtime(self, **kwargs: Any) -> dict[str, Any]:
        record, role = self._record(kwargs, "viewer")
        if record["agent"]["status"] in {"archived", "deleted"}:
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_AGENT_UNAVAILABLE")
        version = next(
            (
                item
                for item in record["versions"]
                if item["agent_version_id"] == kwargs["agent_version_id"]
            ),
            None,
        )
        if version is None:
            raise AgentRuntimeUnavailableError("AGENT_VERSION_REVOKED")
        spec = copy.deepcopy(version["resolved_spec"])
        return {
            "agent": copy.deepcopy(record["agent"]),
            "caller_role": role,
            "version": copy.deepcopy(version),
            "spec": spec,
            "capabilities": [
                {
                    "capability_type": item["type"],
                    "resource_id": item["resource_id"],
                    "resource_version": item.get("resource_version"),
                    "schema_hash": item.get("schema_hash"),
                    "config": copy.deepcopy(item.get("config") or {}),
                }
                for item in spec.get("capabilities", [])
            ],
            "knowledge": copy.deepcopy(spec.get("knowledge", [])),
            "publication": None,
        }

    async def list_members(self, **kwargs: Any) -> list[dict[str, Any]]:
        record, _ = self._record(kwargs, "viewer")
        timestamp = record["agent"]["created_at"]
        return [
            {
                "tenant_id": kwargs["tenant_id"],
                "agent_id": kwargs["agent_id"],
                "principal_type": "user",
                "principal_id": principal_id,
                "role": role,
                "created_by": record["member_created_by"][principal_id],
                "created_at": timestamp,
                "updated_at": timestamp,
            }
            for principal_id, role in record["members"].items()
        ]

    async def upsert_member(self, **kwargs: Any) -> dict[str, Any]:
        record, _ = self._record(kwargs, "owner")
        principal_id = kwargs["principal_id"]
        prior = record["members"].get(principal_id)
        if prior == "owner" and kwargs["role"] != "owner":
            owner_count = sum(role == "owner" for role in record["members"].values())
            if owner_count == 1:
                raise AgentLastOwnerError("AGENT_LAST_OWNER")
        record["members"][principal_id] = kwargs["role"]
        record["member_created_by"].setdefault(principal_id, kwargs["user_id"])
        if (
            prior == "owner"
            and kwargs["role"] != "owner"
            and record["agent"]["owner_id"] == principal_id
        ):
            record["agent"]["owner_id"] = next(
                member_id
                for member_id, member_role in record["members"].items()
                if member_role == "owner"
            )
        timestamp = self._now()
        return {
            "tenant_id": kwargs["tenant_id"],
            "agent_id": kwargs["agent_id"],
            "principal_type": kwargs["principal_type"],
            "principal_id": principal_id,
            "role": kwargs["role"],
            "created_by": record["member_created_by"][principal_id],
            "created_at": timestamp,
            "updated_at": timestamp,
        }

    async def remove_member(self, **kwargs: Any) -> None:
        record, _ = self._record(kwargs, "owner")
        principal_id = kwargs["principal_id"]
        role = record["members"].get(principal_id)
        if not role:
            raise AgentNotFoundError("AGENT_MEMBER_NOT_FOUND")
        if role == "owner" and sum(value == "owner" for value in record["members"].values()) == 1:
            raise AgentLastOwnerError("AGENT_LAST_OWNER")
        del record["members"][principal_id]
        if role == "owner" and record["agent"]["owner_id"] == principal_id:
            record["agent"]["owner_id"] = next(
                member_id
                for member_id, member_role in record["members"].items()
                if member_role == "owner"
            )

    async def copy_agent(self, **kwargs: Any) -> dict[str, Any]:
        source, _ = self._record(kwargs, "owner")
        copied = await self.create_agent(
            tenant_id=kwargs["tenant_id"],
            user_id=kwargs["user_id"],
            name=kwargs.get("name") or f"{source['agent']['name']} Copy",
            slug=kwargs.get("slug"),
            description=source["agent"]["description"],
            spec=sanitize_agent_copy_spec(source["draft"]["spec"]),
        )
        return copied

    async def archive_agent(self, **kwargs: Any) -> dict[str, Any]:
        record, _ = self._record(kwargs, "owner")
        record["agent"]["status"] = "archived"
        record["agent"]["archived_at"] = self._now()
        record["agent"]["updated_at"] = record["agent"]["archived_at"]
        if kwargs["disable_publications"]:
            for publication in record["publications"]:
                publication["status"] = "disabled"
        return copy.deepcopy(record["agent"])

    async def soft_delete_agent(self, **kwargs: Any) -> None:
        record, _ = self._record(kwargs, "owner")
        record["agent"]["status"] = "deleted"
        record["agent"]["deleted_at"] = self._now()


def make_client(
    repository: InMemoryAgentRepository | None = None,
    user: UserContext | None = None,
) -> tuple[TestClient, InMemoryAgentRepository]:
    app = FastAPI()
    app.include_router(router)
    app.include_router(runtime_router)
    repo = repository or InMemoryAgentRepository()
    actor = user or make_user()
    app.state.agent_repository = repo
    app.state.session_manager = InMemoryRuntimeSessionManager()
    app.state.assistant_runtime_assignments = InMemoryRuntimeAssignmentStore()
    app.state.agent_runtime_model_resolver = RuntimeModelResolver()
    app.state.agent_runtime_capability_resolver = PassthroughCapabilityResolver()
    app.state.agent_runtime_knowledge_resolver = PassthroughKnowledgeResolver()
    app.dependency_overrides[get_user_context] = lambda: actor
    return TestClient(app), repo


class InMemoryRuntimeSessionManager:
    def __init__(self) -> None:
        self.bindings: dict[str, dict[str, Any]] = {}

    async def bind_agent_runtime(self, **kwargs: Any) -> Any:
        self.bindings[kwargs["session_id"]] = copy.deepcopy(kwargs)
        return kwargs

    async def get(self, session_id: str) -> Any | None:
        item = self.bindings.get(session_id)
        if item is None:
            return None
        return type("RuntimeSession", (), item)()


class InMemoryRuntimeAssignmentStore:
    async def bind(self, **_kwargs: Any) -> Any:
        return SimpleNamespace(runtime_owner="agent_runtime", kernel_revision="kernel-test")


class RuntimeModelResolver:
    def resolve(self, **kwargs: Any) -> dict[str, Any]:
        return {
            "id": kwargs["model"]["model_id"],
            "provider": kwargs["model"].get("provider_id") or "dashscope",
        }


class PassthroughCapabilityResolver:
    def resolve(self, **kwargs: Any) -> list[dict[str, Any]]:
        return copy.deepcopy(kwargs["bindings"])


class PassthroughKnowledgeResolver:
    def resolve(self, **kwargs: Any) -> list[dict[str, Any]]:
        return copy.deepcopy(kwargs["bindings"])


def create_agent(
    client: TestClient,
    *,
    name: str = "Research Agent",
    spec: dict[str, Any] | None = None,
) -> dict[str, Any]:
    response = client.post(
        "/agents",
        json={"name": name, "description": "Tenant-safe Agent", "spec": spec or valid_spec()},
    )
    assert response.status_code == 201, response.text
    return response.json()["agent"]


def test_create_list_get_and_cursor_pagination() -> None:
    client, _ = make_client()
    first = create_agent(client, name="Agent One")
    second = create_agent(client, name="Agent Two")

    page_one = client.get("/agents", params={"limit": 1})
    assert page_one.status_code == 200
    assert len(page_one.json()["items"]) == 1
    assert page_one.json()["next_cursor"] is not None

    page_two = client.get(
        "/agents",
        params={"limit": 1, "cursor": page_one.json()["next_cursor"]},
    )
    assert page_two.status_code == 200
    assert {page_one.json()["items"][0]["agent_id"], page_two.json()["items"][0]["agent_id"]} == {
        first["agent_id"],
        second["agent_id"],
    }

    detail = client.get(f"/agents/{first['agent_id']}")
    assert detail.status_code == 200
    assert detail.json()["tenant_id"] == "tenant-a"
    assert detail.json()["caller_role"] == "owner"
    assert detail.json()["draft"]["revision"] == 1

    record = client.app.state.agent_repository.records[("tenant-a", first["agent_id"])]
    record["publications"] = [
        {"channel": "hosted", "status": "disabled"},
        {"channel": "embed", "status": "active"},
    ]
    refreshed = client.get("/agents", params={"search": "Agent One"})
    assert refreshed.status_code == 200
    assert refreshed.json()["items"][0]["published_channels"] == ["embed"]


def test_create_materializes_empty_model_before_validation_and_versioning() -> None:
    client, repository = make_client()
    client.app.state.settings = SimpleNamespace(default_model="deployment-default-model")
    spec = valid_spec()
    spec["model"] = {
        "model_id": "",
        "provider_id": "dashscope",
        "temperature": 0.2,
    }

    agent = create_agent(client, spec=spec)
    agent_id = agent["agent_id"]
    stored_spec = repository.records[("tenant-a", agent_id)]["draft"]["spec"]

    assert stored_spec["model"] == {
        "model_id": "deployment-default-model",
        "provider_id": None,
        "temperature": 0.2,
        "max_tokens": None,
        "thinking_mode": None,
    }
    validation = client.post(f"/agents/{agent_id}/validate")
    assert validation.status_code == 200
    assert validation.json()["valid"] is True

    version = client.post(f"/agents/{agent_id}/versions", headers={"If-Match": '"1"'})
    assert version.status_code == 201, version.text
    assert version.json()["version"]["spec"]["model"]["model_id"] == ("deployment-default-model")


@pytest.mark.parametrize(
    "settings",
    [None, SimpleNamespace(default_model="")],
    ids=["missing-settings", "empty-default"],
)
def test_create_with_empty_model_fails_typed_when_server_default_is_unavailable(
    settings: SimpleNamespace | None,
) -> None:
    client, repository = make_client()
    if settings is not None:
        client.app.state.settings = settings
    spec = valid_spec()
    spec["model"] = {"model_id": "", "provider_id": "dashscope"}

    response = client.post(
        "/agents",
        json={"name": "Defaultless Agent", "description": "test", "spec": spec},
    )

    assert response.status_code == 503
    assert response.json()["detail"]["code"] == "AGENT_RUNTIME_MODEL_UNAVAILABLE"
    assert repository.records == {}


def test_update_materializes_empty_model_before_repository_write() -> None:
    client, repository = make_client()
    client.app.state.settings = SimpleNamespace(default_model="updated-default-model")
    agent = create_agent(client)
    spec = valid_spec("Use the deployment default")
    spec["model"] = {"model_id": "", "provider_id": "dashscope"}

    response = client.put(
        f"/agents/{agent['agent_id']}/draft",
        headers={"If-Match": '"1"'},
        json={"spec": spec},
    )

    assert response.status_code == 200, response.text
    stored_model = repository.records[("tenant-a", agent["agent_id"])]["draft"]["spec"]["model"]
    assert stored_model["model_id"] == "updated-default-model"
    assert stored_model["provider_id"] is None


def test_draft_etag_conflict_preserves_newer_edit() -> None:
    client, _ = make_client()
    agent = create_agent(client)
    draft_url = f"/agents/{agent['agent_id']}/draft"

    initial = client.get(draft_url)
    assert initial.status_code == 200
    assert initial.headers["etag"] == '"1"'

    missing_precondition = client.put(draft_url, json={"spec": valid_spec("first edit")})
    assert missing_precondition.status_code == 428
    assert missing_precondition.json()["detail"]["code"] == "AGENT_DRAFT_PRECONDITION_REQUIRED"

    for invalid_etag in ('W/"1"', "1", '"1", "2"', '"unterminated', '"0"', '"01"'):
        rejected = client.put(
            draft_url,
            headers={"If-Match": invalid_etag},
            json={"spec": valid_spec("must not be stored")},
        )
        assert rejected.status_code == 400
        assert rejected.json()["detail"]["code"] == "AGENT_DRAFT_ETAG_INVALID"
        unchanged = client.get(draft_url)
        assert unchanged.json()["revision"] == 1
        assert unchanged.json()["spec"]["instructions"] == "Answer clearly"

    saved = client.put(
        draft_url,
        headers={"If-Match": '"1"'},
        json={"spec": valid_spec("newer edit")},
    )
    assert saved.status_code == 200
    assert saved.headers["etag"] == '"2"'

    stale = client.put(
        draft_url,
        headers={"If-Match": '"1"'},
        json={"spec": valid_spec("stale overwrite")},
    )
    assert stale.status_code == 409
    assert stale.json()["detail"] == {
        "code": "AGENT_DRAFT_CONFLICT",
        "message": "Draft revision is stale",
        "request_id": stale.json()["detail"]["request_id"],
        "current_revision": 2,
    }
    current = client.get(draft_url)
    assert current.json()["spec"]["instructions"] == "newer edit"
    assert current.json()["revision"] == 2


def test_draft_and_metadata_save_is_atomic_across_conflict_validation_and_storage_errors() -> None:
    client, repository = make_client()
    agent = create_agent(client)
    agent_id = agent["agent_id"]
    draft_url = f"/agents/{agent_id}/draft"

    saved = client.put(
        draft_url,
        headers={"If-Match": '"1"'},
        json={
            "name": "Atomic Agent",
            "description": "Metadata and Draft share one transaction.",
            "spec": valid_spec("atomic edit"),
        },
    )
    assert saved.status_code == 200, saved.text
    assert saved.json()["draft"]["revision"] == 2

    def assert_saved_state() -> None:
        detail = client.get(f"/agents/{agent_id}")
        draft = client.get(draft_url)
        assert detail.status_code == 200
        assert detail.json()["name"] == "Atomic Agent"
        assert detail.json()["description"] == "Metadata and Draft share one transaction."
        assert detail.json()["draft_revision"] == 2
        assert draft.json()["revision"] == 2
        assert draft.json()["spec"]["instructions"] == "atomic edit"

    stale = client.put(
        draft_url,
        headers={"If-Match": '"1"'},
        json={
            "name": "Must not commit on conflict",
            "description": "Must not commit on conflict",
            "spec": valid_spec("stale edit"),
        },
    )
    assert stale.status_code == 409
    assert_saved_state()

    repository.next_draft_error = AgentValidationError(
        [
            {
                "field": "knowledge",
                "code": "AGENT_RESOURCE_NOT_FOUND",
                "message": "one or more Dataset bindings are unavailable",
            }
        ]
    )
    invalid = client.put(
        draft_url,
        headers={"If-Match": '"2"'},
        json={
            "name": "Must not commit on validation",
            "description": "Must not commit on validation",
            "spec": valid_spec("invalid resource edit"),
        },
    )
    assert invalid.status_code == 422
    assert invalid.json()["detail"]["code"] == "AGENT_SPEC_INVALID"
    assert_saved_state()

    repository.next_draft_error = AgentRepositoryError("synthetic storage outage")
    unavailable = client.put(
        draft_url,
        headers={"If-Match": '"2"'},
        json={
            "name": "Must not commit on storage outage",
            "description": "Must not commit on storage outage",
            "spec": valid_spec("unavailable edit"),
        },
    )
    assert unavailable.status_code == 503
    assert unavailable.json()["detail"]["code"] == "AGENT_STORAGE_UNAVAILABLE"
    assert_saved_state()

    retried = client.put(
        draft_url,
        headers={"If-Match": '"2"'},
        json={
            "name": "Atomic retry",
            "description": "The retry commits the complete batch.",
            "spec": valid_spec("retry edit"),
        },
    )
    assert retried.status_code == 200
    assert retried.json()["draft"]["revision"] == 3
    detail = client.get(f"/agents/{agent_id}").json()
    assert detail["name"] == "Atomic retry"
    assert detail["description"] == "The retry commits the complete batch."


def test_version_is_immutable_after_later_draft_edit() -> None:
    client, repository = make_client()
    agent = create_agent(client)
    agent_id = agent["agent_id"]

    version_response = client.post(
        f"/agents/{agent_id}/versions",
        headers={"If-Match": '"1"'},
    )
    assert version_response.status_code == 201, version_response.text
    version = version_response.json()["version"]
    original_hash = version["spec_hash"]
    assert version["spec"]["instructions"] == "Answer clearly"

    update = client.put(
        f"/agents/{agent_id}/draft",
        headers={"If-Match": '"1"'},
        json={"spec": valid_spec("changed after version")},
    )
    assert update.status_code == 200

    versions = client.get(f"/agents/{agent_id}/versions")
    assert versions.status_code == 200
    assert versions.json()[0]["spec_hash"] == original_hash
    assert versions.json()[0]["spec"]["instructions"] == "Answer clearly"
    stored = repository.records[("tenant-a", agent_id)]["versions"][0]
    assert stored["resolved_spec"]["instructions"] == "Answer clearly"
    assert stored["source_draft_revision"] == 1


def test_saved_version_can_open_an_isolated_preview_session() -> None:
    client, repository = make_client()
    agent = create_agent(client)
    agent_id = agent["agent_id"]
    version_response = client.post(
        f"/agents/{agent_id}/versions",
        headers={"If-Match": '"1"'},
    )
    assert version_response.status_code == 201
    version = version_response.json()["version"]

    session = client.post(
        f"/agents/{agent_id}/versions/{version['agent_version_id']}/preview/sessions",
        json={},
    )

    assert session.status_code == 201, session.text
    payload = session.json()
    assert payload["agent_id"] == agent_id
    assert payload["agent_version_id"] == version["agent_version_id"]
    assert payload["draft_revision"] is None
    assert payload["publication_id"] is None
    assert payload["channel"] == "preview"
    binding = repository.records[("tenant-a", agent_id)]["versions"][0]
    assert binding["resolved_spec"]["instructions"] == "Answer clearly"


def test_copy_excludes_runtime_acl_secret_and_resource_state() -> None:
    client, repository = make_client()
    source = create_agent(client, spec=valid_spec())
    source_record = repository.records[("tenant-a", source["agent_id"])]
    # Simulate a legacy row created before the public schema closed arbitrary fields.
    source_record["draft"]["spec"].update(
        {
            "capabilities": [{"type": "mcp", "resource_id": "server-a"}],
            "knowledge": [{"dataset_id": "dataset-a"}],
            "skills": [{"skill_id": "skill-a"}],
            "connectors": [{"connection_id": "connector-a"}],
            "tool_bindings": [{"resource_id": "tool-a"}],
            "nested": {"safe": True, "secretRef": "synthetic-reference"},
            "apiKey": "synthetic-not-a-real-key",
        }
    )
    source_record["members"]["viewer-a"] = "viewer"
    source_record["member_created_by"]["viewer-a"] = "owner-a"
    source_record["publications"].append({"publication_id": "pub-a", "status": "active"})
    source_record["tokens"].append("token-a")
    source_record["sessions"].append("session-a")
    source_record["memory"].append("memory-a")

    response = client.post(f"/agents/{source['agent_id']}/copy", json={"name": "Safe Copy"})
    assert response.status_code == 201, response.text
    copied = response.json()["agent"]
    copied_record = repository.records[("tenant-a", copied["agent_id"])]
    copied_spec = copied_record["draft"]["spec"]

    assert copied_record["members"] == {"owner-a": "owner"}
    assert copied_record["versions"] == []
    assert copied_record["publications"] == []
    assert copied_record["tokens"] == []
    assert copied_record["sessions"] == []
    assert copied_record["memory"] == []
    assert copied_record["draft"]["revision"] == 1
    assert copied_spec["model"]["max_tokens"] == 2048
    assert copied_spec["capabilities"] == []
    assert copied_spec["knowledge"] == []
    assert copied_spec["memory"] == {}
    for excluded in (
        "skills",
        "connectors",
        "tool_bindings",
        "nested",
        "apiKey",
        "secretRef",
    ):
        assert excluded not in json.dumps(copied_spec, sort_keys=True)


def test_archive_makes_draft_read_only_and_delete_is_soft() -> None:
    client, repository = make_client()
    agent = create_agent(client)
    agent_id = agent["agent_id"]
    archived = client.post(
        f"/agents/{agent_id}/archive",
        json={"disable_publications": True},
    )
    assert archived.status_code == 200
    assert archived.json()["agent"]["status"] == "archived"

    edit = client.put(
        f"/agents/{agent_id}/draft",
        headers={"If-Match": '"1"'},
        json={"spec": valid_spec("should fail")},
    )
    assert edit.status_code == 409
    assert edit.json()["detail"]["code"] == "AGENT_ARCHIVED"

    deleted = client.delete(f"/agents/{agent_id}")
    assert deleted.status_code == 200
    assert repository.records[("tenant-a", agent_id)]["agent"]["deleted_at"] is not None
    assert client.get(f"/agents/{agent_id}").status_code == 404


def test_validation_and_openapi_contract_are_secret_free() -> None:
    client, _ = make_client()
    client.app.state.settings = SimpleNamespace(default_model="deployment-default-model")
    invalid = create_agent(
        client,
        spec={
            "schema_version": "agent-spec/v1",
            "instructions": "",
            "model": {"model_id": ""},
        },
    )
    validation = client.post(f"/agents/{invalid['agent_id']}/validate")
    assert validation.status_code == 200
    assert validation.json()["valid"] is False
    assert {item["code"] for item in validation.json()["errors"]} == {
        "AGENT_INSTRUCTIONS_REQUIRED",
    }

    unsafe_specs = []
    for key in ("apiKey", "secretRef", "private_key", "authorization", "bearer_token"):
        candidate = valid_spec()
        candidate["memory"] = {key: "synthetic-not-a-real-secret"}
        unsafe_specs.append(candidate)
    nested_credential = valid_spec()
    nested_credential["capabilities"] = [
        {
            "type": "mcp",
            "resource_id": "server-a",
            "config": {"credentials": {"value": "synthetic"}},
        }
    ]
    unsafe_specs.append(nested_credential)
    arbitrary_binding = valid_spec()
    arbitrary_binding["tool_bindings"] = [{"resource_id": "tool-a"}]
    unsafe_specs.append(arbitrary_binding)
    for index, secret_spec in enumerate(unsafe_specs):
        rejected = client.post(
            "/agents",
            json={"name": f"Rejected {index}", "spec": secret_spec},
        )
        assert rejected.status_code == 422

    openapi = client.get("/openapi.json").json()
    paths = openapi["paths"]
    expected = {
        "/agents",
        "/agents/{agent_id}",
        "/agents/{agent_id}/draft",
        "/agents/{agent_id}/validate",
        "/agents/{agent_id}/versions",
        "/agents/{agent_id}/copy",
        "/agents/{agent_id}/archive",
        "/agents/{agent_id}/members/{principal_type}/{principal_id}",
    }
    assert expected <= set(paths)
    rendered = json.dumps(openapi, sort_keys=True)
    assert "token_hash" not in rendered
    assert "secret_ref" not in rendered
    assert hashlib.sha256(b"not-a-token").hexdigest() not in rendered


def test_legacy_draft_response_redacts_secrets_and_unknown_binding_containers() -> None:
    client, repository = make_client()
    agent = create_agent(client)
    record = repository.records[("tenant-a", agent["agent_id"])]
    record["draft"]["spec"].update(
        {
            "apiKey": "synthetic-not-a-real-key",
            "secretRef": "synthetic-reference",
            "tool_bindings": [{"resource_id": "legacy-tool"}],
            "model": {
                "model_id": "qwen3.7-plus",
                "private_key": "synthetic-private-material",
            },
            "memory": {
                "authorization": "synthetic-authorization",
                "mode": "session",
            },
        }
    )

    response = client.get(f"/agents/{agent['agent_id']}/draft")
    assert response.status_code == 200
    spec = response.json()["spec"]
    rendered = json.dumps(spec, sort_keys=True)
    for forbidden in (
        "apiKey",
        "secretRef",
        "tool_bindings",
        "private_key",
        "authorization",
        "synthetic-not-a-real-key",
        "synthetic-private-material",
    ):
        assert forbidden not in rendered
    assert spec["memory"] == {"mode": "session"}
