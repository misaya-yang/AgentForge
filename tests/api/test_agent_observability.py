from __future__ import annotations

import uuid
from datetime import datetime, timezone
from typing import Any

from ai_gateway_core.persistence.repositories.agent_repository import AgentNotFoundError
from fastapi import FastAPI
from fastapi.testclient import TestClient

from src.api.deps import get_user_context
from src.api.v1.agents import router
from src.core.auth.user_resolver import UserContext

AGENT_ID = str(uuid.uuid4())
VERSION_ID = str(uuid.uuid4())
PUBLICATION_ID = str(uuid.uuid4())
TRACE_ID = str(uuid.uuid4())


def _user(role: str = "owner") -> UserContext:
    return UserContext(
        user_id=f"{role}-user",
        tenant_id="tenant-a",
        tier="normal",
        is_authenticated=True,
        roles=["user"],
        ip="127.0.0.1",
    )


class _Repository:
    def __init__(self, role: str = "owner") -> None:
        self.role = role
        self.policy = {
            "tenant_id": "tenant-a",
            "agent_id": AGENT_ID,
            "trace_retention_days": 90,
            "runtime_retention_days": 30,
            "attachment_retention_days": 1,
            "legal_hold": False,
            "principal_requests_per_minute": 30,
            "principal_requests_per_day": 1000,
            "ip_requests_per_minute": 60,
            "ip_requests_per_day": 2000,
            "publication_requests_per_minute": 300,
            "publication_requests_per_day": 10000,
            "max_agents_per_tenant": 100,
            "max_active_publications": 10,
            "max_concurrent_runs": 25,
            "max_daily_tokens": 10000000,
            "max_daily_mcp_calls": 100000,
            "max_storage_bytes": 10737418240,
            "alert_threshold_percent": 90,
            "cache_epoch": 0,
            "updated_by": "owner-user",
            "created_at": None,
            "updated_at": None,
        }
        self.last_policy_changes: dict[str, Any] | None = None
        self.prepared_status = "completed"
        self.finished_storage_ok: bool | None = None

    async def get_agent(self, **_: Any) -> dict[str, Any]:
        return {"agent_id": AGENT_ID, "caller_role": self.role}

    async def list_agent_audit_events(self, **kwargs: Any):
        if self.role != "owner" and not kwargs.get("is_tenant_admin"):
            raise AgentNotFoundError("AGENT_NOT_FOUND")
        now = datetime.now(timezone.utc).isoformat()
        return [
            {
                "id": 7,
                "user_id": "owner-user",
                "action": "api_token_revoke",
                "status": "success",
                "agent_id": AGENT_ID,
                "agent_version_id": VERSION_ID,
                "publication_id": PUBLICATION_ID,
                "channel": "api",
                "request_summary": {"authorization": "***", "token_id": "opaque"},
                "response_summary": {},
                "redaction_state": {"sensitive_fields": "removed"},
                "created_at": now,
            }
        ], 1

    async def get_governance_policy(self, **_: Any) -> dict[str, Any]:
        return dict(self.policy)

    async def update_governance_policy(self, **kwargs: Any) -> dict[str, Any]:
        self.last_policy_changes = kwargs["changes"]
        self.policy.update(kwargs["changes"])
        return dict(self.policy)

    async def invalidate_agent_caches(self, **_: Any) -> dict[str, Any]:
        self.policy["cache_epoch"] += 1
        return {**self.policy, "deleted_cache_rows": 2}

    async def revoke_agent_credentials(self, **_: Any) -> dict[str, int]:
        return {"api_tokens": 2, "mcp_channel_grants": 1, "connector_grants": 1}

    async def prepare_agent_data_deletion(self, **kwargs: Any) -> dict[str, Any]:
        now = datetime.now(timezone.utc).isoformat()
        return {
            "deletion_id": str(uuid.uuid4()),
            "tenant_id": kwargs["tenant_id"],
            "agent_id": kwargs["agent_id"],
            "scope": kwargs["scope"],
            "subject_user_id": kwargs["subject_user_id"],
            "status": self.prepared_status,
            "object_keys": [],
            "deleted_counts": {},
            "error_code": "AGENT_LEGAL_HOLD_ACTIVE" if self.prepared_status == "blocked" else None,
            "requested_by": kwargs["user_id"],
            "requested_at": now,
            "completed_at": now if self.prepared_status != "pending" else None,
        }

    async def finish_agent_data_deletion(self, **kwargs: Any) -> dict[str, Any]:
        self.finished_storage_ok = kwargs["storage_cleanup_succeeded"]
        now = datetime.now(timezone.utc).isoformat()
        return {
            "deletion_id": kwargs["deletion_id"],
            "tenant_id": kwargs["tenant_id"],
            "agent_id": kwargs["agent_id"],
            "scope": "retention",
            "subject_user_id": None,
            "status": "completed" if self.finished_storage_ok else "failed",
            "deleted_counts": {"traces": 3},
            "error_code": None if self.finished_storage_ok else "AGENT_STORAGE_CLEANUP_FAILED",
            "requested_by": kwargs["user_id"],
            "requested_at": now,
            "completed_at": now,
        }


