"""Production ASGI route and repository with a transactional SQL-boundary double.

The double models RETURNING, global UUID conflicts, rollbacks and outbox dedupe.
It is not evidence of actual PostgreSQL constraints, locks or concurrent commits.
"""
from __future__ import annotations

import asyncio
import json
import re
from contextlib import asynccontextmanager
from copy import deepcopy
from types import SimpleNamespace

import httpx
import pytest
from ai_gateway_core.persistence.repositories.agent_trace_repository import (
    AgentTraceRepository,
    EvalLeaseLost,
)
from fastapi import FastAPI

from src.api.deps import AuthContext, get_auth_context
from src.api.v1 import eval as eval_api
from src.core.auth.rbac import RBAC

TRACE = "10000000-0000-4000-8000-000000000001"
OTHER_TRACE = "10000000-0000-4000-8000-000000000002"
SPAN = "20000000-0000-4000-8000-000000000001"
OTHER_SPAN = "20000000-0000-4000-8000-000000000002"
EVENT = "30000000-0000-4000-8000-000000000001"


def normalize(sql):
    return re.sub(r"\s+", " ", sql).strip().lower()


class TransactionalTraceDatabase:
    enabled = True

    def __init__(self):
        self._pool = self
        self.traces = {}
        self.spans = {}
        self.events = {}
        self.jobs = {}
        self.calls = []
        self.depth = 0
        self.acquisitions = 0
        self.commits = 0
        self.rollbacks = 0
        self.fail_at = None
        self.cancel_at = None
        self.claim_valid = True

    def snapshot(self):
        return deepcopy((self.traces, self.spans, self.events, self.jobs))

    @asynccontextmanager
    async def acquire(self):
        self.acquisitions += 1
        yield self

    @asynccontextmanager
    async def transaction(self):
        snapshot = self.snapshot()
        self.depth += 1
        try:
            yield
        except BaseException:
            self.traces, self.spans, self.events, self.jobs = snapshot
            self.rollbacks += 1
            raise
        else:
            self.commits += 1
        finally:
            self.depth -= 1

    def record(self, kind, query, args):
        self.calls.append((kind, normalize(query), args, self.depth))
        if self.cancel_at == kind:
            raise asyncio.CancelledError()
        if self.fail_at == kind:
            raise RuntimeError("synthetic storage fault")

    async def execute(self, query, *args):
        self.record("dimensions", query, args)
        return "UPDATE 1"

    async def fetchrow(self, query, *args):
        sql = normalize(query)
        if "from agent_trace_outbox" in sql and "for update" in sql:
            self.record("claim", query, args)
            return {"job_id": args[0]} if self.claim_valid else None
        if "insert into agent_traces" in sql:
            self.record("trace", query, args)
            old = self.traces.get(str(args[0]))
            # Model the actual conflict UPDATE predicate, not a pre-query guard.
            if old and "where agent_traces.tenant_id = excluded.tenant_id" in sql:
                if old["tenant_id"] != args[3]:
                    return None
                assert "and ($31::text is null or agent_traces.user_id = $31)" in sql
                if args[30] is not None and old["user_id"] != args[30]:
                    return None
            self.traces[str(args[0])] = {
                "trace_id": str(args[0]), "tenant_id": args[3], "user_id": args[4],
                "trace_family": args[1], "output_preview": args[23],
            }
            return {"trace_id": str(args[0])}
        if "insert into agent_trace_spans" in sql:
            self.record("span", query, args)
            old = self.spans.get(str(args[0]))
            if old and "where agent_trace_spans.trace_id = excluded.trace_id" in sql and old["trace_id"] != str(args[1]):
                return None
            if old:
                old["output_preview"] = args[11]
            else:
                self.spans[str(args[0])] = {"span_id": str(args[0]), "trace_id": str(args[1]), "output_preview": args[11]}
            return {"span_id": str(args[0])}
        if sql.startswith("select span_id from agent_trace_spans"):
            self.record("reference", query, args)
            row = self.spans.get(str(args[1]))
            return row if row and row["trace_id"] == str(args[0]) else None
        if "insert into agent_trace_events" in sql:
            self.record("event", query, args)
            self.events[(str(args[0]), args[3])] = {"trace_id": str(args[0]), "span_id": args[1], "payload": args[5]}
            return {"event_id": EVENT}
        if "insert into agent_trace_outbox" in sql:
            self.record("outbox", query, args)
            key = (args[0], str(args[2]))
            old = self.jobs.get(key)
            runtime = args[3] if len(args) > 3 else "status in ('queued', 'running')" not in sql
            if old and (runtime or old["status"] in ("queued", "running")):
                return None
            self.jobs[key] = {"job_id": "40000000-0000-4000-8000-000000000001", "status": "queued", "payload": json.loads(args[1])}
            return self.jobs[key]
        if "select trace_id from agent_traces" in sql:
            self.record("trace_lock", query, args)
            row = self.traces.get(str(args[0]))
            return row if row and row["tenant_id"] == args[1] else None
        raise AssertionError(f"Unexpected SQL kind: {sql[:100]}")


