from __future__ import annotations

import copy
import hashlib
import json
import uuid
from contextlib import asynccontextmanager
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.eval.agent_version_candidate import (
    AgentReleaseCandidateError,
    build_model_authorization_evidence,
)
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentNotFoundError,
    AgentReleaseEvaluationStaleError,
    AgentReleaseGateError,
    AgentReleaseIdempotencyConflictError,
    AgentRuntimeUnavailableError,
    DatabaseAgentRepository,
    require_public_release_expiry,
)
from fastapi import FastAPI
from fastapi.testclient import TestClient

from src.api.deps import get_user_context
from src.api.v1 import agents as agents_module
from src.api.v1.agents import publication_router, router
from src.core.auth.user_resolver import UserContext


def test_equivalent_public_expiry_serializations_have_one_snapshot_identity():
    from src.api.v1._agent_runtime_routes.resolution import _channel_policy

    policies = [
        _channel_policy({"publication": {"policy": {"expires_at": expiry}}}, channel="hosted")
        for expiry in ("2027-01-01T00:00:00Z", "2027-01-01T00:00:00+00:00", "2027-01-01T05:00:00+05:00")
    ]
    assert len({runtime_sha256(policy) for policy in policies}) == 1
    assert policies[0]["expires_at"] == "2027-01-01T00:00:00+00:00"
    changed = _channel_policy({"publication": {"policy": {"expires_at": "2027-01-02T00:00:00Z"}}}, channel="hosted")
    assert runtime_sha256(changed) != runtime_sha256(policies[0])

AGENT_ID = "11111111-1111-4111-8111-111111111111"
DRAFT_ID = "22222222-2222-4222-8222-222222222222"
EVALUATION_ID = "33333333-3333-4333-8333-333333333333"
VERSION_ID = "44444444-4444-4444-8444-444444444444"
OLD_VERSION_ID = "55555555-5555-4555-8555-555555555555"
PUBLICATION_ID = "66666666-6666-4666-8666-666666666666"
EVENT_ID = "77777777-7777-4777-8777-777777777777"


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def test_public_release_expiry_requires_a_future_aware_deadline() -> None:
    now = datetime(2026, 9, 28, tzinfo=timezone.utc)
    assert require_public_release_expiry("private", {}, now=now) is None
    with pytest.raises(AgentReleaseGateError, match="AGENT_PUBLIC_EXPIRY_REQUIRED"):
        require_public_release_expiry("public", {}, now=now)
    with pytest.raises(AgentReleaseGateError, match="AGENT_PUBLIC_EXPIRY_REQUIRED"):
        require_public_release_expiry("public", {"expires_at": "2026-09-29T00:00:00"}, now=now)
    with pytest.raises(AgentReleaseGateError, match="AGENT_PUBLIC_EXPIRY_EXPIRED"):
        require_public_release_expiry("public", {"expires_at": now.isoformat()}, now=now)
    future = now + timedelta(hours=1)
    assert require_public_release_expiry(
        "public", {"expires_at": future.isoformat()}, now=now
    ) == future


async def test_public_runtime_rejects_expired_publication_and_allows_legacy_null() -> None:
    publication = {
        "tenant_id": "tenant-a",
        "agent_id": uuid.UUID(AGENT_ID),
        "channel": "hosted",
        "auth_mode": "public",
        "status": "active",
        "expires_at": datetime.now(timezone.utc) - timedelta(seconds=1),
    }

    class Connection:
        async def fetchrow(self, *_args: Any) -> dict[str, Any]:
            return publication

    class Pool:
        @asynccontextmanager
        async def acquire(self):
            yield Connection()

    repository = DatabaseAgentRepository(SimpleNamespace(enabled=True, _pool=Pool()))

    async def resolved(*_args: Any, **_kwargs: Any) -> dict[str, Any]:
        return {"publication": publication}

    repository._load_channel_runtime_material = resolved  # type: ignore[method-assign]
    kwargs = {
        "public_id": str(uuid.uuid4()),
        "channel": "hosted",
        "caller_tenant_id": "public",
        "user_id": "anonymous",
        "authenticated": False,
        "is_tenant_admin": False,
    }
    with pytest.raises(AgentRuntimeUnavailableError, match="PUBLICATION_EXPIRED"):
        await repository.resolve_public_channel_runtime(**kwargs)

    publication["expires_at"] = None
    assert (await repository.resolve_public_channel_runtime(**kwargs))["publication"] is publication


def _plain_hash(value: Any) -> str:
    raw = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(raw.encode()).hexdigest()


def _user(user_id: str = "owner-a") -> UserContext:
    return UserContext(
        user_id=user_id,
        tenant_id="tenant-a",
        tier="normal",
        is_authenticated=True,
        roles=["user"],
        ip="127.0.0.1",
    )


def _runtime_fingerprint() -> dict[str, str]:
    model_authorization = build_model_authorization_evidence(
        source="agent_runtime_resolver",
        model_id="qwen3.7-plus",
        provider_id="dashscope",
        access_level="public",
        model_enabled=True,
        provider_enabled=True,
        runtime_provider_configured=True,
    )
    return {
        "spec_hash": "a" * 64,
        "model_id": "qwen3.7-plus",
        "provider_id": "dashscope",
        "model_authorization_hash": _plain_hash(model_authorization),
        "prompt_hash": "sha256:" + "b" * 64,
        "tool_schema_hash": "sha256:" + "c" * 64,
        "skill_manifest_hash": "sha256:" + "d" * 64,
        "knowledge_revision": "sha256:" + "e" * 64,
        "eval_dataset_manifest_hash": _plain_hash({"dataset_id": None}),
        "runtime_version": "agent-runtime/v1",
        "snapshot_hash": "sha256:" + "f" * 64,
        "channel_policy_hash": "sha256:" + "1" * 64,
    }


