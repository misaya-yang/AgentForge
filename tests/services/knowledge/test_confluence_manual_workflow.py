"""Production ASGI routes -> adapter -> sync coordinator -> source lifecycle.

Only transport/persistence/current-actor boundaries are controlled. No listeners,
real credentials, provider or PostgreSQL mutation is used.
"""

import asyncio
import copy
import json
from contextlib import asynccontextmanager
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from fastapi import FastAPI
from knowledge_service.api.deps import get_user_context
from knowledge_service.api.routes.confluence_manual_sync import router
from knowledge_service.auth.user_context import UserContext
from knowledge_service.core.exceptions import PermissionDeniedError
from knowledge_service.services.knowledge.confluence.client import ConfluenceClient
from knowledge_service.services.knowledge.confluence.manual_sync import ConfluenceManualSync
from knowledge_service.services.knowledge.document_service import DocumentService

from tests.services.knowledge.test_confluence_shared_source_detach import SourceDatabase

TASK = "4fd4c558-18f1-4b33-9314-675cf4907a01"
PATH = "/knowledge/dataset/sources/confluence/first/sync"


class ManualDatabase(SourceDatabase):
    def __init__(self):
        super().__init__()
        self.tasks = {}
        self._pool = self
        self.connection = {
            "connection_id": "connection",
            "tenant_id": "tenant",
            "created_by": "owner",
            "status": "active",
            "updated_at": "connection-v1",
            "domain": "fixture.atlassian.net",
            "email": "fixture@example.invalid",
            "api_token": "synthetic-test-only",
        }
        for b in self.bindings.values():
            b.update(
                status="completed",
                space_id="space",
                root_page_ids=["root"],
                max_depth=1,
                last_sync_at="previous-success",
                sync_images=False,
            )
        self.pages["root"] = {
            "id": "legacy-root",
            "binding_id": "first",
            "page_id": "root",
            "document_id": "root-document",
            "version": 1,
            "content_hash": "same",
        }
        self.pages["first"]["content_hash"] = "same"
        self.pages["second"]["content_hash"] = "same"
        self.txn = asyncio.Lock()

    @asynccontextmanager
    async def acquire(self):
        yield self

    @asynccontextmanager
    async def transaction(self):
        async with self.txn:
            snapshot = copy.deepcopy(
                (self.dataset, self.document, self.pages, self.bindings, self.tasks)
            )
            try:
                yield
            except BaseException:
                self.dataset, self.document, self.pages, self.bindings, self.tasks = snapshot
                raise

    async def fetchrow(self, query, *args):
        if "FROM confluence_space_bindings" in query:
            return copy.deepcopy(self.bindings.get(args[0]))
        if "FROM confluence_connections" in query:
            return copy.deepcopy(self.connection)
        if "FROM confluence_sync_tasks" in query:
            if "WHERE task_id = $1" in query:
                return copy.deepcopy(self.tasks.get(args[0]))
            rows = [
                row
                for row in self.tasks.values()
                if row["binding_id"] == args[0] and row["owner_id"] == args[1]
            ]
            return copy.deepcopy(rows[-1]) if rows else None
        if query.lstrip().startswith("UPDATE confluence_sync_tasks"):
            task = self.tasks.get(args[0])
            if (
                task
                and task["binding_id"] == args[2]
                and task["status"] in {"pending", "processing"}
            ):
                task.update(status="failed", completed_at=args[1])
                return {"owner_id": task["owner_id"]}
            return None
        return await super().fetchrow(query, *args)

    async def fetchval(self, query, *args):
        if "confluence_sync_tasks" in query:
            return any(
                t["binding_id"] == args[0]
                and (len(args) == 1 or t["task_id"] != args[1])
                and t["status"] in {"pending", "processing"}
                for t in self.tasks.values()
            )
        return await super().fetchval(query, *args)

    async def execute(self, query, *args):
        if "INSERT INTO confluence_sync_tasks" in query:
            if args[0] in self.tasks:
                raise RuntimeError("duplicate task")
            self.tasks[args[0]] = {
                "task_id": args[0],
                "binding_id": args[1],
                "owner_id": args[2],
                "status": "pending",
                "progress": 0,
                "total_items": 0,
                "processed_items": 0,
            }
            return "INSERT 1"
        if "UPDATE confluence_space_bindings" in query:
            self.bindings[args[0]]["status"] = "error" if "status = 'error'" in query else "syncing"
            return "UPDATE 1"
        return await super().execute(query, *args)

    async def get_confluence_binding(self, key, *, connection=None):
        assert connection in (None, self)
        return copy.deepcopy(self.bindings.get(key))

    async def get_confluence_connection(self, key, *, connection=None):
        assert key == "connection" and connection in (None, self)
        return copy.deepcopy(self.connection)

    async def get_confluence_sync_task(self, key, *, connection=None):
        assert connection in (None, self)
        return copy.deepcopy(self.tasks.get(key))

    async def update_confluence_binding(self, key, updates, **_kwargs):
        self.bindings[key].update(updates)

    async def update_confluence_sync_task(self, key, updates, **_kwargs):
        self.tasks[key].update(updates)

    async def list_confluence_pages(self, key, **_kwargs):
        return [copy.deepcopy(p) for p in self.pages.values() if p["binding_id"] == key]

    async def get_confluence_page_by_page_id(self, binding_id, page_id):
        return next(
            (
                copy.deepcopy(p)
                for p in self.pages.values()
                if p["binding_id"] == binding_id and p["page_id"] == page_id
            ),
            None,
        )

    async def list_confluence_bindings(self, **_kwargs):
        return list(map(copy.deepcopy, self.bindings.values()))


