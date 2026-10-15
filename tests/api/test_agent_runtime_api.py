from __future__ import annotations

import copy
import hashlib
import json
import uuid
from datetime import datetime, timedelta, timezone
from threading import Lock
from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_contracts.agent_launch import ResolvedAgentLaunchV1
from ai_gateway_core.exceptions import PermissionDeniedError
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentRuntimeUnavailableError,
    DatabaseAgentRepository,
)
from fastapi import FastAPI
from fastapi.responses import StreamingResponse
from fastapi.testclient import TestClient

from src.api.deps import get_user_context
from src.api.v1 import agent_runtime as runtime_module
from src.api.v1.agent_public import document_router
from src.api.v1.agent_public import router as public_router
from src.api.v1.agent_runtime import router as runtime_router
from src.api.v1.agents import publication_router
from src.api.v1.assistant import router as assistant_router
from src.core.auth.user_resolver import UserContext

PUBLICATION_ID = "33333333-3333-4333-8333-333333333333"
PUBLIC_ID = "44444444-4444-4444-8444-444444444444"
AGENT_ID = "11111111-1111-4111-8111-111111111111"
VERSION_ID = "22222222-2222-4222-8222-222222222222"


def _user(*, authenticated: bool = False) -> UserContext:
    return UserContext(
        user_id="owner-a" if authenticated else "anon:browser-a",
        tenant_id="tenant-a" if authenticated else "public",
        tier="normal" if authenticated else "anonymous",
        is_authenticated=authenticated,
        roles=["user"] if authenticated else ["guest"],
        ip="127.0.0.1",
    )


def _resolution(channel: str, *, policy: dict[str, Any] | None = None) -> dict[str, Any]:
    return {
        "agent": {
            "tenant_id": "tenant-a",
            "agent_id": AGENT_ID,
            "name": "Support Guide",
            "description": "Answers approved support questions.",
        },
        "caller_role": "viewer",
        "version": {
            "agent_version_id": VERSION_ID,
            "spec_hash": "a" * 64,
        },
        "publication": {
            "tenant_id": "tenant-a",
            "agent_id": AGENT_ID,
            "publication_id": PUBLICATION_ID,
            "public_id": PUBLIC_ID,
            "channel": channel,
            "auth_mode": "token" if channel == "api" else "public",
            "policy": policy
            or {
                "attachments": True,
                "high_risk_tools": True,
                "allowed_origins": ["https://allowed.example"],
                "requests_per_minute": 30,
                "requests_per_day": 1000,
            },
            "status": "active",
            "version_id": VERSION_ID,
        },
        "spec": {
            "schema_version": "agent-spec/v1",
            "identity": {
                "theme_color": "#635bff",
                "welcome_message": "How can I help?",
                "suggested_prompts": ["Summarize the policy"],
            },
            "instructions": "Use only the immutable publication.",
            "model": {"model_id": "qwen3.7-plus", "provider_id": "dashscope"},
            "capabilities": [],
            "knowledge": [],
            "memory": {"mode": "user"},
        },
        "capabilities": [
            {
                "capability_type": "native",
                "resource_id": "safe-read",
                "risk": "low",
                "config": {},
            },
            {
                "capability_type": "connector",
                "resource_id": "write-ticket",
                "risk": "low",
                "config": {"write": True},
            },
            {
                "capability_type": "mcp",
                "resource_id": "dangerous-tool",
                "risk": "high",
                "config": {},
            },
        ],
        "knowledge": [],
    }


class _Resolver:
    def resolve(self, **kwargs: Any) -> list[dict[str, Any]]:
        return list(kwargs["bindings"])


class _Sessions:
    def __init__(self) -> None:
        self.items: dict[str, SimpleNamespace] = {}

    async def bind_agent_runtime(self, **kwargs: Any) -> SimpleNamespace:
        existing = self.items.get(kwargs["session_id"])
        candidate = SimpleNamespace(**kwargs)
        if existing:
            comparable = (
                "user_id",
                "tenant_id",
                "agent_id",
                "agent_version_id",
                "agent_draft_revision",
                "publication_id",
                "channel",
                "runtime_fingerprint",
                "agent_spec_hash",
            )
            if any(getattr(existing, key) != getattr(candidate, key) for key in comparable):
                raise PermissionDeniedError("session mismatch")
            return existing
        self.items[kwargs["session_id"]] = candidate
        return candidate

    async def get(self, session_id: str) -> SimpleNamespace | None:
        return self.items.get(session_id)


class _Assignments:
    def __init__(self) -> None:
        self.bound: list[tuple[str, str, str]] = []

    async def bind_new_session(
        self,
        *,
        tenant_id: str,
        user_id: str,
        session_id: str,
        policy: Any,
    ) -> SimpleNamespace:
        del policy
        self.bound.append((tenant_id, user_id, session_id))
        return SimpleNamespace(runtime_owner="agent_runtime")