def _candidate(
    *,
    channel: str = "hosted",
    auth_mode: str = "private",
    policy: dict[str, Any] | None = None,
    dataset_id: str | None = None,
) -> dict[str, Any]:
    channel_policy = policy or {
        "attachments": False,
        "high_risk_tools": False,
        "allowed_origins": [],
    }
    fingerprint = _runtime_fingerprint()
    fingerprint["channel_policy_hash"] = runtime_sha256(channel_policy)
    return {
        "schema_version": "agent-release-candidate/v1",
        "tenant_id": "tenant-a",
        "agent_id": AGENT_ID,
        "draft_id": DRAFT_ID,
        "draft_revision": 3,
        "spec_hash": "a" * 64,
        "channel": channel,
        "auth_mode": auth_mode,
        "channel_policy": copy.deepcopy(channel_policy),
        "channel_policy_hash": _plain_hash(channel_policy),
        "dataset_id": dataset_id,
        "dataset_version": "v1" if dataset_id else None,
        "dataset_manifest_hash": "7" * 64 if dataset_id else None,
        "model_authorization": build_model_authorization_evidence(
            source="agent_runtime_resolver",
            model_id="qwen3.7-plus",
            provider_id="dashscope",
            access_level="public",
            model_enabled=True,
            provider_enabled=True,
            runtime_provider_configured=True,
        ),
        "runtime_fingerprint": fingerprint,
        "runtime_fingerprint_hash": _plain_hash(fingerprint),
        "release_identity_hash": "9" * 64,
        "evaluation_identity_hash": "8" * 64,
    }


def _evaluation(candidate: dict[str, Any] | None = None) -> dict[str, Any]:
    candidate = candidate or _candidate()
    timestamp = _now()
    return {
        "tenant_id": "tenant-a",
        "evaluation_id": EVALUATION_ID,
        "agent_id": AGENT_ID,
        "draft_id": DRAFT_ID,
        "draft_revision": 3,
        "spec_hash": "a" * 64,
        "runtime_fingerprint": copy.deepcopy(candidate["runtime_fingerprint"]),
        "runtime_fingerprint_hash": candidate["runtime_fingerprint_hash"],
        "release_identity_hash": candidate["release_identity_hash"],
        "evaluation_identity_hash": candidate.get("evaluation_identity_hash"),
        "profile_id": "offline_v1",
        "profile_version": "2026-07-19",
        "dataset_id": candidate.get("dataset_id"),
        "dataset_version": candidate.get("dataset_version"),
        "dataset_manifest_hash": candidate.get("dataset_manifest_hash"),
        "experiment_run_id": None,
        "channel": candidate["channel"],
        "auth_mode": candidate["auth_mode"],
        "channel_policy": copy.deepcopy(candidate["channel_policy"]),
        "channel_policy_hash": candidate["channel_policy_hash"],
        "status": "passed",
        "stale": False,
        "stale_reasons": [],
        "validation_snapshot": {"resource_authorization_rechecked": True},
        "gate_snapshot": {
            "status": "passed",
            "execution_scope": "provider_free_release_integrity",
            "model_quality_evaluated": False,
        },
        "events": [
            {
                "event_id": str(uuid.uuid4()),
                "evaluation_id": EVALUATION_ID,
                "sequence": sequence,
                "status": status,
                "summary": {},
                "created_at": timestamp,
            }
            for sequence, status in enumerate(("queued", "running", "passed"), start=1)
        ],
        "created_by": "owner-a",
        "created_at": timestamp,
        "completed_at": timestamp,
    }


def _version(version_id: str = VERSION_ID, number: int = 2) -> dict[str, Any]:
    return {
        "tenant_id": "tenant-a",
        "agent_version_id": version_id,
        "agent_id": AGENT_ID,
        "version_number": number,
        "schema_version": "agent-spec/v1",
        "resolved_spec": {
            "schema_version": "agent-spec/v1",
            "identity": {},
            "instructions": "saved prompt",
            "model": {"model_id": "qwen3.7-plus"},
            "capabilities": [],
            "knowledge": [],
            "memory": {},
        },
        "spec_hash": "a" * 64,
        "source_draft_id": DRAFT_ID,
        "source_draft_revision": 3,
        "release_evaluation_id": EVALUATION_ID,
        "release_identity_hash": "9" * 64,
        "created_by": "owner-a",
        "created_at": _now(),
    }


def _publication(version_id: str = VERSION_ID) -> dict[str, Any]:
    timestamp = _now()
    return {
        "tenant_id": "tenant-a",
        "publication_id": PUBLICATION_ID,
        "agent_id": AGENT_ID,
        "channel": "hosted",
        "public_id": str(uuid.uuid4()),
        "version_id": version_id,
        "version_number": 2,
        "version_spec_hash": "a" * 64,
        "auth_mode": "private",
        "policy": {"attachments": False, "high_risk_tools": False, "allowed_origins": []},
        "status": "active",
        "created_by": "owner-a",
        "updated_by": "owner-a",
        "created_at": timestamp,
        "updated_at": timestamp,
    }


