"""Production binding routes -> owner -> coordinator -> source publication.

Provider calls, database transactions and queue transport are controlled. No
network listener, credential, real provider or actual PostgreSQL acceptance.
"""

import asyncio
import copy
import json
import uuid
from contextlib import asynccontextmanager
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from knowledge_service.services.knowledge.confluence.client import (
    ConfluenceAPIError,
    ConfluenceClient,
)
from knowledge_service.services.knowledge.confluence.models import (
    ConfluencePage,
    ConfluenceSpace,
)
from knowledge_service.services.knowledge.dataset_service import DatasetService

from tests.services.knowledge.test_confluence_manual_workflow import (
    TASK,
    make_workflow,
    settle,
)

BINDING = "93f0e57e-3828-4a21-9edb-ce8f01bfaa98"
BASE = "/knowledge/dataset/sources/confluence"
PAYLOAD = {
    "request_id": BINDING,
    "connection_id": "connection",
    "space_key": "SPACE",
    "space_id": "space",
    "root_page_id": "root",
}


def setup(monkeypatch):
    app, svc, db, user, state = make_workflow(monkeypatch)
    db.bindings = {}
    db.pages = {}
    db.document = None
    db.document_id = str(
        uuid.uuid5(uuid.NAMESPACE_URL, "ai-platform:confluence:dataset:connection:root")
    )
    db.dataset["created_by"] = "owner"
    db.connection["name"] = "Saved fixture"
    calls = []
    state.update(insert_failure=False, revoke_on_lock=False)
    original_fetchrow = db.fetchrow

    async def fetchrow(query, *args):
        if (
            "FROM confluence_connections" in query
            and "FOR UPDATE" in query
            and state["revoke_on_lock"]
        ):
            db.dataset["created_by"] = "different-owner"
        if "INSERT INTO confluence_space_bindings" in query:
            assert "ON CONFLICT DO NOTHING" in query and "DO UPDATE" not in query
            assert "'manual', false" in query and "false, false, false" in query
            calls.append("insert-binding")
            if state["insert_failure"]:
                raise RuntimeError("controlled database error")
            if args[0] in db.bindings or any(
                (r["connection_id"], r["space_key"], r["dataset_id"])
                == (args[1], args[4], args[3])
                for r in db.bindings.values()
            ):
                return None
            row = dict(
                zip(
                    [
                        "binding_id",
                        "connection_id",
                        "tenant_id",
                        "dataset_id",
                        "space_key",
                        "space_id",
                        "space_name",
                        "owner_id",
                        "root_page_id",
                        "root_page_title",
                    ],
                    args,
                    strict=True,
                )
            )
            row.update(
                created_by=args[7],
                status="pending",
                sync_mode="manual",
                sync_enabled=False,
                sync_images=False,
                include_comments=False,
                include_attachments=False,
                max_depth=10,
            )
            db.bindings[row["binding_id"]] = row
            return copy.deepcopy(row)
        return await original_fetchrow(query, *args)

    async def fetch(query, *args):
        if "dataset_permissions" in query:
            return []
        assert "SELECT connection_id, name" in query
        assert (
            "tenant_id = $1" in query
            and "COALESCE(NULLIF(owner_id, ''), created_by) = $2" in query
        )
        assert "LIMIT 51 OFFSET $3" in query and args[:2] == ("tenant", "owner")
        rows = state.get(
            "connections", [{"connection_id": "connection", "name": "Saved fixture"}]
        )
        return copy.deepcopy(rows[args[2] : args[2] + 51])

    @asynccontextmanager
    async def lease(*args):
        calls.append("source-lease")
        yield db

    async def insert_document(row, **kwargs):
        assert row["document_id"] == db.document_id
        db.document = copy.deepcopy(row)
        calls.append("reserve-document")

    async def begin(*args, **kwargs):
        assert kwargs["connection"] is db
        db.document["status"] = "syncing"
        return True

    async def publish(*args, **kwargs):
        assert kwargs["connection"] is db
        record = kwargs["page_record"]
        db.document.update(
            content=kwargs["content"],
            status="waiting",
            metadata=kwargs["source_metadata"],
        )
        db.pages["root"] = {
            **record,
            "id": "root-record",
            "document_id": db.document_id,
        }
        calls.append("publish-source")
        return True

    db.fetchrow, db.fetch = fetchrow, fetch
    db.document_index_update_lease = lease
    db.insert_document = insert_document
    db.begin_confluence_document_sync = begin
    db.prepare_confluence_document_update = publish
    db.abort_confluence_document_sync = AsyncMock(return_value=True)
    svc.knowledge_service.dataset_service = DatasetService(SimpleNamespace(), db)
    svc._kwargs["knowledge_worker"] = SimpleNamespace(
        enqueue_claimed=AsyncMock(side_effect=lambda *_args: calls.append("enqueue"))
    )
    svc._kwargs["image_storage_service"] = SimpleNamespace(
        delete_document_images=AsyncMock()
    )
    space = ConfluenceSpace(
        "space", "SPACE", "Engineering", "global", "current", homepage_id="root"
    )
    page = ConfluencePage(
        "root", "SPACE", "Homepage", 1, "<p>First source text</p>", space_id="space"
    )
    monkeypatch.setattr(
        ConfluenceClient, "get_space_by_key", AsyncMock(return_value=space)
    )
    monkeypatch.setattr(ConfluenceClient, "get_page", AsyncMock(return_value=page))
    client = httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app, raise_app_exceptions=False),
        base_url="http://fixture",
    )
    return client, svc, db, user, state, calls


