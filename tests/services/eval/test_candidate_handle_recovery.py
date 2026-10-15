import json

import httpx
import pytest

from src.services.eval import eval_candidate_client as client_module
from src.services.eval import eval_outbox_worker as worker_module
from src.services.eval.eval_candidate_client import EvalCandidateClient


def terminal_stream():
    frames = []
    for sequence, (kind, data) in enumerate([
        ("run_started", {"run_id": "turn-1"}),
        ("text_delta", {"content": "answer"}),
        ("run_finished", {"status": "succeeded"}),
    ], 1):
        frames.append("data: " + json.dumps({
            "sequence": sequence, "event": {"turn_id": "turn-1", "payload": {"event_type": kind, "data": data}},
        }) + "\n\n")
    return "".join(frames)


@pytest.mark.asyncio
async def test_accepted_handle_resumes_without_creating_another_turn(monkeypatch):
    calls, saved = [], []

    def handler(request):
        calls.append((request.method, request.url.path))
        if request.url.path.endswith("/auth/me"):
            return httpx.Response(200, json={"user_id": "user", "tenant_id": "tenant"})
        if request.method == "POST" and request.url.path.endswith("/turns"):
            return httpx.Response(200, json={"turn": {"id": "turn-1", "events_url": "/api/v2/agent/threads/thread-1/events"}})
        if request.url.path.endswith("/events"):
            return httpx.Response(200, text=terminal_stream())
        return httpx.Response(200, json={"thread": {"thread_id": "thread-1", "runtime": {"owner": "agent_runtime"}}})

    real_client = httpx.AsyncClient
    monkeypatch.setattr(client_module.httpx, "AsyncClient", lambda **kwargs: real_client(
        **kwargs, transport=httpx.MockTransport(handler),
    ))
    client = EvalCandidateClient()
    client.token, client.api_key, client.base_url = "fixture-token", "", "http://gateway"

    async def marker(handle):
        assert not any(path.endswith("/turns") for _, path in calls)
        saved.append(dict(handle))

    async def crash_after_accept(handle):
        saved.append(dict(handle))
        raise RuntimeError("simulated worker death before SSE")

    with pytest.raises(RuntimeError, match="simulated worker death"):
        await client.run(tenant_id="tenant", run_case_id="case", message="hello", config={},
                         on_dispatch_started=marker, on_turn_created=crash_after_accept)
    assert saved[-1]["turn_id"] == "turn-1"
    result = await client.run(tenant_id="tenant", run_case_id="case", message="hello", config={},
                              resume_handle=saved[-1])
    assert result.output == "answer"
    assert sum(method == "POST" and path.endswith("/turns") for method, path in calls) == 1


@pytest.mark.asyncio
async def test_uncertain_dispatch_is_never_blindly_replayed(monkeypatch):
    calls = []

    class Repository:
        async def list_traces(self, **_kwargs):
            return [], 0

        async def reconcile_candidate_handle(self, **_kwargs):
            return None

    class Candidate:
        async def run(self, **kwargs):
            calls.append(kwargs)

    monkeypatch.setattr(worker_module, "_eval_candidate_client", Candidate())
    with pytest.raises(RuntimeError, match="DISPATCH_RECONCILIATION_REQUIRED"):
        await worker_module._build_candidate_runner(Repository())(
            tenant_id="tenant", execution_config={},
            run_case={"run_case_id": "case", "dispatch_state": "dispatching",
                      "runtime_handle": {"thread_id": "thread"}, "input": {"message": "send once"}},
        )
    assert calls == []


@pytest.mark.asyncio
async def test_resume_rejects_rebound_credential_before_reading_handle(monkeypatch):
    calls = []

    def handler(request):
        calls.append(request.url.path)
        return httpx.Response(200, json={"user_id": "different-user", "tenant_id": "tenant"})

    real_client = httpx.AsyncClient
    monkeypatch.setattr(client_module.httpx, "AsyncClient", lambda **kwargs: real_client(
        **kwargs, transport=httpx.MockTransport(handler),
    ))
    client = EvalCandidateClient()
    client.token, client.base_url = "fixture-token", "http://gateway"
    with pytest.raises(RuntimeError, match="HANDLE_IDENTITY_MISMATCH"):
        await client.run(tenant_id="tenant", run_case_id="case", message="hello", config={},
                         resume_handle={"tenant_id": "tenant", "user_id": "original-user", "run_case_id": "case"})
    assert calls == ["/api/v1/auth/me"]