def _release_result(*, replayed: bool, operation: str = "promote") -> dict[str, Any]:
    return {
        "version": _version(OLD_VERSION_ID if operation == "rollback" else VERSION_ID),
        "publication": _publication(
            OLD_VERSION_ID if operation == "rollback" else VERSION_ID
        ),
        "event": {
            "tenant_id": "tenant-a",
            "event_id": EVENT_ID,
            "publication_id": PUBLICATION_ID,
            "agent_id": AGENT_ID,
            "from_version_id": OLD_VERSION_ID if operation == "promote" else VERSION_ID,
            "to_version_id": VERSION_ID if operation == "promote" else OLD_VERSION_ID,
            "actor_id": "owner-a",
            "reason": "release",
            "validation_snapshot": {"resource_authorization_rechecked": True},
            "operation": operation,
            "release_evaluation_id": EVALUATION_ID if operation == "promote" else None,
            "request_hash": "8" * 64,
            "created_at": _now(),
        },
        "idempotent_replay": replayed,
    }


class _ReleaseRepository:
    def __init__(self) -> None:
        self.evaluation = _evaluation()
        self.publish_keys: set[str] = set()
        self.release_requests: dict[tuple[str, str], dict[str, Any]] = {}
        self.candidate_resolution_calls = 0
        self.version_resolution_calls = 0
        self.last_gate: dict[str, Any] | None = None

    @staticmethod
    def _require_owner(kwargs: dict[str, Any]) -> None:
        if kwargs.get("user_id") != "owner-a":
            raise AgentNotFoundError("AGENT_NOT_FOUND")

    async def record_release_evaluation(self, **kwargs: Any) -> dict[str, Any]:
        self._require_owner(kwargs)
        self.last_gate = copy.deepcopy(kwargs["gate"])
        self.evaluation = _evaluation(kwargs["candidate"])
        self.evaluation["gate_snapshot"] = copy.deepcopy(kwargs["gate"])
        return copy.deepcopy(self.evaluation)

    async def create_release_evaluation(self, **kwargs: Any) -> dict[str, Any]:
        self._require_owner(kwargs)
        self.evaluation = _evaluation(kwargs["candidate"])
        self.evaluation.update(
            {
                "status": "queued",
                "completed_at": None,
                "started_at": None,
                "gate_snapshot": {
                    "status": "queued",
                    "profile_id": kwargs["profile"]["profile_id"],
                    "profile_version": kwargs["profile"]["profile_version"],
                    "blocking_findings": [],
                    "non_blocking_findings": [],
                },
                "events": [
                    {
                        "event_id": str(uuid.uuid4()),
                        "evaluation_id": EVALUATION_ID,
                        "sequence": 1,
                        "status": "queued",
                        "summary": {},
                        "created_at": _now(),
                    }
                ],
            }
        )
        return copy.deepcopy(self.evaluation)

    async def start_release_evaluation(self, **kwargs: Any) -> dict[str, Any]:
        self._require_owner(kwargs)
        if self.evaluation["status"] != "queued":
            result = copy.deepcopy(self.evaluation)
            result["execution_claimed"] = False
            return result
        self.evaluation["status"] = "running"
        self.evaluation["started_at"] = _now()
        self.evaluation["gate_snapshot"]["status"] = "running"
        self.evaluation["events"].append(
            {
                "event_id": str(uuid.uuid4()),
                "evaluation_id": EVALUATION_ID,
                "sequence": 2,
                "status": "running",
                "summary": {},
                "created_at": _now(),
            }
        )
        result = copy.deepcopy(self.evaluation)
        result["execution_claimed"] = True
        return result

    async def complete_release_evaluation(self, **kwargs: Any) -> dict[str, Any]:
        self._require_owner(kwargs)
        if self.evaluation["status"] == "cancelled":
            return copy.deepcopy(self.evaluation)
        self.last_gate = copy.deepcopy(kwargs["gate"])
        self.evaluation["status"] = kwargs["gate"]["status"]
        self.evaluation["gate_snapshot"] = copy.deepcopy(kwargs["gate"])
        self.evaluation["completed_at"] = _now()
        self.evaluation["events"].append(
            {
                "event_id": str(uuid.uuid4()),
                "evaluation_id": EVALUATION_ID,
                "sequence": len(self.evaluation["events"]) + 1,
                "status": kwargs["gate"]["status"],
                "summary": {},
                "created_at": _now(),
            }
        )
        return copy.deepcopy(self.evaluation)

    async def cancel_release_evaluation(self, **kwargs: Any) -> dict[str, Any]:
        self._require_owner(kwargs)
        self.evaluation["status"] = "cancelled"
        self.evaluation["gate_snapshot"]["status"] = "cancelled"
        self.evaluation["completed_at"] = _now()
        self.evaluation["events"].append(
            {
                "event_id": str(uuid.uuid4()),
                "evaluation_id": EVALUATION_ID,
                "sequence": len(self.evaluation["events"]) + 1,
                "status": "cancelled",
                "summary": {},
                "created_at": _now(),
            }
        )
        return copy.deepcopy(self.evaluation)

    async def list_release_evaluations(self, **_kwargs: Any) -> list[dict[str, Any]]:
        return [copy.deepcopy(self.evaluation)]

    async def get_release_evaluation(self, **kwargs: Any) -> dict[str, Any]:
        self._require_owner(kwargs)
        return copy.deepcopy(self.evaluation)

    async def get_release_diff(self, **_kwargs: Any) -> dict[str, Any]:
        return {
            "evaluation_id": EVALUATION_ID,
            "draft_revision": 3,
            "publication_id": None,
            "current_version_id": None,
            "current_version_number": None,
            "diff": {"schema_version": "agent-release-diff/v1", "changed_sections": []},
        }

    async def publish_agent(self, **kwargs: Any) -> dict[str, Any]:
        self._require_owner(kwargs)
        key = kwargs["idempotency_key"]
        replayed = key in self.publish_keys
        self.publish_keys.add(key)
        result = _release_result(replayed=replayed)
        self.release_requests[("promote", key)] = {
            "agent_id": AGENT_ID,
            "evaluation_id": kwargs["evaluation_id"],
            "reason": kwargs["reason"],
            "result": copy.deepcopy(result),
        }
        return result

    async def replay_release_request(self, **kwargs: Any) -> dict[str, Any] | None:
        self._require_owner(kwargs)
        record = self.release_requests.get((kwargs["operation"], kwargs["idempotency_key"]))
        if record is None:
            return None
        matches = (
            record["agent_id"] == kwargs["agent_id"]
            and record["reason"] == kwargs["reason"]
        )
        if kwargs["operation"] == "promote":
            matches = matches and record["evaluation_id"] == kwargs["evaluation_id"]
        else:
            matches = (
                matches
                and record["publication_id"] == kwargs["publication_id"]
                and record["target_version_id"] == kwargs["target_version_id"]
            )
        if not matches:
            raise AgentReleaseIdempotencyConflictError(
                "AGENT_RELEASE_IDEMPOTENCY_CONFLICT"
            )
        result = copy.deepcopy(record["result"])
        result["idempotent_replay"] = True
        return result

    async def list_publications(self, **_kwargs: Any) -> list[dict[str, Any]]:
        return [_publication()]

    async def list_publish_events(self, **_kwargs: Any) -> list[dict[str, Any]]:
        return [_release_result(replayed=False)["event"]]

    async def get_publication(self, **kwargs: Any) -> dict[str, Any]:
        self._require_owner(kwargs)
        return _publication(VERSION_ID)

    async def resolve_version_runtime(self, **kwargs: Any) -> dict[str, Any]:
        self.version_resolution_calls += 1
        return {
            "agent": {"tenant_id": "tenant-a", "agent_id": AGENT_ID},
            "version": _version(str(kwargs["agent_version_id"]), 1),
            "spec": _version()["resolved_spec"],
            "capabilities": [],
            "knowledge": [],
            "publication": None,
        }

    async def rollback_publication(self, **kwargs: Any) -> dict[str, Any]:
        self._require_owner(kwargs)
        result = _release_result(replayed=False, operation="rollback")
        self.release_requests[("rollback", kwargs["idempotency_key"])] = {
            "agent_id": AGENT_ID,
            "publication_id": kwargs["publication_id"],
            "target_version_id": kwargs["target_version_id"],
            "reason": kwargs["reason"],
            "result": copy.deepcopy(result),
        }
        return result