class _Repository:
    def __init__(self) -> None:
        self.tokens: dict[str, dict[str, Any]] = {
            "agt_valid": {
                "token_id": "55555555-5555-4555-8555-555555555555",
                "scopes": {
                    "chat:write",
                    "sessions:write",
                    "attachments:write",
                    "feedback:write",
                },
                "revoked": False,
            }
        }
        self.public_auth_mode = "public"
        self.publication_status = "active"
        self.version_id = VERSION_ID
        self.policy: dict[str, Any] | None = None
        self.idempotency: dict[tuple[str, str, str], dict[str, Any]] = {}
        self.attachments: dict[str, dict[str, Any]] = {
            "artifact-a": {
                "artifact_id": "artifact-a",
                "filename": "policy.pdf",
                "mime_type": "application/pdf",
                "file_path": "/uploads/runtime/policy.pdf",
            }
        }
        self._idempotency_lock = Lock()
        self.feedback: list[dict[str, Any]] = []
        self.metadata: list[dict[str, Any]] = []
        self.governance = {
            "principal_requests_per_minute": 30,
            "principal_requests_per_day": 1000,
            "ip_requests_per_minute": 60,
            "ip_requests_per_day": 2000,
            "publication_requests_per_minute": 300,
            "publication_requests_per_day": 10_000,
        }
        self.governance_unavailable = False
        self.version_resolution_calls: list[dict[str, Any]] = []

    async def get_runtime_governance_usage(self, **_kwargs: Any) -> dict[str, Any]:
        if self.governance_unavailable:
            raise RuntimeError("governance backend unavailable")
        return {
            "policy": copy.deepcopy(self.governance),
            "usage": {},
            "exceeded": [],
        }

    async def resolve_api_token_runtime(self, **kwargs: Any) -> dict[str, Any]:
        token = self.tokens.get(kwargs["raw_token"])
        if not token or token["revoked"]:
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_TOKEN_INVALID")
        if not set(kwargs["required_scopes"]).issubset(token["scopes"]):
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_TOKEN_SCOPE_FORBIDDEN")
        if self.publication_status != "active":
            raise AgentRuntimeUnavailableError("PUBLICATION_DISABLED")
        result = _resolution("api")
        if self.policy is not None:
            result["publication"]["policy"] = copy.deepcopy(self.policy)
        result["version"]["agent_version_id"] = kwargs.get("pinned_version_id") or self.version_id
        result["api_token"] = {
            "token_id": token["token_id"],
            "scopes": sorted(token["scopes"]),
        }
        return result

    async def resolve_public_channel_runtime(self, **kwargs: Any) -> dict[str, Any]:
        if kwargs["public_id"] != PUBLIC_ID or self.publication_status != "active":
            raise AgentRuntimeUnavailableError("PUBLICATION_DISABLED")
        if self.public_auth_mode in {"private", "tenant"} and not kwargs["authenticated"]:
            raise AgentRuntimeUnavailableError("PUBLICATION_AUTHENTICATION_REQUIRED")
        if self.public_auth_mode == "tenant" and kwargs["caller_tenant_id"] != "tenant-a":
            raise AgentRuntimeUnavailableError("PUBLICATION_ACCESS_DENIED")
        result = _resolution(kwargs["channel"])
        if self.policy is not None:
            result["publication"]["policy"] = copy.deepcopy(self.policy)
        result["publication"]["auth_mode"] = self.public_auth_mode
        result["version"]["agent_version_id"] = kwargs.get("pinned_version_id") or self.version_id
        return result

    async def resolve_version_runtime(self, **kwargs: Any) -> dict[str, Any]:
        self.version_resolution_calls.append(copy.deepcopy(kwargs))
        assert kwargs["tenant_id"] == "tenant-a"
        assert kwargs["agent_id"] == AGENT_ID
        assert kwargs["agent_version_id"] == VERSION_ID
        assert kwargs["user_id"] == "owner-a"
        result = _resolution("preview")
        result["publication"] = None
        return result

    async def get_publication_channel(self, **kwargs: Any) -> dict[str, Any]:
        if kwargs["public_id"] != PUBLIC_ID or self.publication_status != "active":
            raise AgentRuntimeUnavailableError("PUBLICATION_DISABLED")
        return {
            "publication_id": PUBLICATION_ID,
            "public_id": PUBLIC_ID,
            "channel": "embed",
            "auth_mode": self.public_auth_mode,
            "status": "active",
            "name": "Support Guide",
            "description": "Approved answers",
            "identity": {},
            "policy": {"allowed_origins": ["https://allowed.example"]},
        }

    async def reserve_runtime_idempotency(self, **kwargs: Any) -> dict[str, Any]:
        key = (kwargs["publication_id"], kwargs["principal_id"], kwargs["idempotency_key"])
        with self._idempotency_lock:
            existing = self.idempotency.get(key)
            if existing and existing["request_hash"] != kwargs["request_hash"]:
                raise AgentRuntimeUnavailableError("AGENT_RUNTIME_IDEMPOTENCY_CONFLICT")
            if existing:
                return {"created": False, **copy.deepcopy(existing)}
            row = {
                "request_hash": kwargs["request_hash"],
                "session_id": kwargs["session_id"],
                "status": "pending",
                "response_body": None,
                "response_media_type": None,
                "response_status_code": None,
            }
            self.idempotency[key] = row
            return {"created": True, **copy.deepcopy(row)}

    async def complete_runtime_idempotency(self, **kwargs: Any) -> None:
        key = (kwargs["publication_id"], kwargs["principal_id"], kwargs["idempotency_key"])
        row = self.idempotency[key]
        row.update({
            "status": "completed",
            "response_body": kwargs["response_body"],
            "response_media_type": kwargs["response_media_type"],
            "response_status_code": kwargs["response_status_code"],
        })

    async def fail_runtime_idempotency(self, **kwargs: Any) -> None:
        key = (kwargs["publication_id"], kwargs["principal_id"], kwargs["idempotency_key"])
        if key in self.idempotency:
            self.idempotency[key]["status"] = "failed"

    async def create_runtime_attachment(self, **kwargs: Any) -> dict[str, Any]:
        attachment_id = str(uuid.uuid4())
        row = {
            "attachment_id": attachment_id,
            "artifact_id": attachment_id,
            "filename": kwargs["filename"],
            "mime_type": kwargs["mime_type"],
            "size_bytes": kwargs["size_bytes"],
            "expires_at": (datetime.now(timezone.utc) + timedelta(days=1)).isoformat(),
            "file_path": "/" + kwargs["storage_key"],
        }
        self.attachments[attachment_id] = row
        return copy.deepcopy(row)

    async def resolve_runtime_attachments(self, **kwargs: Any) -> list[dict[str, Any]]:
        try:
            return [
                {
                    key: self.attachments[attachment_id][key]
                    for key in ("artifact_id", "filename", "mime_type", "file_path")
                }
                for attachment_id in kwargs["attachment_ids"]
            ]
        except KeyError as exc:
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_ATTACHMENT_NOT_FOUND") from exc

    async def record_runtime_feedback(self, **kwargs: Any) -> dict[str, Any]:
        row = {"feedback_id": str(uuid.uuid4()), **kwargs}
        self.feedback.append(row)
        return row

    async def create_api_token(self, **kwargs: Any) -> tuple[str, dict[str, Any]]:
        raw = f"agt_{uuid.uuid4().hex}"
        token_id = str(uuid.uuid4())
        row = {
            "tenant_id": kwargs["tenant_id"],
            "token_id": token_id,
            "publication_id": kwargs["publication_id"],
            "name": kwargs["name"],
            "scopes": kwargs["scopes"],
            "expires_at": kwargs["expires_at"],
            "revoked_at": None,
            "last_used_at": None,
            "rotated_from_token_id": None,
            "created_by": kwargs["user_id"],
            "created_at": datetime.now(timezone.utc).isoformat(),
        }
        self.metadata.append(row)
        return raw, copy.deepcopy(row)

    async def list_api_tokens(self, **kwargs: Any) -> list[dict[str, Any]]:
        return [
            copy.deepcopy(item)
            for item in self.metadata
            if item["publication_id"] == kwargs["publication_id"]
        ]

    async def rotate_api_token(self, **kwargs: Any) -> tuple[str, dict[str, Any]]:
        old = next(item for item in self.metadata if item["token_id"] == kwargs["token_id"])
        old["revoked_at"] = datetime.now(timezone.utc).isoformat()
        raw, row = await self.create_api_token(
            tenant_id=kwargs["tenant_id"],
            publication_id=kwargs["publication_id"],
            user_id=kwargs["user_id"],
            name=kwargs["name"] or old["name"],
            scopes=kwargs["scopes"] or old["scopes"],
            expires_at=kwargs["expires_at"] or old["expires_at"],
        )
        row["rotated_from_token_id"] = old["token_id"]
        self.metadata[-1]["rotated_from_token_id"] = old["token_id"]
        return raw, row

    async def revoke_api_token(self, **kwargs: Any) -> dict[str, Any]:
        row = next(item for item in self.metadata if item["token_id"] == kwargs["token_id"])
        row["revoked_at"] = datetime.now(timezone.utc).isoformat()
        return copy.deepcopy(row)