class _TraceRepository:
    def __init__(self) -> None:
        self.filters: dict[str, Any] = {}

    async def get_agent_operations_summary(self, **kwargs: Any) -> dict[str, Any]:
        self.filters = kwargs
        return {
            "total_runs": 2,
            "succeeded_runs": 1,
            "failed_runs": 1,
            "success_rate": 0.5,
            "p95_latency_ms": 120,
            "p95_ttft_ms": 35,
            "tool_calls": 5,
            "tool_succeeded": 4,
            "tool_success_rate": 0.8,
            "knowledge_queries": 3,
            "knowledge_hits": 2,
            "knowledge_hit_rate": 2 / 3,
            "feedback_count": 2,
            "positive_feedback_count": 1,
            "feedback_positive_rate": 0.5,
            "retention_limited": True,
            "retention": {"trace_retention_days": 90, "legal_hold": False},
            "breakdown": [],
        }

    async def list_traces(self, **kwargs: Any):
        self.filters.update(kwargs)
        now = datetime.now(timezone.utc).isoformat()
        return [
            {
                "trace_id": TRACE_ID,
                "trace_family": "assistant",
                "workflow_kind": "agent_chat",
                "tenant_id": "tenant-a",
                "user_id": "opaque-principal",
                "agent_id": AGENT_ID,
                "agent_version_id": VERSION_ID,
                "publication_id": PUBLICATION_ID,
                "channel": "api",
                "status": "succeeded",
                "input_preview": "authorization=secret-value",
                "output_preview": "Bearer raw-token",
                "redaction_state": {},
                "metadata": {"internal_prompt": "must not project"},
                "metrics": {},
                "privacy": {"secret_ref": "vault://hidden"},
                "total_latency_ms": 120,
                "total_tokens": 22,
                "total_cost_cents": 1,
                "started_at": now,
                "created_at": now,
            }
        ], 1


def _client(role: str = "owner") -> tuple[TestClient, _Repository, _TraceRepository]:
    app = FastAPI()
    app.include_router(router, prefix="/api/v1")
    repository = _Repository(role)
    trace_repository = _TraceRepository()
    app.state.agent_repository = repository
    app.state.agent_trace_repository = trace_repository
    app.dependency_overrides[get_user_context] = lambda: _user(role)
    return TestClient(app), repository, trace_repository


def test_agent_analytics_uses_explicit_dimensions_and_redacts_previews() -> None:
    client, _, traces = _client()
    response = client.get(
        f"/api/v1/agents/{AGENT_ID}/analytics",
        params={
            "agent_version_id": VERSION_ID,
            "publication_id": PUBLICATION_ID,
            "channel": "api",
            "started_after": "2026-07-01T00:00:00Z",
        },
    )
    assert response.status_code == 200
    payload = response.json()
    assert payload["metrics"]["retention_limited"] is True
    assert payload["metrics"]["p95_ttft_ms"] == 35
    assert payload["metrics"]["tool_success_rate"] == 0.8
    assert payload["metrics"]["knowledge_hit_rate"] == 2 / 3
    assert payload["metrics"]["feedback_positive_rate"] == 0.5
    assert payload["traces"][0]["agent_version_id"] == VERSION_ID
    assert "secret-value" not in payload["traces"][0]["input_preview"]
    assert "raw-token" not in payload["traces"][0]["output_preview"]
    assert "internal_prompt" not in response.text
    assert traces.filters["agent_id"] == AGENT_ID
    assert traces.filters["agent_version_id"] == VERSION_ID
    assert traces.filters["publication_id"] == PUBLICATION_ID
    assert traces.filters["channel"] == "api"


def test_audit_is_owner_only_paginated_and_redacted() -> None:
    owner_client, _, _ = _client("owner")
    response = owner_client.get(f"/api/v1/agents/{AGENT_ID}/audit-events?limit=10")
    assert response.status_code == 200
    assert response.json()["total"] == 1
    assert "secret" not in response.text
    assert response.json()["events"][0]["redaction_state"]["sensitive_fields"] == "removed"

    viewer_client, _, _ = _client("viewer")
    denied = viewer_client.get(f"/api/v1/agents/{AGENT_ID}/audit-events")
    assert denied.status_code == 404


def test_governance_policy_cache_and_credential_controls() -> None:
    client, repository, _ = _client()
    updated = client.put(
        f"/api/v1/agents/{AGENT_ID}/governance",
        json={"legal_hold": True, "trace_retention_days": 365},
    )
    assert updated.status_code == 200
    assert updated.json()["legal_hold"] is True
    assert repository.last_policy_changes == {
        "trace_retention_days": 365,
        "legal_hold": True,
    }

    invalidated = client.post(f"/api/v1/agents/{AGENT_ID}/governance/cache:invalidate")
    assert invalidated.status_code == 200
    assert invalidated.json() == {
        "request_id": invalidated.json()["request_id"],
        "cache_epoch": 1,
        "deleted_cache_rows": 2,
    }

    revoked = client.post(f"/api/v1/agents/{AGENT_ID}/governance/credentials:revoke")
    assert revoked.status_code == 200
    assert revoked.json()["revoked"]["api_tokens"] == 2


def test_legal_hold_returns_durable_blocked_deletion() -> None:
    client, repository, _ = _client()
    repository.prepared_status = "blocked"
    response = client.post(
        f"/api/v1/agents/{AGENT_ID}/governance/data-deletions",
        json={"scope": "tenant", "idempotency_key": "delete-tenant-0001"},
    )
    assert response.status_code == 200
    assert response.json()["status"] == "blocked"
    assert response.json()["error_code"] == "AGENT_LEGAL_HOLD_ACTIVE"
    assert repository.finished_storage_ok is None