@pytest.fixture
def release_client(monkeypatch: pytest.MonkeyPatch):
    repository = _ReleaseRepository()
    selected_user = [_user()]
    app = FastAPI()
    app.state.agent_repository = repository
    app.include_router(router)
    app.include_router(publication_router)
    app.dependency_overrides[get_user_context] = lambda: selected_user[0]

    async def resolve_candidate(**kwargs: Any) -> dict[str, Any]:
        repository.candidate_resolution_calls += 1
        return _candidate(
            channel=kwargs["channel"],
            auth_mode=kwargs["auth_mode"],
            policy=kwargs["channel_policy"],
            dataset_id=kwargs["dataset_id"],
        )

    async def build_snapshot(*_args: Any, **_kwargs: Any) -> dict[str, Any]:
        return {
            "schema_version": "agent-runtime/v1",
            "tenant_id": "tenant-a",
            "agent_id": AGENT_ID,
            "publication": {"channel": "hosted", "auth_mode": "private"},
            "model": {"id": "qwen3.7-plus", "provider": "dashscope"},
            "instructions": {"prompt_hash": "sha256:" + "b" * 64},
            "capabilities": [],
            "knowledge": {"datasets": []},
            "channel_policy": {
                "attachments": False,
                "high_risk_tools": False,
                "allowed_origins": [],
            },
            "fingerprints": {
                "spec": "sha256:" + "a" * 64,
                "tool_schema": "sha256:" + "c" * 64,
                "skills": "sha256:" + "d" * 64,
                "knowledge_revision": "sha256:" + "e" * 64,
            },
        }

    async def resolve_model_authorization(**_kwargs: Any) -> dict[str, Any]:
        return build_model_authorization_evidence(
            source="agent_runtime_resolver",
            model_id="qwen3.7-plus",
            provider_id="dashscope",
            access_level="public",
            model_enabled=True,
            provider_enabled=True,
            runtime_provider_configured=True,
        )

    monkeypatch.setattr(agents_module, "_resolve_release_candidate", resolve_candidate)
    monkeypatch.setattr(
        agents_module,
        "_resolve_release_model_authorization",
        resolve_model_authorization,
    )
    import src.api.v1.agent_runtime as agent_runtime_module

    monkeypatch.setattr(agent_runtime_module, "_build_snapshot", build_snapshot)
    with TestClient(app) as client:
        yield client, repository, selected_user