class _AtomicChannelLimiter:
    def __init__(self) -> None:
        self.counts: dict[tuple[str, str], int] = {}
        self._lock = Lock()

    async def consume(self, **kwargs: Any) -> int:
        publication_id = kwargs["publication_id"]
        principal_id = kwargs["principal_id"]
        client_ip = kwargs["client_ip"]
        dimensions = (
            f"principal:{principal_id}:minute",
            f"principal:{principal_id}:day",
            f"ip:{client_ip}:minute",
            f"ip:{client_ip}:day",
            "publication:minute",
            "publication:day",
        )
        with self._lock:
            for index, (dimension, limit) in enumerate(
                zip(dimensions, kwargs["limits"], strict=True),
                start=1,
            ):
                if self.counts.get((publication_id, dimension), 0) >= limit:
                    return index
            for dimension in dimensions:
                key = (publication_id, dimension)
                self.counts[key] = self.counts.get(key, 0) + 1
        return 0


class _FileStorage:
    async def upload_file_streaming(self, **kwargs: Any) -> SimpleNamespace:
        content = b""
        async for chunk in kwargs["content_iterator"]:
            content += chunk
        if len(content) > kwargs["max_size_bytes"]:
            raise ValueError("file too large")
        file_id = uuid.uuid4().hex[:8]
        storage_key = f"uploads/{kwargs['user_id']}/{file_id}.txt"
        return SimpleNamespace(
            storage_key=storage_key,
            content_type=kwargs["content_type"],
            size_bytes=len(content),
        )

    async def delete_file(self, _storage_key: str) -> bool:
        return True