def payload(*, owner=None, span=SPAN, parent=None, event_span=SPAN, adapter="api", enqueue=True):
    return {"trace": {
        "trace_id": TRACE, "user_id": owner, "source_adapter": adapter, "output_preview": "changed",
        "spans": [{"span_id": span, "trace_id": TRACE, "parent_span_id": parent,
                   "span_kind": "custom", "name": "span", "status": "succeeded", "output_preview": "changed"}],
        "events": [{"event_id": EVENT, "trace_id": TRACE, "span_id": event_span,
                    "event_type": "terminal", "sequence_no": 1, "payload": {"safe": True}}],
    }, "enqueue": enqueue}


def seed(db, *, tenant="tenant-b", owner="other", trace=TRACE, span=OTHER_SPAN):
    db.traces[trace] = {"trace_id": trace, "tenant_id": tenant, "user_id": owner,
                        "trace_family": "assistant", "output_preview": "private"}
    db.spans[span] = {"span_id": span, "trace_id": trace, "output_preview": "private"}


def fixture(monkeypatch, *, roles=("user",), tenant="tenant-a", permissions=True):
    db = TransactionalTraceDatabase()
    repo = AgentTraceRepository(db)
    monkeypatch.setattr(eval_api, "_get_trace_repository", lambda _: repo)
    auth = AuthContext(user_id="actor", tenant_id=tenant, roles=list(roles),
                       permissions=["console:eval:view", "console:eval:run"] if permissions else [],
                       is_authenticated=True)
    app = FastAPI()
    app.include_router(eval_api.router, prefix="/api/v1")
    app.dependency_overrides[get_auth_context] = lambda: auth
    app.state.dispatcher = SimpleNamespace(rbac=RBAC(role_permissions={}))
    return db, repo, app


async def post(app, body):
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://test") as client:
        return await client.post("/api/v1/eval/ingest/traces", json=body)


@pytest.mark.parametrize("roles", [("user",), ("admin",), ("operator",), ("platform_admin",), ("admin", "platform_admin")])
async def test_foreign_tenant_trace_conflict_is_denied_before_children(monkeypatch, roles):
    db, _, app = fixture(monkeypatch, roles=roles)
    seed(db)
    before = db.snapshot()
    response = await post(app, payload())
    assert response.status_code == 404
    assert response.json() == {"detail": "Trace not found"}
    assert db.snapshot() == before
    assert [call[0] for call in db.calls] == ["trace"]
    assert db.rollbacks == 1 and db.commits == 0


@pytest.mark.parametrize("roles", [("user",), ("platform_admin",), ("Admin",), ("tenant_admin",)])
@pytest.mark.parametrize("owner", [None, "other"])
async def test_ordinary_scope_cannot_overwrite_or_attribute_another_user(monkeypatch, roles, owner):
    db, _, app = fixture(monkeypatch, roles=roles)
    seed(db, tenant="tenant-a")
    before = db.snapshot()
    response = await post(app, payload(owner=owner))
    assert response.status_code == 404
    assert db.snapshot() == before
    assert all(call[0] == "trace" for call in db.calls)


@pytest.mark.parametrize("roles,existing_owner,new_owner", [
    (("user",), None, None), (("user",), "actor", "actor"),
    (("admin",), "other", "other"), (("operator",), "other", "third-user"),
    (("admin", "platform_admin"), "other", None), (("platform_admin",), "actor", None),
])
async def test_owner_and_tenant_operator_ingestion_commit_one_complete_tree(monkeypatch, roles, existing_owner, new_owner):
    db, _, app = fixture(monkeypatch, roles=roles)
    if existing_owner:
        seed(db, tenant="tenant-a", owner=existing_owner)
    response = await post(app, payload(owner=new_owner))
    assert response.status_code == 202
    assert response.json()["status"] == "stored"
    assert db.traces[TRACE]["tenant_id"] == "tenant-a"
    assert db.traces[TRACE]["user_id"] == (new_owner or "actor")
    assert db.spans[SPAN]["trace_id"] == TRACE
    assert (TRACE, 1) in db.events and ("tenant-a", TRACE) in db.jobs
    assert db.acquisitions == db.commits == 1 and db.rollbacks == 0
    assert all(call[3] == 1 for call in db.calls)


@pytest.mark.parametrize("kind", ["span_collision", "parent_reference", "event_reference"])
@pytest.mark.parametrize("tenant", ["tenant-a", "tenant-b"])
async def test_foreign_child_identity_rolls_back_parent_and_prior_children(monkeypatch, kind, tenant):
    db, _, app = fixture(monkeypatch, roles=("admin",))
    seed(db, trace=OTHER_TRACE, tenant=tenant)
    body = payload(span=OTHER_SPAN, event_span=OTHER_SPAN) if kind == "span_collision" else payload(
        parent=OTHER_SPAN if kind == "parent_reference" else None,
        event_span=OTHER_SPAN if kind == "event_reference" else SPAN,
    )
    before = db.snapshot()
    response = await post(app, body)
    assert response.status_code == 404
    assert db.snapshot() == before
    assert db.rollbacks == 1 and not db.commits
    assert not any(call[0] == "outbox" for call in db.calls)