def test_eval_request_rejects_client_owned_gate_fields(release_client) -> None:
    client, repository, _ = release_client

    response = client.post(
        f"/agents/{AGENT_ID}/evals",
        json={
            "draft_revision": 3,
            "channel": "hosted",
            "status": "passed",
            "profile_id": "client-bypass",
            "runtime_fingerprint": {"forged": True},
        },
    )

    assert response.status_code == 422
    assert repository.last_gate is None


def test_public_eval_requires_explicit_future_expiry(release_client) -> None:
    client, repository, _ = release_client
    path = f"/agents/{AGENT_ID}/evals"
    public_request = {"draft_revision": 3, "channel": "hosted", "auth_mode": "public"}
    missing = client.post(path, json=public_request)
    expired = client.post(
        path,
        json={
            **public_request,
            "channel_policy": {
                "expires_at": (datetime.now(timezone.utc) - timedelta(minutes=1)).isoformat()
            },
        },
    )
    internal_with_expiry = client.post(
        path,
        json={
            "draft_revision": 3,
            "channel_policy": {
                "expires_at": (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
            },
        },
    )
    assert [response.status_code for response in (missing, expired, internal_with_expiry)] == [
        422,
        422,
        422,
    ]
    assert repository.candidate_resolution_calls == 0


@pytest.mark.asyncio
async def test_release_authorization_receives_the_resolved_default_model() -> None:
    class Resolver:
        def __init__(self) -> None:
            self.models: list[dict[str, Any]] = []

        def resolve(self, **kwargs: Any) -> dict[str, Any]:
            self.models.append(dict(kwargs["model"]))
            return {
                "id": kwargs["model"]["model_id"],
                "provider": kwargs["model"]["provider_id"],
                "access_level": "public",
            }

    resolver = Resolver()
    request = SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(agent_runtime_model_resolver=resolver)
        )
    )
    resolution = {
        "spec": {
            "model": {
                "model_id": "",
                "provider_id": "stale-ui-placeholder",
                "temperature": 0.2,
            }
        }
    }

    evidence = await agents_module._resolve_release_model_authorization(
        request=request,
        user=_user(),
        resolution=resolution,
        model_id="deployment-default-model",
        provider_id="resolved-provider",
    )

    assert resolver.models == [
        {
            "model_id": "deployment-default-model",
            "provider_id": "resolved-provider",
            "temperature": 0.2,
        }
    ]
    assert evidence["model_id"] == "deployment-default-model"
    assert evidence["provider_id"] == "resolved-provider"


async def test_tenant_reader_cannot_authorize_owner_premium_model() -> None:
    class Resolver:
        def resolve(self, **kwargs: Any) -> dict[str, Any]:
            return {
                "id": kwargs["model"]["model_id"],
                "provider": kwargs["model"]["provider_id"],
                "access_level": "premium",
            }

    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(agent_runtime_model_resolver=Resolver()))
    )
    reader = agents_module._release_audience_user(
        _user(), agent_id=AGENT_ID, channel="hosted", auth_mode="tenant"
    )
    with pytest.raises(AgentReleaseCandidateError, match="AGENT_RUNTIME_MODEL_FORBIDDEN"):
        await agents_module._resolve_release_model_authorization(
            request=request,
            user=reader,
            resolution={"spec": {"model": {"model_id": "premium-model"}}},
            model_id="premium-model",
            provider_id="provider-a",
        )