@pytest.fixture
def runtime_client(monkeypatch: pytest.MonkeyPatch) -> tuple[TestClient, _Repository, list[dict[str, Any]]]:
    monkeypatch.setenv("ASSISTANT_E2E_STUB_LLM", "true")
    monkeypatch.setenv("GATEWAY_ASSISTANT_SHARED_SECRET", "test-shared-secret-value")
    captured: list[dict[str, Any]] = []

    class _RuntimeControl:
        async def start_turn(self, **kwargs: Any) -> Any:
            launch = kwargs["resolved_agent_launch"]
            assert isinstance(launch, ResolvedAgentLaunchV1)
            snapshot = launch.to_control_snapshot()
            captured.append(
                {
                    "runtime_envelope": {"resolved_snapshot": snapshot},
                    "attachments": [
                        {"file_path": ref}
                        for ref in launch.runtime_inputs["readonly_capabilities"][
                            "attachments"
                        ]["refs"]
                    ],
                    "launch_entrypoint": launch.identity["entrypoint"],
                }
            )
            return SimpleNamespace(run_id="run-runtime")

        async def stream_events(self, **_kwargs: Any):
            yield b'data: {"event_type":"text_delta","data":{"content":"approved response"}}\n\n'
            yield b'data: {"event_type":"run_finished","data":{"status":"succeeded"}}\n\n'

    app = FastAPI()
    app.state.agent_runtime_control = _RuntimeControl()
    repository = _Repository()
    app.state.agent_repository = repository
    app.state.session_manager = _Sessions()
    app.state.assistant_runtime_assignments = _Assignments()
    app.state.assistant_runtime_assignment_policy = object()
    app.state.agent_runtime_capability_resolver = _Resolver()
    app.state.agent_runtime_knowledge_resolver = _Resolver()
    app.state.agent_channel_limiter = _AtomicChannelLimiter()
    app.state.file_storage = _FileStorage()
    app.include_router(runtime_router, prefix="/api/v1")
    app.include_router(public_router, prefix="/api/v1")
    app.include_router(publication_router, prefix="/api/v1")
    app.include_router(document_router)
    app.dependency_overrides[get_user_context] = lambda: _user(authenticated=False)
    return TestClient(app), repository, captured


@pytest.fixture
def version_resume_client(
    monkeypatch: pytest.MonkeyPatch,
) -> tuple[TestClient, _Repository, list[dict[str, Any]], list[tuple[str, str, str]]]:
    monkeypatch.setenv("ASSISTANT_E2E_STUB_LLM", "true")
    monkeypatch.setenv("GATEWAY_ASSISTANT_SHARED_SECRET", "test-shared-secret-value")
    captured: list[dict[str, Any]] = []
    approval_scopes: list[tuple[str, str, str]] = []
    approved = False

    async def _proxy(_request: Any, user: UserContext, **kwargs: Any) -> Any:
        nonlocal approved
        path = str(kwargs["path"])
        if path.startswith("approvals/"):
            approval_id = path.removeprefix("approvals/")
            assert json.loads(kwargs["body"]) == {"approved": True}
            approval_scopes.append((user.tenant_id, user.user_id, approval_id))
            approved = True
            return {"approval": {"approval_id": approval_id, "status": "approved"}}

        assert path == "agent-runtime/chat/stream"
        body = json.loads(kwargs["body"])
        captured.append(body)

        async def _events():
            if body.get("resume_run_id"):
                assert approved is True
                yield (
                    'data: {"event_type":"run_started","data":{"run_id":"run-a",'
                    '"session_id":"version-session","status":"running"}}\n\n'
                )
                yield (
                    'data: {"event_type":"run_finished","data":{"run_id":"run-a",'
                    '"session_id":"version-session","status":"succeeded"}}\n\n'
                )
            else:
                yield (
                    'data: {"event_type":"approval_required","data":{"run_id":"run-a",'
                    '"session_id":"version-session","approval_id":"approval-a",'
                    '"checkpoint_id":"checkpoint-a","status":"pending"}}\n\n'
                )

        return StreamingResponse(_events(), media_type="text/event-stream")

    app = FastAPI()
    repository = _Repository()
    app.state.agent_repository = repository
    app.state.session_manager = _Sessions()
    app.state.assistant_runtime_assignments = _Assignments()
    app.state.assistant_runtime_assignment_policy = object()
    app.state.agent_runtime_capability_resolver = _Resolver()
    app.state.agent_runtime_knowledge_resolver = _Resolver()
    app.include_router(runtime_router, prefix="/api/v1")
    app.include_router(assistant_router, prefix="/api/v1")
    app.dependency_overrides[get_user_context] = lambda: _user(authenticated=True)
    return TestClient(app), repository, captured, approval_scopes