def make_workflow(monkeypatch):
    db = ManualDatabase()
    user = UserContext("owner", "tenant")
    state = {"editor": True, "active": True}

    async def actor(**kwargs):
        if not state["active"]:
            from fastapi import HTTPException

            raise HTTPException(403, "inactive")
        return user

    async def access(actor, dataset_id, required="viewer"):
        if required == "editor" and not state["editor"]:
            raise PermissionDeniedError("revoked")
        return copy.deepcopy(db.dataset)

    knowledge = SimpleNamespace(
        require_dataset_access=AsyncMock(side_effect=access),
        vector_store=SimpleNamespace(delete_document_points=AsyncMock()),
        image_storage_service=SimpleNamespace(delete_document_assets=AsyncMock()),
    )
    documents = DocumentService(SimpleNamespace(), db)
    documents._ks = knowledge

    async def detach(actor, binding, page):
        return await documents.delete_document(
            actor,
            binding["dataset_id"],
            str(page.get("document_id") or ""),
            _confluence_source=(binding, page),
        )

    knowledge.detach_confluence_source = detach
    svc = ConfluenceManualSync(
        settings=SimpleNamespace(
            confluence=SimpleNamespace(
                client_cache_ttl_seconds=300, request_timeout=1, max_retries=0
            )
        ),
        database=db,
        knowledge_service=knowledge,
        knowledge_worker=SimpleNamespace(),
        image_storage_service=knowledge.image_storage_service,
        actor_resolver=actor,
    )
    monkeypatch.setattr(ConfluenceClient, "get_page_children", AsyncMock(return_value=[]))
    monkeypatch.setattr(
        ConfluenceClient, "get_page", AsyncMock(return_value=SimpleNamespace(content_hash="same"))
    )
    app = FastAPI()
    app.include_router(router)
    app.state.confluence_sync_service = svc
    app.dependency_overrides[get_user_context] = lambda: user
    return app, svc, db, user, state


async def settle(svc):
    tasks = list(svc._tasks.values())
    if tasks:
        await asyncio.gather(*tasks)


@pytest.mark.asyncio
async def test_http_manual_sync_shared_detach_and_exact_readback(monkeypatch):
    app, svc, db, user, state = make_workflow(monkeypatch)
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://fixture"
    ) as client:
        accepted = await client.post(PATH, json={"request_id": TASK})
        assert accepted.status_code == 202 and accepted.json()["task_id"] == TASK
        await settle(svc)
        result = await client.get(PATH + "/" + TASK)
        assert result.status_code == 200 and result.json()["status"] == "completed"
        assert "first" not in db.pages and "second" in db.pages and db.document is not None
        # Explicit duplicate ID is only readback, never another provider run.
        calls = ConfluenceClient.get_page.await_count
        duplicate = await client.post(PATH, json={"request_id": TASK})
        assert (
            duplicate.json()["status"] == "completed"
            and ConfluenceClient.get_page.await_count == calls
        )
    await svc.close()