@pytest.mark.parametrize(
    ("channel", "auth_mode"),
    [
        ("hosted", "private"),
        ("hosted", "tenant"),
        ("hosted", "public"),
        ("embed", "token"),
        ("api", "token"),
    ],
)
def test_eval_api_binds_authorized_eval_dataset_snapshot(
    monkeypatch: pytest.MonkeyPatch,
    channel: str,
    auth_mode: str,
) -> None:
    class DatasetReleaseRepository(_ReleaseRepository):
        def __init__(self) -> None:
            super().__init__()
            self.dataset_requests: list[str] = []
            self.preview_request_users: list[str] = []
            self.dataset_request_users: list[str] = []

        async def resolve_preview_runtime(self, **kwargs: Any) -> dict[str, Any]:
            self.preview_request_users.append(str(kwargs["user_id"]))
            return {
                "agent": {"tenant_id": "tenant-a", "agent_id": AGENT_ID},
                "draft": {
                    "draft_id": DRAFT_ID,
                    "revision": 3,
                    "spec_hash": "a" * 64,
                },
                "spec": {
                    "schema_version": "agent-spec/v1",
                    "identity": {},
                    "instructions": "saved prompt",
                    "model": {
                        "model_id": "qwen3.7-plus",
                        "provider_id": "dashscope",
                    },
                    "capabilities": [],
                    "knowledge": [],
                    "memory": {"mode": "session"},
                },
                "capabilities": [],
                "knowledge": [],
            }

        async def resolve_eval_dataset_snapshot(self, **kwargs: Any) -> dict[str, Any]:
            self.dataset_requests.append(str(kwargs["dataset_id"]))
            self.dataset_request_users.append(str(kwargs["user_id"]))
            return {
                "dataset_id": str(kwargs["dataset_id"]),
                "tenant_id": str(kwargs["tenant_id"]),
                "version": "release-v3",
                "manifest_hash": "7" * 64,
                "example_count": 2,
            }

    repository = DatasetReleaseRepository()
    app = FastAPI()
    app.state.agent_repository = repository
    app.include_router(router)
    app.dependency_overrides[get_user_context] = lambda: _user()
    audience_users: list[UserContext] = []
    expires_at = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
    policy = {"attachments": False, "high_risk_tools": False, "allowed_origins": []}
    if auth_mode == "public":
        policy["expires_at"] = expires_at

    async def build_snapshot(
        _request: Any, _resolution: Any, audience_user: UserContext, **_kwargs: Any
    ) -> dict[str, Any]:
        audience_users.append(audience_user)
        return {
            "schema_version": "agent-runtime/v1",
            "tenant_id": "tenant-a",
            "agent_id": AGENT_ID,
            "publication": {"channel": channel, "auth_mode": auth_mode},
            "model": {
                "id": "qwen3.7-plus",
                "provider": "dashscope",
                "parameters": {},
            },
            "instructions": {"prompt_hash": "sha256:" + "1" * 64},
            "capabilities": [],
            "knowledge": {"datasets": []},
            "channel_policy": policy,
            "fingerprints": {
                "spec": "sha256:" + "a" * 64,
                "tool_schema": "sha256:" + "2" * 64,
                "skills": "sha256:" + "3" * 64,
                "knowledge_revision": "sha256:" + "4" * 64,
            },
        }

    async def model_authorization(**kwargs: Any) -> dict[str, Any]:
        assert kwargs["user"] is audience_users[-1]
        return build_model_authorization_evidence(
            source="agent_runtime_resolver",
            model_id="qwen3.7-plus",
            provider_id="dashscope",
            access_level="public",
            model_enabled=True,
            provider_enabled=True,
            runtime_provider_configured=True,
        )

    import src.api.v1.agent_runtime as agent_runtime_module

    monkeypatch.setattr(agent_runtime_module, "_build_snapshot", build_snapshot)
    monkeypatch.setattr(
        agents_module,
        "_resolve_release_model_authorization",
        model_authorization,
    )
    with TestClient(app) as client:
        response = client.post(
            f"/agents/{AGENT_ID}/evals",
            json={
                "draft_revision": 3,
                "channel": channel,
                "auth_mode": auth_mode,
                "channel_policy": policy,
                "dataset_id": "99999999-9999-4999-8999-999999999999",
            },
        )

    assert response.status_code == 201, response.text
    body = response.json()
    assert body["status"] == "queued"
    assert body["dataset_version"] == "release-v3"
    assert body["dataset_manifest_hash"] == "7" * 64
    assert repository.dataset_requests == ["99999999-9999-4999-8999-999999999999"]
    assert repository.preview_request_users == ["owner-a"]
    assert repository.dataset_request_users == ["owner-a"]
    assert audience_users[0].is_authenticated is (
        auth_mode != "public" and channel != "embed"
    )
    if auth_mode == "tenant":
        assert audience_users[0].user_id != "owner-a"
        assert audience_users[0].tier == "normal"
        assert audience_users[0].roles == ["user"]
    elif auth_mode == "private":
        assert audience_users[0].user_id == "owner-a"
    elif channel == "embed":
        assert audience_users[0].user_type == "anonymous"
        assert audience_users[0].roles == ["guest"]
    elif channel == "api":
        assert audience_users[0].user_type == "service"
        assert audience_users[0].roles == ["agent_runtime"]
        assert audience_users[0].user_id != "owner-a"


@pytest.mark.parametrize(
    ("path", "payload", "key"),
    [
        (
            f"/agents/{AGENT_ID}/publish",
            {"evaluation_id": EVALUATION_ID, "reason": "api_key=synthetic-test-value"},
            "publish-secret-0001",
        ),
        (
            f"/publications/{PUBLICATION_ID}/rollback",
            {
                "target_version_id": OLD_VERSION_ID,
                "reason": "Authorization: Bearer synthetic-test-value",
            },
            "rollback-secret-0001",
        ),
    ],
)
def test_release_mutations_reject_secret_shaped_audit_reasons(
    release_client,
    path: str,
    payload: dict[str, str],
    key: str,
) -> None:
    client, _, _ = release_client

    response = client.post(
        path,
        json=payload,
        headers={"Idempotency-Key": key},
    )

    assert response.status_code == 422


def test_owner_runs_server_profile_and_reads_lifecycle(release_client) -> None:
    client, repository, _ = release_client

    queued = client.post(
        f"/agents/{AGENT_ID}/evals",
        json={"draft_revision": 3, "channel": "hosted"},
    )
    created = client.post(f"/agents/{AGENT_ID}/evals/{EVALUATION_ID}/execute")

    assert queued.status_code == 201, queued.text
    assert queued.json()["status"] == "queued"
    assert [event["status"] for event in queued.json()["events"]] == ["queued"]
    assert created.status_code == 200, created.text
    body = created.json()
    assert body["status"] == "passed"
    assert [event["status"] for event in body["events"]] == [
        "queued",
        "running",
        "passed",
    ]
    assert repository.last_gate is not None
    assert repository.last_gate["model_quality_evaluated"] is False
    assert "saved prompt" not in created.text

    listed = client.get(f"/agents/{AGENT_ID}/evals")
    assert listed.status_code == 200
    assert listed.json()["evaluations"][0]["evaluation_id"] == EVALUATION_ID