def test_version_preview_approval_resume_re_resolves_and_re_signs_same_pinned_version(
    version_resume_client: tuple[
        TestClient,
        _Repository,
        list[dict[str, Any]],
        list[tuple[str, str, str]],
    ],
) -> None:
    client, repository, captured, approval_scopes = version_resume_client
    route = f"/api/v1/agents/{AGENT_ID}/versions/{VERSION_ID}/preview/chat/stream"
    first = client.post(
        route,
        json={
            "message": "perform the approved action",
            "session_id": "version-session",
            "resume_run_id": "run-a",
            "resume_approval_id": "approval-a",
        },
    )
    assert first.status_code == 409, first.text
    assert first.json()["detail"]["code"] == "AGENT_RUNTIME_RESUME_NOT_AVAILABLE"
    assert approval_scopes == []
    assert len(repository.version_resolution_calls) == 1
    assert captured == []


def test_version_preview_binds_single_kernel_assignment_before_turn(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, _repository, captured = runtime_client
    client.app.dependency_overrides[get_user_context] = lambda: _user(authenticated=True)
    response = client.post(
        f"/api/v1/agents/{AGENT_ID}/versions/{VERSION_ID}/preview/chat/stream",
        json={"message": "explain transformers"},
    )

    assert response.status_code == 200, response.text
    assert "approved response" in response.text
    assert captured
    assert captured[-1]["launch_entrypoint"] == "studio_preview"
    assignments = client.app.state.assistant_runtime_assignments
    assert len(assignments.bound) == 1
    tenant_id, user_id, session_id = assignments.bound[0]
    assert (tenant_id, user_id) == ("tenant-a", "owner-a")
    assert session_id


def test_published_runtime_resume_is_signed_into_downstream_body(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, repository, captured = runtime_client
    headers = {"Authorization": "Bearer agt_valid"}
    session_id = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/sessions",
        headers=headers,
    ).json()["session_id"]

    resumed = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/chat/stream",
        headers=headers,
        json={
            "message": "continue",
            "session_id": session_id,
            "resume_run_id": "run-a",
            "resume_approval_id": "approval-a",
        },
    )
    assert resumed.status_code == 409, resumed.text
    assert resumed.json()["detail"]["code"] == "AGENT_RUNTIME_RESUME_NOT_AVAILABLE"

    repository.policy = {
        "attachments": True,
        "high_risk_tools": False,
        "allowed_origins": ["https://allowed.example"],
        "requests_per_minute": 30,
        "requests_per_day": 1000,
    }
    changed_policy = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/chat/stream",
        headers=headers,
        json={
            "message": "continue",
            "session_id": session_id,
            "resume_run_id": "run-a",
            "resume_approval_id": "approval-a",
        },
    )
    assert changed_policy.status_code == 404
    assert changed_policy.json()["detail"]["code"] == "AGENT_RUNTIME_SESSION_NOT_FOUND"


def test_runtime_api_requires_scoped_token_and_creates_isolated_session(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, _repository, _captured = runtime_client
    missing = client.post(f"/api/v1/agent-runtime/{PUBLICATION_ID}/sessions")
    assert missing.status_code == 401
    assert missing.json()["detail"]["code"] == "AGENT_RUNTIME_TOKEN_REQUIRED"

    created = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/sessions",
        headers={"Authorization": "Bearer agt_valid"},
    )
    assert created.status_code == 201
    assert created.json()["channel"] == "api"
    assert created.json()["publication_id"] == PUBLICATION_ID


def test_runtime_stream_attachment_scope_idempotency_and_sse(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, repository, captured = runtime_client
    headers = {"Authorization": "Bearer agt_valid", "Idempotency-Key": "turn-1"}
    response = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/chat/stream",
        headers=headers,
        json={
            "message": "hello",
            "attachments": [{"artifact_id": "artifact-a", "filename": "policy.pdf"}],
        },
    )
    assert response.status_code == 200
    assert "approved response" in response.text
    assert captured[-1]["launch_entrypoint"] == "published_agent"
    assert captured[-1]["runtime_envelope"]["resolved_snapshot"]["publication"]["channel"] == "api"
    assert captured[-1]["attachments"][0]["file_path"] == "/uploads/runtime/policy.pdf"

    invocation_count = len(captured)
    replay = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/chat/stream",
        headers=headers,
        json={
            "message": "hello",
            "attachments": [{"artifact_id": "artifact-a", "filename": "policy.pdf"}],
        },
    )
    assert replay.status_code == 200
    assert replay.headers["x-idempotent-replay"] == "true"
    assert replay.text == response.text
    assert len(captured) == invocation_count

    repository.tokens["agt_valid"]["scopes"].remove("attachments:write")
    denied = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/chat/stream",
        headers={"Authorization": "Bearer agt_valid"},
        json={"message": "hello", "attachments": [{"artifact_id": "artifact-a"}]},
    )
    assert denied.status_code == 403
    assert denied.json()["detail"]["code"] == "AGENT_RUNTIME_TOKEN_SCOPE_FORBIDDEN"

    conflict = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/chat/stream",
        headers=headers,
        json={"message": "different request"},
    )
    assert conflict.status_code == 409
    assert conflict.json()["detail"]["code"] == "AGENT_RUNTIME_IDEMPOTENCY_CONFLICT"