@pytest.mark.asyncio
async def test_failed_provider_read_then_explicit_new_request_recovers(monkeypatch):
    app, svc, db, user, state = make_workflow(monkeypatch)
    ConfluenceClient.get_page_children.side_effect = RuntimeError("sensitive-provider-fixture")
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://fixture"
    ) as client:
        assert (await client.post(PATH, json={"request_id": TASK})).status_code == 202
        await settle(svc)
        failed = (await client.get(PATH + "/" + TASK)).json()
        assert failed["status"] == "failed" and "sensitive-provider-fixture" not in json.dumps(
            failed
        )
        assert db.bindings["first"]["last_sync_at"] == "previous-success" and "first" in db.pages
        ConfluenceClient.get_page_children.side_effect = None
        retry = TASK[:-1] + "2"
        assert (await client.post(PATH, json={"request_id": retry})).status_code == 202
        await settle(svc)
        assert (await client.get(PATH + "/" + retry)).json()["status"] == "completed"
    await svc.close()


@pytest.mark.asyncio
async def test_definitive_admission_rejection_has_no_task(monkeypatch):
    app, svc, db, user, state = make_workflow(monkeypatch)
    state["editor"] = False
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://fixture"
    ) as client:
        response = await client.post(PATH, json={"request_id": TASK})
        assert response.status_code == 403 and response.json()["detail"]["accepted"] is False
        assert response.json()["detail"]["code"] == "CONFLUENCE_SYNC_NOT_ACCEPTED"
        assert not db.tasks
    await svc.close()


@pytest.mark.asyncio
async def test_revocation_after_acceptance_prevents_provider_and_allows_viewer_failure_read(
    monkeypatch,
):
    app, svc, db, user, state = make_workflow(monkeypatch)
    await svc.start(user, "dataset", "first", TASK)
    state["editor"] = False
    await settle(svc)
    ConfluenceClient.get_page.assert_not_awaited()
    assert db.tasks[TASK]["status"] == "failed"
    db.connection["status"] = "error"
    result = await svc.status(user, "dataset", "first", TASK)
    assert result["status"] == "failed"
    await svc.close()


@pytest.mark.asyncio
async def test_shutdown_before_first_task_dispatch_settles_admitted_task(monkeypatch):
    app, svc, db, user, state = make_workflow(monkeypatch)
    await svc.start(user, "dataset", "first", TASK)
    await svc.close()
    assert db.tasks[TASK]["status"] == "failed" and db.bindings["first"]["status"] == "error"
    assert not svc._services
    ConfluenceClient.get_page.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("revoke_before_commit", [False, True])
async def test_authorized_source_publication_reuses_connection_and_checks_current_grants(
    monkeypatch, revoke_before_commit
):
    from fastapi import HTTPException
    from knowledge_service.services.knowledge.confluence.manual_sync import _AuthorizedSync
    from knowledge_service.services.knowledge.confluence.models import ConfluencePage
    from knowledge_service.services.knowledge.dataset_service import DatasetService

    app, owner, db, user, state = make_workflow(monkeypatch)
    db.tasks[TASK] = {
        "task_id": TASK,
        "binding_id": "first",
        "owner_id": "owner",
        "status": "processing",
    }
    db.document.update(status="completed", enabled=True, archived=False, content="")
    calls = []

    @asynccontextmanager
    async def source_lease(*args):
        calls.append("lease")
        yield db

    async def grants(query, *args):
        assert "dataset_permissions" in query
        calls.append("grants-on-held-connection")
        return (
            [{"subject_type": "user", "subject_id": "owner", "permission": "editor"}]
            if state["editor"]
            else []
        )

    async def begin(*args, **kwargs):
        assert kwargs["connection"] is db
        db.document["status"] = "syncing"
        calls.append("begin-source")
        return True

    async def prepare(*args, **kwargs):
        assert kwargs["connection"] is db
        db.document.update(status="waiting", content=kwargs["content"])
        calls.append("publish-source")
        return True

    async def abort(*args, **kwargs):
        assert kwargs["connection"] is db
        db.document["status"] = "completed"
        calls.append("abort-source")
        return True

    async def assets(*args):
        calls.append("assets")
        if revoke_before_commit:
            state["editor"] = False

    async def enqueue(*args):
        calls.append("enqueue")

    db.document_index_update_lease = source_lease
    db.fetch = grants
    db.begin_confluence_document_sync = begin
    db.prepare_confluence_document_update = prepare
    db.abort_confluence_document_sync = abort
    knowledge = owner.knowledge_service
    knowledge.dataset_service = DatasetService(SimpleNamespace(), db)
    service = _AuthorizedSync(
        binding=db.bindings["first"],
        connection=db.connection,
        actor=user,
        task_id=TASK,
        actor_resolver=owner.actor_resolver,
        settings=owner._kwargs["settings"],
        database=db,
        knowledge_service=knowledge,
        knowledge_worker=SimpleNamespace(enqueue_claimed=enqueue),
        image_storage_service=SimpleNamespace(delete_document_images=assets),
    )
    page = ConfluencePage(
        page_id="page",
        space_key="SPACE",
        title="Page",
        version=2,
        body_storage="<p>Updated source</p>",
    )

    async def publish():
        return await service._prepare_existing_page_update(
            binding=db.bindings["first"],
            document_id=db.document_id,
            page=page,
            sync_generation="fixture-generation",
            attachment_manifest=[],
            source_attachment_manifest=[],
        )

    if revoke_before_commit:
        with pytest.raises(HTTPException):
            await publish()
        assert "publish-source" not in calls and "enqueue" not in calls and "abort-source" in calls
        assert db.document["content"] == ""
    else:
        assert await publish() == 0
        assert (
            calls.index("begin-source")
            < calls.index("assets")
            < calls.index("publish-source")
            < calls.index("enqueue")
        )
        assert db.document["content"] == "Updated source"
    assert calls.count("grants-on-held-connection") >= 2
    assert service._authority_connection is None
    await service.close()
    await owner.close()