def test_owner_cancels_queued_evaluation_and_execute_cannot_overwrite_it(
    release_client,
) -> None:
    client, _, _ = release_client
    queued = client.post(
        f"/agents/{AGENT_ID}/evals",
        json={"draft_revision": 3, "channel": "hosted"},
    )
    cancelled = client.post(f"/agents/{AGENT_ID}/evals/{EVALUATION_ID}/cancel")
    execute = client.post(f"/agents/{AGENT_ID}/evals/{EVALUATION_ID}/execute")

    assert queued.status_code == 201
    assert cancelled.status_code == 200
    assert cancelled.json()["status"] == "cancelled"
    assert [event["status"] for event in cancelled.json()["events"]] == [
        "queued",
        "cancelled",
    ]
    assert execute.status_code == 200
    assert execute.json()["status"] == "cancelled"


def test_list_marks_current_runtime_fingerprint_drift_stale(
    release_client,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client, _, _ = release_client

    async def drifted_candidate(**kwargs: Any) -> dict[str, Any]:
        candidate = _candidate(
            channel=kwargs["channel"],
            auth_mode=kwargs["auth_mode"],
            policy=kwargs["channel_policy"],
            dataset_id=kwargs["dataset_id"],
        )
        candidate["runtime_fingerprint_hash"] = "0" * 64
        return candidate

    monkeypatch.setattr(agents_module, "_resolve_release_candidate", drifted_candidate)
    response = client.get(f"/agents/{AGENT_ID}/evals")

    assert response.status_code == 200
    evaluation_body = response.json()["evaluations"][0]
    assert evaluation_body["status"] == "stale"
    assert evaluation_body["stale"] is True
    assert "runtime_fingerprint_changed" in evaluation_body["stale_reasons"]


def test_publish_requires_key_and_replays_same_result(release_client) -> None:
    client, repository, _ = release_client
    payload = {"evaluation_id": EVALUATION_ID, "reason": "release"}

    missing = client.post(f"/agents/{AGENT_ID}/publish", json=payload)
    first = client.post(
        f"/agents/{AGENT_ID}/publish",
        json=payload,
        headers={"Idempotency-Key": "publish-key-0001"},
    )
    repository.evaluation["stale"] = True
    repository.evaluation["status"] = "stale"
    replay = client.post(
        f"/agents/{AGENT_ID}/publish",
        json=payload,
        headers={"Idempotency-Key": "publish-key-0001"},
    )

    assert missing.status_code == 428
    assert first.status_code == 200, first.text
    assert replay.status_code == 200, replay.text
    assert first.json()["version"]["agent_version_id"] == VERSION_ID
    assert replay.json()["event"]["event_id"] == first.json()["event"]["event_id"]
    assert replay.json()["idempotent_replay"] is True
    assert repository.candidate_resolution_calls == 1


def test_publish_selected_run_uses_server_resolved_version_preview(
    release_client, monkeypatch: pytest.MonkeyPatch
) -> None:
    client, repository, _ = release_client
    run_id = str(uuid.uuid4())
    snapshot = {
        "schema_version": "agent-runtime/v1",
        "tenant_id": "tenant-a",
        "agent_id": AGENT_ID,
        "agent_version_id": OLD_VERSION_ID,
        "publication": {"id": None, "channel": "preview", "auth_mode": "private"},
        "model": {"id": "qwen3.7-plus", "provider": "dashscope", "parameters": {}},
        "knowledge": {"datasets": []},
        "capabilities": [],
        "fingerprints": {"spec": "sha256:" + "a" * 64},
    }
    target = {
        "candidate_type": "agent_version",
        "agent_id": AGENT_ID,
        "agent_version_id": OLD_VERSION_ID,
        "agent_spec_hash": "a" * 64,
        "agent_runtime_snapshot_hash": runtime_sha256(snapshot),
    }

    class TraceRepository:
        async def get_experiment_run(self, **kwargs: Any) -> dict[str, Any]:
            assert kwargs == {"tenant_id": "tenant-a", "run_id": run_id}
            return {
                "status": "succeeded", "run_mode": "live_candidate",
                "target_snapshot": target,
            }

    async def build_snapshot(_request: Any, _resolution: Any, _user: Any, *, channel: str):
        assert channel == "preview"
        return snapshot

    import src.api.v1.agent_runtime as agent_runtime_module

    monkeypatch.setattr(agent_runtime_module, "_build_snapshot", build_snapshot)
    client.app.state.agent_trace_repository = TraceRepository()
    captured: dict[str, Any] = {}
    publish = repository.publish_agent

    async def capture_publish(**kwargs: Any) -> dict[str, Any]:
        captured.update(kwargs)
        return await publish(**kwargs)

    repository.publish_agent = capture_publish  # type: ignore[method-assign]
    response = client.post(
        f"/agents/{AGENT_ID}/publish",
        json={"evaluation_id": EVALUATION_ID, "experiment_run_id": run_id},
        headers={"Idempotency-Key": "selected-run-api-0001"},
    )
    assert response.status_code == 200, response.text
    assert captured["experiment_run_id"] == run_id
    assert captured["selected_version_id"] == OLD_VERSION_ID
    assert captured["selected_version_snapshot_hash"] == runtime_sha256(snapshot)

    target["agent_runtime_snapshot_hash"] = "sha256:" + "0" * 64
    stale = client.post(
        f"/agents/{AGENT_ID}/publish",
        json={"evaluation_id": EVALUATION_ID, "experiment_run_id": run_id},
        headers={"Idempotency-Key": "selected-run-api-stale"},
    )
    assert stale.status_code == 409
    assert stale.json()["detail"]["code"] == "AGENT_EVAL_RUN_IDENTITY_MISMATCH"


def test_conflicting_idempotency_key_and_stale_eval_are_stable_conflicts(
    release_client,
) -> None:
    client, repository, _ = release_client
    first = client.post(
        f"/agents/{AGENT_ID}/publish",
        json={"evaluation_id": EVALUATION_ID, "reason": "first"},
        headers={"Idempotency-Key": "conflict-key-0001"},
    )
    conflict = client.post(
        f"/agents/{AGENT_ID}/publish",
        json={"evaluation_id": EVALUATION_ID, "reason": "different"},
        headers={"Idempotency-Key": "conflict-key-0001"},
    )
    repository.evaluation["stale"] = True
    repository.evaluation["status"] = "stale"
    stale = client.post(
        f"/agents/{AGENT_ID}/publish",
        json={"evaluation_id": EVALUATION_ID},
        headers={"Idempotency-Key": "publish-key-0002"},
    )

    assert first.status_code == 200
    assert conflict.status_code == 409
    assert conflict.json()["detail"]["code"] == "AGENT_RELEASE_IDEMPOTENCY_CONFLICT"
    assert stale.status_code == 409
    assert stale.json()["detail"]["code"] == "AGENT_EVAL_STALE"


def test_viewer_cannot_eval_publish_or_rollback(release_client) -> None:
    client, _, selected_user = release_client
    selected_user[0] = _user("viewer-a")

    publish = client.post(
        f"/agents/{AGENT_ID}/publish",
        json={"evaluation_id": EVALUATION_ID},
        headers={"Idempotency-Key": "publish-key-0003"},
    )
    rollback = client.post(
        f"/publications/{PUBLICATION_ID}/rollback",
        json={"target_version_id": OLD_VERSION_ID, "reason": "rollback"},
        headers={"Idempotency-Key": "rollback-key-0001"},
    )

    assert publish.status_code == 404
    assert rollback.status_code == 404


def test_rollback_uses_server_resolved_version_and_returns_audit_event(
    release_client,
) -> None:
    client, repository, _ = release_client

    response = client.post(
        f"/publications/{PUBLICATION_ID}/rollback",
        json={"target_version_id": OLD_VERSION_ID, "reason": "release rollback"},
        headers={"Idempotency-Key": "rollback-key-0002"},
    )
    replay = client.post(
        f"/publications/{PUBLICATION_ID}/rollback",
        json={"target_version_id": OLD_VERSION_ID, "reason": "release rollback"},
        headers={"Idempotency-Key": "rollback-key-0002"},
    )

    assert response.status_code == 200, response.text
    assert response.json()["event"]["operation"] == "rollback"
    assert response.json()["publication"]["version_id"] == OLD_VERSION_ID
    assert replay.status_code == 200, replay.text
    assert replay.json()["event"]["event_id"] == response.json()["event"]["event_id"]
    assert replay.json()["idempotent_replay"] is True
    assert repository.version_resolution_calls == 1


def test_unconfigured_production_profile_fails_closed(
    release_client,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client, repository, _ = release_client
    monkeypatch.setenv("AGENT_RELEASE_PROFILE", "production_v1")

    response = client.post(
        f"/agents/{AGENT_ID}/evals",
        json={"draft_revision": 3, "channel": "hosted"},
    )

    assert response.status_code == 503
    assert response.json()["detail"]["code"] == "AGENT_RELEASE_PROFILE_UNAVAILABLE"
    assert repository.last_gate is None


def test_repository_stale_error_mapping_includes_current_revision(release_client) -> None:
    client, repository, _ = release_client

    async def stale(**_kwargs: Any):
        raise AgentReleaseEvaluationStaleError(8)

    repository.get_release_evaluation = stale  # type: ignore[method-assign]
    response = client.post(
        f"/agents/{AGENT_ID}/publish",
        json={"evaluation_id": EVALUATION_ID},
        headers={"Idempotency-Key": "publish-key-0004"},
    )

    assert response.status_code == 409
    assert response.json()["detail"]["current_revision"] == 8


def test_release_request_unique_violation_maps_to_stable_idempotency_conflict(
    release_client,
) -> None:
    client, repository, _ = release_client

    class ReleaseRequestUniqueViolation(Exception):
        sqlstate = "23505"
        constraint_name = "agent_release_requests_pkey"

    async def conflict(**_kwargs: Any) -> dict[str, Any]:
        raise ReleaseRequestUniqueViolation()

    repository.publish_agent = conflict  # type: ignore[method-assign]
    response = client.post(
        f"/agents/{AGENT_ID}/publish",
        json={"evaluation_id": EVALUATION_ID, "reason": "release"},
        headers={"Idempotency-Key": "publish-race-0001"},
    )

    assert response.status_code == 409
    assert response.json()["detail"]["code"] == "AGENT_RELEASE_IDEMPOTENCY_CONFLICT"