def test_runtime_session_is_publication_principal_and_version_pinned(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, repository, captured = runtime_client
    headers = {"Authorization": "Bearer agt_valid"}
    created = client.post(f"/api/v1/agent-runtime/{PUBLICATION_ID}/sessions", headers=headers)
    session_id = created.json()["session_id"]
    repository.version_id = "66666666-6666-4666-8666-666666666666"
    streamed = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/chat/stream",
        headers=headers,
        json={"message": "continue", "session_id": session_id},
    )
    assert streamed.status_code == 200
    assert captured[-1]["runtime_envelope"]["resolved_snapshot"]["agent_version_id"] == VERSION_ID

    other_token = copy.deepcopy(repository.tokens["agt_valid"])
    other_token["token_id"] = "77777777-7777-4777-8777-777777777777"
    repository.tokens["agt_other"] = other_token
    denied = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/chat/stream",
        headers={"Authorization": "Bearer agt_other"},
        json={"message": "steal", "session_id": session_id},
    )
    assert denied.status_code == 404


def test_runtime_feedback_requires_scope_and_owned_session(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, repository, _captured = runtime_client
    headers = {"Authorization": "Bearer agt_valid"}
    session_id = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/sessions", headers=headers
    ).json()["session_id"]
    submitted = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/feedback",
        headers=headers,
        json={"session_id": session_id, "rating": 1},
    )
    assert submitted.status_code == 200
    assert repository.feedback[-1]["comment"] == ""

    repository.tokens["agt_valid"]["scopes"].remove("feedback:write")
    denied = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/feedback",
        headers=headers,
        json={"session_id": session_id, "rating": -1},
    )
    assert denied.status_code == 403