@pytest.mark.parametrize(
    "role,enabled,registered",
    [("api", True, True), ("all", True, True), ("worker", True, False), ("api", False, False)],
)
def test_real_startup_builder_is_passive_and_never_starts_scheduler(role, enabled, registered):
    from knowledge_service.services.knowledge.confluence.manual_sync import build_manual_sync

    class NoIO:
        def __getattr__(self, name):
            raise AssertionError("startup must not inspect bindings, provider, worker or storage")

    result = build_manual_sync(
        settings=SimpleNamespace(
            runtime_role=role, confluence=SimpleNamespace(manual_sync_enabled=enabled)
        ),
        database=NoIO(),
        knowledge_service=NoIO(),
        knowledge_worker=NoIO(),
        image_storage_service=NoIO(),
    )
    assert (result is not None) == registered
    if result is not None:
        assert result._tasks == {} and result._services == {}


@pytest.mark.asyncio
async def test_admission_checks_active_task_even_when_binding_status_is_completed(monkeypatch):
    from fastapi import HTTPException

    app, svc, db, user, state = make_workflow(monkeypatch)
    db.tasks["older"] = {
        "task_id": "older",
        "binding_id": "first",
        "owner_id": "owner",
        "status": "processing",
    }
    db.bindings["first"]["status"] = "completed"
    with pytest.raises(HTTPException) as failure:
        await svc.start(user, "dataset", "first", TASK)
    assert failure.value.status_code == 409 and TASK not in db.tasks
    await svc.close()


@pytest.mark.asyncio
async def test_old_task_cannot_overwrite_new_admission_or_terminal_task(monkeypatch):
    from knowledge_service.services.knowledge.confluence.manual_sync import _AuthorizedSync

    app, owner, db, user, state = make_workflow(monkeypatch)
    db.tasks[TASK] = {
        "task_id": TASK,
        "binding_id": "first",
        "owner_id": "owner",
        "status": "completed",
    }
    db.tasks["newer"] = {
        "task_id": "newer",
        "binding_id": "first",
        "owner_id": "owner",
        "status": "pending",
    }
    db.bindings["first"]["status"] = "syncing"
    service = _AuthorizedSync(
        binding=db.bindings["first"],
        connection=db.connection,
        actor=user,
        task_id=TASK,
        actor_resolver=owner.actor_resolver,
        **owner._kwargs,
    )
    await service.db.update_confluence_binding("first", {"status": "error"})
    await service.db.update_confluence_sync_task(TASK, {"status": "failed"})
    assert db.bindings["first"]["status"] == "syncing" and db.tasks[TASK]["status"] == "completed"
    await service.close()
    await owner.close()


@pytest.mark.asyncio
async def test_connection_change_during_discovery_prevents_source_mutation(monkeypatch):
    app, svc, db, user, state = make_workflow(monkeypatch)

    async def change_connection(*args, **kwargs):
        db.connection["updated_at"] = "connection-v2"
        return []

    ConfluenceClient.get_page_children.side_effect = change_connection
    await svc.start(user, "dataset", "first", TASK)
    await settle(svc)
    assert db.tasks[TASK]["status"] == "failed"
    assert "first" in db.pages and db.document is not None
    assert db.bindings["first"]["last_sync_at"] == "previous-success"
    ConfluenceClient.get_page.assert_not_awaited()
    await svc.close()