@pytest.mark.parametrize("kind", ["trace", "span", "event", "outbox", "dimensions"])
@pytest.mark.parametrize("fault", ["error", "cancel"])
async def test_storage_or_cancellation_rolls_back_without_partial_receipt(monkeypatch, kind, fault):
    db, repo, _ = fixture(monkeypatch)
    seed(db, tenant="tenant-a", owner="actor")
    before = db.snapshot()
    setattr(db, "fail_at" if fault == "error" else "cancel_at", kind)
    error = RuntimeError if fault == "error" else asyncio.CancelledError
    with pytest.raises(error):
        await repo.ingest_trace(tenant_id="tenant-a", created_by="actor", payload=payload(adapter="gateway.agent_runtime"))
    assert db.snapshot() == before
    assert db.rollbacks == 1 and db.commits == 0


@pytest.mark.parametrize("adapter,previous,expected_job", [
    ("gateway.agent_runtime", "done", False), ("gateway.agent_runtime", "queued", False),
    ("api", "done", True), ("api", "queued", False),
])
async def test_existing_enqueue_deduplication_semantics_preserved(monkeypatch, adapter, previous, expected_job):
    db, _, app = fixture(monkeypatch)
    seed(db, tenant="tenant-a", owner="actor")
    db.jobs[("tenant-a", TRACE)] = {"job_id": "existing", "status": previous}
    response = await post(app, payload(adapter=adapter))
    assert response.status_code == 202
    assert bool(response.json()["job_id"]) == expected_job
    assert db.commits == 1


async def test_trusted_internal_capture_preserves_subject_attribution(monkeypatch):
    db, repo, _ = fixture(monkeypatch)
    result = await repo.ingest_trace(tenant_id="tenant-a", created_by="eval-worker", payload=payload(owner="candidate-subject", adapter="gateway.agent_runtime"))
    assert result["status"] == "stored"
    assert db.traces[TRACE]["user_id"] == "candidate-subject"
    assert [call[0] for call in db.calls].count("dimensions") == 1
    assert all(call[3] == 1 for call in db.calls)


@pytest.mark.parametrize("valid,claim_tenant", [(False, "tenant-a"), (True, "tenant-b"), (True, "tenant-a")])
async def test_outbox_claim_is_revalidated_inside_matching_ingest_transaction(monkeypatch, valid, claim_tenant):
    db, repo, _ = fixture(monkeypatch)
    db.claim_valid = valid
    with repo.bind_outbox_claim({"job_id": "job", "tenant_id": claim_tenant, "owner_id": "worker", "claim_token": "claim"}):
        if not valid or claim_tenant != "tenant-a":
            with pytest.raises(EvalLeaseLost):
                await repo.ingest_trace(tenant_id="tenant-a", created_by="actor", payload=payload())
            assert not db.traces and not db.jobs
            assert all(call[0] == "claim" for call in db.calls)
        else:
            result = await repo.ingest_trace(tenant_id="tenant-a", created_by="actor", payload=payload())
            assert result["status"] == "stored"
            assert db.calls[0][0] == "claim"
            assert len([call for call in db.calls if call[0] == "claim"]) == 1
            assert all(call[3] == 1 for call in db.calls)


@pytest.mark.parametrize("tenant,permissions", [("", True), ("tenant-a", False)])
async def test_handler_keeps_existing_tenant_and_capability_admission(monkeypatch, tenant, permissions):
    db, _, app = fixture(monkeypatch, tenant=tenant, permissions=permissions)
    response = await post(app, payload())
    assert response.status_code == 403
    assert db.calls == []


async def test_enqueue_false_keeps_no_job_semantics(monkeypatch):
    db, _, app = fixture(monkeypatch)
    response = await post(app, payload(enqueue=False))
    assert response.status_code == 202 and response.json()["job_id"] is None
    assert not db.jobs and db.commits == 1


@pytest.mark.parametrize("claim_tenant,valid", [("tenant-b", True), ("tenant-a", False)])
async def test_standalone_enqueue_retains_claim_tenant_and_lease_fences(monkeypatch, claim_tenant, valid):
    db, repo, _ = fixture(monkeypatch)
    seed(db, tenant="tenant-a", owner="actor")
    before = db.snapshot()
    db.claim_valid = valid
    with repo.bind_outbox_claim({"job_id": "job", "tenant_id": claim_tenant, "owner_id": "worker", "claim_token": "claim"}):
        with pytest.raises(EvalLeaseLost):
            await repo.create_trace_ingested_outbox_job(
                tenant_id="tenant-a", trace_id=TRACE, trace_family="assistant",
                status="succeeded", source_adapter="api",
            )
    assert db.snapshot() == before
    assert not any(call[0] == "outbox" for call in db.calls)