def test_public_hosted_forces_session_memory_and_removes_write_and_high_risk_tools(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, _repository, captured = runtime_client
    config = client.get(f"/api/v1/public/agents/{PUBLIC_ID}?channel=hosted")
    assert config.status_code == 200
    assert config.json()["identity"]["welcome_message"] == "How can I help?"
    assert config.json()["version_number"] == 1
    assert config.json()["capability_count"] == 1
    assert config.json()["knowledge_count"] == 0
    assert config.json()["release_gate_verified"] is False
    session_id = client.post(
        f"/api/v1/public/agents/{PUBLIC_ID}/sessions", json={"channel": "hosted"}
    ).json()["session_id"]
    response = client.post(
        f"/api/v1/public/agents/{PUBLIC_ID}/chat/stream",
        json={"channel": "hosted", "session_id": session_id, "message": "hello"},
    )
    assert response.status_code == 200
    snapshot = captured[-1]["runtime_envelope"]["resolved_snapshot"]
    assert snapshot["memory"] == {"mode": "session"}
    assert [item["id"] for item in snapshot["capabilities"]] == ["safe-read"]


def test_hosted_attachment_upload_resolves_server_owned_path(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, _repository, captured = runtime_client
    uploaded = client.post(
        f"/api/v1/public/agents/{PUBLIC_ID}/attachments?channel=hosted",
        files={"file": ("notes.txt", b"approved context", "text/plain")},
    )
    assert uploaded.status_code == 201
    attachment = uploaded.json()
    response = client.post(
        f"/api/v1/public/agents/{PUBLIC_ID}/chat/stream",
        json={
            "channel": "hosted",
            "message": "summarize the attachment",
            "attachments": [{
                "artifact_id": attachment["artifact_id"],
                "filename": attachment["filename"],
                "mime_type": attachment["mime_type"],
            }],
        },
    )
    assert response.status_code == 200
    assert captured[-1]["attachments"][0]["file_path"].startswith("/uploads/")

    denied = client.post(
        f"/api/v1/public/agents/{PUBLIC_ID}/chat/stream",
        json={
            "channel": "hosted",
            "message": "steal",
            "attachments": [{"artifact_id": str(uuid.uuid4())}],
        },
    )
    assert denied.status_code == 404
    assert denied.json()["detail"]["code"] == "AGENT_RUNTIME_ATTACHMENT_NOT_FOUND"


def test_hosted_private_requires_authentication_and_tenant_mode_isolated(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, repository, _captured = runtime_client
    repository.public_auth_mode = "private"
    required = client.get(f"/api/v1/public/agents/{PUBLIC_ID}?channel=hosted")
    assert required.status_code == 401
    assert required.json()["detail"]["code"] == "PUBLICATION_AUTHENTICATION_REQUIRED"

    client.app.dependency_overrides[get_user_context] = lambda: _user(authenticated=True)
    assert client.get(f"/api/v1/public/agents/{PUBLIC_ID}?channel=hosted").status_code == 200


def test_publication_disable_and_per_publication_rate_limit_are_immediate(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, repository, _captured = runtime_client
    limited = _resolution("hosted", policy={
        "attachments": False,
        "high_risk_tools": False,
        "allowed_origins": [],
        "requests_per_minute": 1,
        "requests_per_day": 2,
    })
    original = repository.resolve_public_channel_runtime

    async def _limited(**kwargs: Any) -> dict[str, Any]:
        result = await original(**kwargs)
        result["publication"]["policy"] = limited["publication"]["policy"]
        return result

    repository.resolve_public_channel_runtime = _limited  # type: ignore[method-assign]
    first = client.post(
        f"/api/v1/public/agents/{PUBLIC_ID}/sessions", json={"channel": "hosted"}
    )
    assert first.status_code == 201
    second = client.post(
        f"/api/v1/public/agents/{PUBLIC_ID}/sessions", json={"channel": "hosted"}
    )
    assert second.status_code == 429
    assert second.json()["detail"]["code"] == "AGENT_RUNTIME_RATE_LIMITED"

    repository.publication_status = "disabled"
    disabled = client.get(f"/api/v1/public/agents/{PUBLIC_ID}?channel=hosted")
    assert disabled.status_code == 409
    assert disabled.json()["detail"]["code"] == "PUBLICATION_DISABLED"


def test_saved_governance_limit_is_authoritative_and_lookup_fails_closed(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, repository, _captured = runtime_client
    repository.policy = {
        "requests_per_minute": 100,
        "requests_per_day": 1000,
    }
    repository.governance["principal_requests_per_minute"] = 1
    headers = {"Authorization": "Bearer agt_valid"}

    first = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/sessions", headers=headers
    )
    second = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/sessions", headers=headers
    )
    assert first.status_code == 201
    assert second.status_code == 429
    assert second.json()["detail"]["code"] == "AGENT_RUNTIME_RATE_LIMITED"

    repository.governance_unavailable = True
    unavailable = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/sessions", headers=headers
    )
    assert unavailable.status_code == 503
    assert unavailable.json()["detail"]["code"] == "AGENT_RUNTIME_QUOTA_UNAVAILABLE"


def test_ip_bucket_cannot_be_bypassed_with_another_runtime_token(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, repository, _captured = runtime_client
    repository.policy = {
        "attachments": False,
        "high_risk_tools": False,
        "allowed_origins": [],
        "requests_per_minute": 10,
        "requests_per_day": 100,
        "ip_requests_per_minute": 1,
        "ip_requests_per_day": 100,
        "publication_requests_per_minute": 100,
        "publication_requests_per_day": 1000,
    }
    other = copy.deepcopy(repository.tokens["agt_valid"])
    other["token_id"] = "77777777-7777-4777-8777-777777777777"
    repository.tokens["agt_other"] = other
    first = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/sessions",
        headers={"Authorization": "Bearer agt_valid"},
    )
    assert first.status_code == 201
    bypass = client.post(
        f"/api/v1/agent-runtime/{PUBLICATION_ID}/sessions",
        headers={"Authorization": "Bearer agt_other"},
    )
    assert bypass.status_code == 429
    assert bypass.json()["detail"]["code"] == "AGENT_RUNTIME_RATE_LIMITED"


def test_management_token_create_list_rotate_and_revoke_show_raw_once(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, _repository, _captured = runtime_client
    client.app.dependency_overrides[get_user_context] = lambda: _user(authenticated=True)
    created = client.post(
        f"/api/v1/publications/{PUBLICATION_ID}/tokens",
        json={"name": "production", "scopes": ["chat:write", "sessions:write"]},
    )
    assert created.status_code == 201
    raw = created.json()["token"]
    token_id = created.json()["token_metadata"]["token_id"]
    listed = client.get(f"/api/v1/publications/{PUBLICATION_ID}/tokens")
    assert listed.status_code == 200
    assert raw not in listed.text
    assert "token_hash" not in listed.text

    rotated = client.post(
        f"/api/v1/publications/{PUBLICATION_ID}/tokens/{token_id}/rotate", json={}
    )
    assert rotated.status_code == 201
    assert rotated.json()["token"] != raw
    replacement = rotated.json()["token_metadata"]["token_id"]
    revoked = client.delete(
        f"/api/v1/publications/{PUBLICATION_ID}/tokens/{replacement}"
    )
    assert revoked.status_code == 200
    assert revoked.json()["revoked_at"] is not None


class _Transaction:
    async def __aenter__(self) -> None:
        return None

    async def __aexit__(self, *_args: Any) -> None:
        return None


class _HashConnection:
    def __init__(self) -> None:
        self.calls: list[tuple[str, tuple[Any, ...]]] = []

    def transaction(self) -> _Transaction:
        return _Transaction()

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any]:
        self.calls.append((query, args))
        if "SELECT p.publication_id" in query:
            return {
                "publication_id": uuid.UUID(PUBLICATION_ID),
                "agent_id": uuid.UUID(AGENT_ID),
                "channel": "api",
                "status": "active",
            }
        return {
            "tenant_id": "tenant-a",
            "token_id": uuid.uuid4(),
            "publication_id": uuid.UUID(PUBLICATION_ID),
            "name": "server",
            "scopes": ["chat:write"],
            "expires_at": None,
            "revoked_at": None,
            "created_by": "owner-a",
            "created_at": datetime.now(timezone.utc),
        }

    async def execute(self, query: str, *args: Any) -> str:
        self.calls.append((query, args))
        return "INSERT 0 1"


class _Acquire:
    def __init__(self, conn: _HashConnection) -> None:
        self.conn = conn

    async def __aenter__(self) -> _HashConnection:
        return self.conn

    async def __aexit__(self, *_args: Any) -> None:
        return None


class _Pool:
    def __init__(self, conn: _HashConnection) -> None:
        self.conn = conn

    def acquire(self) -> _Acquire:
        return _Acquire(self.conn)


@pytest.mark.asyncio
async def test_database_repository_hashes_raw_token_and_rejects_past_expiry() -> None:
    conn = _HashConnection()
    repository = DatabaseAgentRepository(SimpleNamespace(_pool=_Pool(conn), enabled=True))
    raw, _metadata = await repository.create_api_token(
        tenant_id="tenant-a",
        publication_id=PUBLICATION_ID,
        user_id="owner-a",
        name="server",
        scopes=["chat:write"],
        expires_at=None,
    )
    insert = next(call for call in conn.calls if "INSERT INTO agent_api_tokens" in call[0])
    stored_hash = insert[1][2]
    assert raw.startswith("agt_")
    assert stored_hash == hashlib.sha256(raw.encode()).hexdigest()
    assert all(raw not in repr(args) for _query, args in conn.calls)

    with pytest.raises(AgentRuntimeUnavailableError, match="TOKEN_EXPIRY_INVALID"):
        await repository.create_api_token(
            tenant_id="tenant-a",
            publication_id=PUBLICATION_ID,
            user_id="owner-a",
            name="expired",
            scopes=["chat:write"],
            expires_at=datetime.now(timezone.utc) - timedelta(seconds=1),
        )


def test_confirmation_stamp_only_pins_definitions_that_confirm() -> None:
    """High/critical platform bindings are pinned only when the live
    definition actually supports confirmation; otherwise enforcement stays
    fail-closed at runtime instead of a misleading snapshot pin."""
    # Non-platform or non-high-risk bindings are never stamped.
    assert (
        runtime_module._confirmation_stamp(
            {"risk": "high"}, risk="high", runtime_type="mcp", definition=None
        )
        == {"risk": "high"}
    )
    assert (
        runtime_module._confirmation_stamp(
            {}, risk="medium", runtime_type="platform", definition=None
        )
        == {}
    )

    # An existing pin is preserved as-is.
    pinned = {"requires_confirmation": False}
    result = runtime_module._confirmation_stamp(
        dict(pinned), risk="high", runtime_type="platform", definition=None
    )
    assert result == pinned

    # A definition that supports confirmation gets stamped.
    stamped = runtime_module._confirmation_stamp(
        {},
        risk="critical",
        runtime_type="platform",
        definition=SimpleNamespace(requires_confirmation=True),
    )
    assert stamped["requires_confirmation"] is True

    # An unresolvable or non-confirming definition is left unpinned.
    assert "requires_confirmation" not in runtime_module._confirmation_stamp(
        {}, risk="high", runtime_type="platform", definition=None
    )
    assert "requires_confirmation" not in runtime_module._confirmation_stamp(
        {},
        risk="high",
        runtime_type="platform",
        definition=SimpleNamespace(requires_confirmation=False),
    )


def test_public_effective_native_capabilities_defaults_high_risk_confirmation() -> None:
    """High/critical native capabilities still project as needing confirmation
    even when the snapshot no longer fabricates a requires_confirmation pin."""
    snapshot = {
        "capabilities": [
            {
                "type": "platform",
                "id": "dangerous-native",
                "schema_hash": "sha256:" + "a" * 64,
                "risk": "high",
                "config": {},
            },
            {
                "type": "platform",
                "id": "safe-native",
                "schema_hash": "sha256:" + "b" * 64,
                "risk": "low",
                "config": {},
            },
        ]
    }
    projected = runtime_module._public_effective_native_capabilities(snapshot)
    assert projected == [
        {
            "name": "dangerous-native",
            "schema_hash": "sha256:" + "a" * 64,
            "risk": "high",
            "requires_confirmation": True,
        }
    ]
