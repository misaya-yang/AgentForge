"""Eval credentials must match the job before any candidate write."""

from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import jwt
import pytest

from src.services.eval import eval_candidate_client as candidate_module
from src.services.eval import eval_outbox_worker as outbox_module
from src.services.eval.eval_candidate_client import EvalCandidateClient


def test_outbox_delegation_claims_bind_verified_subject_and_tenant(monkeypatch):
    monkeypatch.setenv("GATEWAY_AUTHENTICATION__JWT__SECRET", "fixture-signing-key-" * 3)
    monkeypatch.setenv("GATEWAY_AUTHENTICATION__JWT__ALGORITHMS", '["HS256"]')
    candidate = EvalCandidateClient(allow_service_identity=True)
    candidate.token = candidate.api_key = ""
    delegation = {"actor": "eval-worker", "subject": "verified-user", "tenant_id": "tenant-a",
                  "run_id": "run", "job_id": "job"}
    headers = candidate._auth_headers(tenant_id="tenant-a", delegation=delegation)
    claims = jwt.decode(headers["Authorization"][7:], "fixture-signing-key-" * 3, algorithms=["HS256"],
                        options={"verify_aud": False})
    assert claims["tenant_id"] == "tenant-a"
    assert claims["sub"] == claims["user_id"] == "verified-user"
    assert claims["act"]["sub"] == "eval-worker"
    assert claims["act"]["tenant_id"] == "tenant-a"
    assert claims["roles"] == ["user"] and claims["permissions"] == []
    assert claims["exp"] - claims["iat"] == 300
    candidate.token = "explicit-configured-token"
    assert candidate._auth_headers(tenant_id="tenant-b") == {"Authorization": "Bearer explicit-configured-token"}


@pytest.mark.asyncio
async def test_service_identity_still_requires_canonical_introspection(monkeypatch):
    requests = []

    def handler(request):
        requests.append(request)
        return httpx.Response(200, json={"user_id": "eval-candidate", "tenant_id": "other"})

    monkeypatch.setenv("GATEWAY_AUTHENTICATION__JWT__SECRET", "fixture-signing-key-" * 3)
    candidate = _client(monkeypatch, handler)
    candidate.allow_service_identity = True
    candidate.token = candidate.api_key = ""
    with pytest.raises(RuntimeError, match="TENANT_MISMATCH"):
        await candidate.run(tenant_id="tenant-a", run_case_id="case", message="hello", config={},
                            delegation={"actor": "eval-worker", "subject": "verified-user",
                                        "tenant_id": "tenant-a", "run_id": "run", "job_id": "job"})
    assert [(request.method, request.url.path) for request in requests] == [("GET", "/api/v1/auth/me")]


def _client(monkeypatch, handler, *, mode="jwt") -> EvalCandidateClient:
    real_client = httpx.AsyncClient
    monkeypatch.setattr(candidate_module.httpx, "AsyncClient", lambda **kwargs: real_client(
        **kwargs, transport=httpx.MockTransport(handler),
    ))
    candidate = EvalCandidateClient()
    candidate.base_url = "http://gateway"
    candidate.token = "test-token" if mode == "jwt" else ""
    candidate.api_key = "test-key" if mode == "api_key" else ""
    return candidate


@pytest.mark.parametrize("mode", ["jwt", "api_key"])
@pytest.mark.parametrize("job_tenant,credential_tenant", [("tenant-b", "tenant-a"), ("tenant-a", "tenant-b")])
async def test_mismatched_identity_never_creates_candidate(monkeypatch, mode, job_tenant, credential_tenant):
    requests = []

    def handler(request):
        requests.append(request)
        return httpx.Response(200, json={"user_id": "eval-user", "tenant_id": credential_tenant})

    callback = AsyncMock()
    candidate = _client(monkeypatch, handler, mode=mode)
    with pytest.raises(RuntimeError, match="AGENT_EVAL_TENANT_MISMATCH"):
        await candidate.run(
            tenant_id=job_tenant, run_case_id="case", message="hello", config={},
            on_run_started=callback,
        )

    assert [(r.method, r.url.path) for r in requests] == [("GET", "/api/v1/auth/me")]
    assert "X-Tenant-Id" not in requests[0].headers
    assert "X-User-Id" not in requests[0].headers
    callback.assert_not_awaited()