@pytest.mark.asyncio
async def test_saved_connection_to_verified_binding_to_explicit_sync_publishes_source(
    monkeypatch,
):
    client, svc, db, user, state, calls = setup(monkeypatch)
    async with client:
        listed = await client.get(BASE + "/connections")
        assert listed.status_code == 200
        assert listed.json() == {
            "connections": [{"connection_id": "connection", "name": "Saved fixture"}],
            "next_offset": None,
        }
        space = await client.get(
            BASE + "/connections/connection/space", params={"space_key": "SPACE"}
        )
        assert space.status_code == 200 and space.json()["root_page_id"] == "root"
        created = await client.post(BASE + "/bindings", json=PAYLOAD)
        assert created.status_code == 201 and created.json()["binding_id"] == BINDING
        fixture = json.loads(
            (
                Path(__file__).resolve().parents[3]
                / "web/src/pages/knowledge/sources/sourceBindingContract.fixture.json"
            ).read_text()
        )
        assert created.json() == fixture["receipt"] and space.json() == fixture["space"]
        assert not db.tasks and not db.pages and not db.document and not svc._tasks
        assert calls == ["insert-binding"]
        assert db.bindings[BINDING]["sync_enabled"] is False
        status = await client.get(BASE + "/bindings/" + BINDING)
        assert status.json() == created.json()
        sync = BASE + f"/{BINDING}/sync"
        assert (await client.post(sync, json={"request_id": TASK})).status_code == 202
        await settle(svc)
        result = (await client.get(sync + "/" + TASK)).json()
        assert result["status"] == "completed" and result["synced_items"] == 1
        assert (
            db.document["content"] == "First source text"
            and db.document["status"] == "waiting"
        )
        assert db.pages["root"]["binding_id"] == BINDING
        assert calls == [
            "insert-binding",
            "reserve-document",
            "source-lease",
            "publish-source",
            "enqueue",
        ]
        # Repeating setup after synchronization neither resets status nor syncs again.
        assert (
            await client.post(BASE + "/bindings", json=PAYLOAD)
        ).json() == created.json()
        assert db.bindings[BINDING]["status"] == "completed" and len(db.tasks) == 1
        assert "api_token" not in json.dumps(
            [listed.json(), space.json(), created.json(), result]
        )
    await svc.close()


@pytest.mark.asyncio
async def test_binding_identity_and_natural_key_are_not_upserts(monkeypatch):
    client, svc, db, user, state, calls = setup(monkeypatch)
    async with client:
        responses = await asyncio.gather(
            *[client.post(BASE + "/bindings", json=PAYLOAD) for _ in range(2)]
        )
        assert [r.status_code for r in responses] == [201, 201]
        assert len(db.bindings) == 1 and not db.tasks
        for update in [
            {"space_id": "different"},
            {"root_page_id": "other"},
            {"request_id": str(uuid.uuid4())},
        ]:
            response = await client.post(BASE + "/bindings", json=PAYLOAD | update)
            assert (
                response.status_code == 409
                and response.json()["detail"]["accepted"] is False
            )
        assert len(db.bindings) == 1
    await svc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "change", ["owner", "tenant", "inactive", "editor", "actor", "revoke_on_lock"]
)
async def test_current_authority_prevents_creation(monkeypatch, change):
    client, svc, db, user, state, calls = setup(monkeypatch)
    if change == "owner":
        db.connection["owner_id"] = "other"
    if change == "tenant":
        db.connection["tenant_id"] = "other"
    if change == "inactive":
        db.connection["status"] = "inactive"
    if change == "editor":
        state["editor"] = False
    if change == "actor":
        state["active"] = False
    if change == "revoke_on_lock":
        state["revoke_on_lock"] = True
    async with client:
        response = await client.post(BASE + "/bindings", json=PAYLOAD)
        assert (
            response.status_code in {403, 404}
            and response.json()["detail"]["accepted"] is False
        )
        assert not db.bindings and not db.tasks
    await svc.close()