@pytest.mark.parametrize("payload", [
    {}, [], {"tenant_id": "tenant-a"}, {"user_id": "eval-user"},
    {"user_id": "", "tenant_id": "tenant-a"},
    {"user_id": "eval-user", "tenant_id": ""},
    {"user_id": "eval-user", "tenant_id": "public"},
    {"user_id": "eval-user", "tenant_id": 12},
    {"user_id": "eval-user", "tenant_id": " tenant-a "},
])
async def test_missing_or_unscoped_identity_fails_closed(monkeypatch, payload):
    requests = []

    def handler(request):
        requests.append(request)
        return httpx.Response(200, json=payload)

    candidate = _client(monkeypatch, handler)
    with pytest.raises(RuntimeError, match="AGENT_EVAL_IDENTITY_INVALID"):
        await candidate.run(tenant_id="tenant-a", run_case_id="case", message="hello", config={})
    assert len(requests) == 1
    assert requests[0].method == "GET"


@pytest.mark.parametrize("status", [302, 401, 403, 500])
async def test_identity_http_failure_never_follows_redirect_or_writes(monkeypatch, status):
    requests = []

    def handler(request):
        requests.append(request)
        return httpx.Response(status, headers={"Location": "https://other.invalid"}, text="private response")

    candidate = _client(monkeypatch, handler)
    with pytest.raises(RuntimeError, match="AGENT_EVAL_IDENTITY_UNAVAILABLE") as caught:
        await candidate.run(tenant_id="tenant-a", run_case_id="case", message="hello", config={})
    assert "private response" not in str(caught.value)
    assert [(r.method, r.url.path) for r in requests] == [("GET", "/api/v1/auth/me")]


@pytest.mark.parametrize("failure", ["json", "timeout"])
async def test_identity_transport_and_json_failures_are_safe(monkeypatch, failure):
    requests = []

    def handler(request):
        requests.append(request)
        if failure == "timeout":
            raise httpx.ReadTimeout("private transport context", request=request)
        return httpx.Response(200, text="private non-json response")

    candidate = _client(monkeypatch, handler)
    with pytest.raises(RuntimeError, match="AGENT_EVAL_IDENTITY_(INVALID|UNAVAILABLE)") as caught:
        await candidate.run(tenant_id="tenant-a", run_case_id="case", message="hello", config={})
    assert "private" not in str(caught.value)
    assert len(requests) == 1


@pytest.mark.parametrize("job_tenant,credential_tenant", [("tenant-b", "tenant-a"), ("tenant-a", "tenant-b")])
async def test_real_candidate_runner_does_not_ingest_a_mismatched_trace(
    monkeypatch, job_tenant, credential_tenant,
):
    requests = []

    def handler(request):
        requests.append(request)
        return httpx.Response(200, json={"user_id": "eval-user", "tenant_id": credential_tenant})

    candidate = _client(monkeypatch, handler)
    monkeypatch.setattr(outbox_module, "_eval_candidate_client", candidate)
    repository = SimpleNamespace(
        list_traces=AsyncMock(return_value=([], 0)),
        get_trace_detail=AsyncMock(return_value=None),
        ingest_trace=AsyncMock(),
        update_experiment_run_case=AsyncMock(),
    )
    with pytest.raises(RuntimeError, match="AGENT_EVAL_TENANT_MISMATCH"):
        await outbox_module._build_candidate_runner(repository)(
            tenant_id=job_tenant,
            run_case={"run_case_id": "case", "input": {"message": "hello"}},
            execution_config={},
        )

    repository.ingest_trace.assert_not_awaited()
    repository.update_experiment_run_case.assert_not_awaited()
    assert [(r.method, r.url.path) for r in requests] == [("GET", "/api/v1/auth/me")]


async def test_reused_candidate_checks_identity_on_every_run(monkeypatch):
    tenants = iter(["tenant-a", "tenant-b"])
    methods = []

    def handler(request):
        methods.append(request.method)
        return httpx.Response(200, json={"user_id": "eval-user", "tenant_id": next(tenants)})

    candidate = _client(monkeypatch, handler)
    for job_tenant in ["tenant-b", "tenant-a"]:
        with pytest.raises(RuntimeError, match="AGENT_EVAL_TENANT_MISMATCH"):
            await candidate.run(tenant_id=job_tenant, run_case_id="case", message="hello", config={})
    assert methods == ["GET", "GET"]