@pytest.mark.asyncio
async def test_changed_connection_after_provider_read_is_rejected(monkeypatch):
    client, svc, db, user, state, calls = setup(monkeypatch)
    page = ConfluenceClient.get_page.return_value

    async def changed(*args, **kwargs):
        db.connection["updated_at"] = "changed"
        return page

    ConfluenceClient.get_page.side_effect = changed
    async with client:
        response = await client.post(BASE + "/bindings", json=PAYLOAD)
        assert response.status_code == 409 and not db.bindings
    await svc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "problem", ["provider", "wrong_space", "wrong_root", "no_homepage"]
)
async def test_space_discovery_failures_do_not_create_or_expose_provider_errors(
    monkeypatch, problem
):
    client, svc, db, user, state, calls = setup(monkeypatch)
    if problem == "provider":
        ConfluenceClient.get_space_by_key.side_effect = ConfluenceAPIError(
            "secret-body"
        )
    if problem == "wrong_space":
        ConfluenceClient.get_space_by_key.return_value.space_key = "OTHER"
    if problem == "wrong_root":
        ConfluenceClient.get_page.return_value.space_id = "other-space"
    if problem == "no_homepage":
        ConfluenceClient.get_space_by_key.return_value.homepage_id = None
    async with client:
        response = await client.post(BASE + "/bindings", json=PAYLOAD)
        assert response.status_code in {422, 502} and "secret-body" not in response.text
        assert not db.bindings and not db.tasks
    await svc.close()


@pytest.mark.asyncio
async def test_connection_pages_are_explicit_and_credentials_absent(monkeypatch):
    client, svc, db, user, state, calls = setup(monkeypatch)
    state["connections"] = [
        {"connection_id": f"c{i:03}", "name": f"Connection {i}"} for i in range(101)
    ]
    async with client:
        pages = [
            (await client.get(BASE + "/connections", params={"offset": off})).json()
            for off in [0, 50, 100]
        ]
        assert [len(p["connections"]) for p in pages] == [50, 50, 1]
        assert [p["next_offset"] for p in pages] == [50, 100, None]
        assert len({c["connection_id"] for p in pages for c in p["connections"]}) == 101
    await svc.close()


@pytest.mark.asyncio
async def test_ambiguous_database_error_never_becomes_definitive_rejection(monkeypatch):
    client, svc, db, user, state, calls = setup(monkeypatch)
    state["insert_failure"] = True
    async with client:
        response = await client.post(BASE + "/bindings", json=PAYLOAD)
        assert (
            response.status_code == 500
            and "CONFLUENCE_BINDING_NOT_ACCEPTED" not in response.text
        )
        assert (await client.get(BASE + "/bindings/" + BINDING)).status_code == 404
    await svc.close()


@pytest.mark.asyncio
async def test_exact_receipt_read_survives_inactive_connection_but_not_owner_or_dataset_revocation(
    monkeypatch,
):
    client, svc, db, user, state, calls = setup(monkeypatch)
    async with client:
        assert (await client.post(BASE + "/bindings", json=PAYLOAD)).status_code == 201
        db.connection["status"] = "inactive"
        assert (await client.get(BASE + "/bindings/" + BINDING)).status_code == 200
        db.bindings[BINDING]["owner_id"] = "other"
        assert (await client.get(BASE + "/bindings/" + BINDING)).status_code == 404
    await svc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "update",
    [
        {"request_id": "invalid"},
        {"space_key": "../bad"},
        {"root_page_id": ""},
        {"api_token": "must-not-accept"},
    ],
)
async def test_route_rejects_invalid_or_credential_payload(monkeypatch, update):
    client, svc, db, user, state, calls = setup(monkeypatch)
    async with client:
        response = await client.post(BASE + "/bindings", json=PAYLOAD | update)
        assert response.status_code == 422 and not db.bindings
    await svc.close()
