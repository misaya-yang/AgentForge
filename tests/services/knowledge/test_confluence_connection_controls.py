"""Real controls routes and authority checks with synthetic persistence/provider only."""

import copy
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException
from knowledge_service.services.knowledge.confluence.credentials import (
    MARKER,
    connection_credentials,
    seal_credentials,
)
from knowledge_service.services.knowledge.confluence.manual_sync import _AuthorizedSync

from tests.services.knowledge.test_confluence_manual_workflow import TASK
from tests.services.knowledge.test_confluence_source_binding_workflow import BASE, setup

URL = BASE + "/connections/connection/control"
KEY = "synthetic-controls-key"


def workflow(monkeypatch):
    client, svc, db, user, state, calls = setup(monkeypatch)
    db.connection.update(sync_mode="manual", updated_at=datetime(2026, 10, 12, tzinfo=timezone.utc))
    state["impact_rows"] = []
    old_row, old_fetch, old_value = db.fetchrow, db.fetch, db.fetchval

    async def fetchrow(query, *args):
        if "SELECT dataset_id FROM datasets" in query:
            if state.get("revoke_at_lock"):
                db.dataset["created_by"] = "other"
            return {"dataset_id": "dataset"}
        if "UPDATE confluence_connections" in query:
            assert "transaction_timestamp() > updated_at" in query
            assert "api_token" not in query and "email" not in query
            if state.get("clock_not_advanced"):
                return None
            db.connection.update(
                status=args[1], updated_at=db.connection["updated_at"] + timedelta(microseconds=1)
            )
            calls.append("control-write")
            if state.get("lost_ack"):
                raise RuntimeError("synthetic unknown commit")
            return copy.deepcopy(db.connection)
        if "FROM confluence_connections" in query and "SELECT connection_id," in query:
            assert "api_token" not in query and "email" not in query
            if state.get("change_at_lock") and "FOR UPDATE" in query:
                db.connection["owner_id"] = "other"
            return {k: v for k, v in db.connection.items() if k not in {"email", "api_token"}}
        return await old_row(query, *args)

    async def fetch(query, *args):
        if "FROM confluence_connections" in query:
            assert (
                "tenant_id = $1" in query
                and "COALESCE(NULLIF(owner_id, ''), created_by) = $2" in query
            )
            assert args[:2] == ("tenant", "owner")
            assert "api_token" not in query and "email" not in query
            rows = state.get("managed_rows", [db.connection])
            return copy.deepcopy(rows[args[2] : args[2] + 51])
        if "FROM confluence_space_bindings" in query:
            assert "connection_id = $1 AND tenant_id = $2" in query
            assert args[:2] == ("connection", "tenant")
            return copy.deepcopy(state["impact_rows"][args[2] : args[2] + 51])
        return await old_fetch(query, *args)

    async def fetchval(query, *args):
        if "COUNT(*) FROM confluence_space_bindings" in query:
            assert args == ("connection", "tenant")
            return len(state["impact_rows"])
        return await old_value(query, *args)

    db.fetchrow, db.fetch, db.fetchval = fetchrow, fetch, fetchval
    return client, svc, db, user, state, calls


def command(db, status="disabled"):
    return {
        "status": status,
        "expected_status": db.connection["status"],
        "expected_revision": db.connection["updated_at"].isoformat(),
    }


@pytest.mark.asyncio
@pytest.mark.parametrize("encrypted", [False, True])
async def test_pause_resume_exact_cas_preserves_credentials_and_never_calls_provider(
    monkeypatch, encrypted
):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    if encrypted:
        db.connection["api_token"] = seal_credentials(
            db.connection, "fixture@example.invalid", "synthetic-test-only", KEY
        )
        db.connection["email"] = MARKER
    before = copy.deepcopy(db.connection)
    svc.source_connections._verify = AsyncMock(side_effect=AssertionError("no provider"))
    async with client:
        listed = await client.get(BASE + "/connections/manage")
        assert (
            listed.status_code == 200
            and listed.json()["connections"][0]["revision"] == before["updated_at"].isoformat()
        )
        payload = command(db)
        paused = await client.patch(URL, json=payload)
        assert paused.status_code == 200 and paused.json()["status"] == "disabled"
        assert (await client.patch(URL, json=payload)).status_code == 409
        listed = await client.get(BASE + "/connections/manage")
        assert listed.json()["connections"][0]["status"] == "disabled"
        resumed = await client.patch(URL, json=command(db, "active"))
        assert (
            resumed.status_code == 200
            and resumed.json()["revision"] != before["updated_at"].isoformat()
        )
        assert calls == ["control-write", "control-write"] and not db.tasks
        assert db.connection == before | {
            "status": "active",
            "updated_at": db.connection["updated_at"],
        }
        assert connection_credentials(db.connection, KEY).api_token == "synthetic-test-only"
        assert "api_token" not in resumed.text and "email" not in resumed.text
    svc.source_connections._verify.assert_not_awaited()
    await svc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "change",
    [
        "actor",
        "editor",
        "tenant",
        "owner",
        "revoke_at_lock",
        "change_at_lock",
        "clock_not_advanced",
    ],
)
async def test_controls_require_current_exact_authority_and_advancing_revision(monkeypatch, change):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    payload = command(db)
    if change == "actor":
        state["active"] = False
    elif change == "editor":
        state["editor"] = False
    elif change == "tenant":
        db.connection["tenant_id"] = "other"
    elif change == "owner":
        db.connection["owner_id"] = "other"
    else:
        state[change] = True
    async with client:
        response = await client.patch(URL, json=payload)
        assert response.status_code in {403, 404, 409}, response.text
        assert not calls and db.connection["status"] == "active"
    await svc.close()


@pytest.mark.asyncio
async def test_index_block_and_key_loss_do_not_prevent_pause_or_metadata_read(monkeypatch):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    svc.source_bindings._writable = lambda _: (_ for _ in ()).throw(HTTPException(409, "blocked"))
    svc.source_connections.encryption_key = ""
    assert await svc.source_connections.controls_available(user, "dataset") is True
    assert await svc.source_bindings.available(user, "dataset") is False
    async with client:
        assert (await client.patch(URL, json=command(db))).status_code == 200
        assert (await client.get(URL)).json()["connection"]["status"] == "disabled"
    await svc.close()


@pytest.mark.asyncio
async def test_lost_write_ack_readback_returns_current_state_without_retry(monkeypatch):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    state["lost_ack"] = True
    async with client:
        response = await client.patch(URL, json=command(db))
        assert response.status_code == 500
        readback = await client.get(URL)
        assert readback.status_code == 200 and readback.json()["connection"]["status"] == "disabled"
        assert calls == ["control-write"]
    await svc.close()


@pytest.mark.asyncio
async def test_impact_hides_inaccessible_and_foreign_owner_binding_details(monkeypatch):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    state["impact_rows"] = [
        {
            "binding_id": "visible",
            "dataset_id": "dataset",
            "created_by": "owner",
            "space_name": "Visible",
            "status": "pending",
        },
        {
            "binding_id": "hidden-id",
            "dataset_id": "secret-dataset",
            "created_by": "owner",
            "space_name": "Hidden name",
            "status": "pending",
        },
        {
            "binding_id": "foreign-id",
            "dataset_id": "dataset",
            "created_by": "other",
            "space_name": "Foreign name",
            "status": "pending",
        },
    ]
    original = svc.knowledge_service.require_dataset_access

    async def access(actor, dataset_id, required="viewer"):
        if dataset_id == "secret-dataset":
            raise HTTPException(403)
        return await original(actor, dataset_id, required=required)

    svc.knowledge_service.require_dataset_access = access
    async with client:
        result = await client.get(URL)
        assert result.status_code == 200
        data = result.json()
        assert data["binding_count"] == 3 and data["hidden_count"] == 2
        assert [r["binding_id"] for r in data["bindings"]] == ["visible"]
        assert all(
            secret not in result.text
            for secret in (
                "secret-dataset",
                "hidden-id",
                "Hidden name",
                "foreign-id",
                "Foreign name",
                "synthetic-test-only",
            )
        )
        state["impact_rows"] = state["impact_rows"][:1] * 51
        first = (await client.get(URL)).json()
        last = (await client.get(URL, params={"offset": 50})).json()
        assert first["next_offset"] == 50 and len(first["bindings"]) == 50
        assert last["next_offset"] is None and len(last["bindings"]) == 1
    await svc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("resume", [False, True])
async def test_pause_and_fast_resume_invalidate_active_task_next_check(monkeypatch, resume):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    binding = {
        "binding_id": "binding",
        "dataset_id": "dataset",
        "connection_id": "connection",
        "tenant_id": "tenant",
        "created_by": "owner",
    }
    db.bindings["binding"] = binding
    db.tasks[TASK] = {
        "task_id": TASK,
        "binding_id": "binding",
        "owner_id": "owner",
        "status": "processing",
    }
    service = _AuthorizedSync(
        binding=binding,
        connection=db.connection,
        actor=user,
        task_id=TASK,
        actor_resolver=svc.actor_resolver,
        **svc._kwargs,
    )
    async with client:
        assert (await client.patch(URL, json=command(db))).status_code == 200
        if resume:
            assert (await client.patch(URL, json=command(db, "active"))).status_code == 200
        with pytest.raises(HTTPException):
            await service._require_sync_authority()
        async with service._sync_authority_connection(db):
            with pytest.raises(HTTPException):
                await service._require_sync_authority()
        assert (
            db.tasks[TASK]["status"] == "processing"
        )  # Controls do not pretend to cancel already admitted work.
        assert calls == ["control-write"] * (2 if resume else 1)
    await service.close()
    await svc.close()


@pytest.mark.asyncio
async def test_controls_bypass_generic_cached_mutation_responses(monkeypatch):
    from ai_gateway_core.comm import InMemoryIdempotencyStore
    from knowledge_service.api.connection_idempotency import ConnectionIdempotencyMiddleware

    client, svc, db, user, state, calls = workflow(monkeypatch)
    store = InMemoryIdempotencyStore()
    store.get_cached = AsyncMock(side_effect=AssertionError("controls entered generic cache"))
    client._transport.app.add_middleware(ConnectionIdempotencyMiddleware, store=store)
    payload = command(db)
    async with client:
        assert (
            await client.patch(URL, json=payload, headers={"Idempotency-Key": "fixture"})
        ).status_code == 200
        state["active"] = False
        assert (
            await client.patch(URL, json=payload, headers={"Idempotency-Key": "fixture"})
        ).status_code == 403
    store.get_cached.assert_not_awaited()
    assert calls == ["control-write"]
    await svc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("resume", [False, True])
async def test_publication_rechecks_real_pause_and_resume_after_assets(monkeypatch, resume):
    from contextlib import asynccontextmanager

    from fastapi import HTTPException
    from knowledge_service.services.knowledge.confluence.manual_sync import _AuthorizedSync
    from knowledge_service.services.knowledge.confluence.models import ConfluencePage
    from knowledge_service.services.knowledge.dataset_service import DatasetService

    from tests.services.knowledge.test_confluence_manual_workflow import make_workflow

    app, owner, db, user, state = make_workflow(monkeypatch)
    db.connection.update(
        name="Saved", sync_mode="manual", updated_at=datetime(2026, 10, 12, tzinfo=timezone.utc)
    )
    old_fetchrow = db.fetchrow

    async def control_row(query, *args):
        if "SELECT dataset_id FROM datasets" in query:
            return {"dataset_id": "dataset"}
        if "UPDATE confluence_connections" in query:
            assert "transaction_timestamp() > updated_at" in query
            db.connection.update(
                status=args[1], updated_at=db.connection["updated_at"] + timedelta(microseconds=1)
            )
            return copy.deepcopy(db.connection)
        return await old_fetchrow(query, *args)

    db.fetchrow = control_row
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
        await owner.source_connections.control(
            user, "dataset", "connection", SimpleNamespace(**command(db))
        )
        if resume:
            await owner.source_connections.control(
                user, "dataset", "connection", SimpleNamespace(**command(db, "active"))
            )

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

    with pytest.raises(HTTPException):
        await publish()
    assert "publish-source" not in calls and "enqueue" not in calls and "abort-source" in calls
    assert db.document["content"] == ""
    assert calls.count("grants-on-held-connection") >= 2
    assert service._authority_connection is None
    await service.close()
    await owner.close()


@pytest.mark.asyncio
async def test_paused_connection_rejects_new_sync_without_queue_or_provider(monkeypatch):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    db.bindings["binding"] = {
        "binding_id": "binding",
        "dataset_id": "dataset",
        "connection_id": "connection",
        "tenant_id": "tenant",
        "created_by": "owner",
    }
    async with client:
        assert (await client.patch(URL, json=command(db))).status_code == 200
        response = await client.post(BASE + "/binding/sync", json={"request_id": TASK})
        assert response.status_code == 403 and not db.tasks
        assert calls == ["control-write"]
    await svc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("change", ["tenant", "owner", "actor", "dataset"])
async def test_metadata_impact_has_exact_current_authority(monkeypatch, change):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    if change == "tenant":
        db.connection["tenant_id"] = "other"
    elif change == "owner":
        db.connection["owner_id"] = "other"
    elif change == "actor":
        state["active"] = False
    else:
        db.dataset["tenant_id"] = "other"
    async with client:
        response = await client.get(URL)
        assert response.status_code in {403, 404}
        assert "fixture.atlassian.net" not in response.text
    await svc.close()


@pytest.mark.asyncio
async def test_managed_pagination_and_invalid_or_unsupported_changes(monkeypatch):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    state["managed_rows"] = [db.connection] * 51
    async with client:
        first = (await client.get(BASE + "/connections/manage")).json()
        second = (await client.get(BASE + "/connections/manage", params={"offset": 50})).json()
        assert len(first["connections"]) == 50 and first["next_offset"] == 50
        assert len(second["connections"]) == 1 and second["next_offset"] is None
        assert (
            await client.patch(URL, json=command(db) | {"status": "deleted"})
        ).status_code == 422
        assert (
            await client.patch(URL, json=command(db) | {"api_token": "synthetic"})
        ).status_code == 422
        db.connection["status"] = "error"
        assert (await client.patch(URL, json=command(db))).status_code == 422
        assert not calls
    await svc.close()


async def signed_controls(monkeypatch):
    import json
    from pathlib import Path

    import httpx
    from ai_gateway_core.auth.gateway_secret import GatewaySecret, InMemoryReplayStore
    from fastapi import FastAPI
    from knowledge_service.api.routes.confluence_manual_sync import router
    from knowledge_service.auth import GatewaySecretAuthMiddleware

    unused, svc, db, user, state, calls = workflow(monkeypatch)
    await unused.aclose()
    fixture = json.loads(
        (Path(__file__).parents[2] / "fixtures/knowledge_hybrid_manual_sync.json").read_text()
    )
    app = FastAPI()
    app.include_router(router, prefix="/api/v1")
    app.state.confluence_sync_service = svc
    app.state.settings = SimpleNamespace(app=SimpleNamespace(allow_anonymous=False))
    app.add_middleware(
        GatewaySecretAuthMiddleware,
        gateway_secret=GatewaySecret(
            secret=fixture["secret"],
            version="v2",
            key_id="local",
            keys={"local": fixture["secret"]},
            replay_store=InMemoryReplayStore(),
            caller_service="gateway",
            audience="knowledge-service",
            allowed_path_prefixes=("/api/v1",),
        ),
        allow_anonymous=False,
    )
    client = httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="https://knowledge.example.invalid"
    )
    return client, svc, db, state, calls, fixture


@pytest.mark.asyncio
async def test_native_signed_control_patch_and_metadata_readback(monkeypatch):
    from tests.services.knowledge.test_native_gateway_manual_sync_contract import send

    client, svc, db, state, calls, fixture = await signed_controls(monkeypatch)
    async with client:
        response = await send(client, fixture["native_requests"][3], monkeypatch)
        assert response.status_code == 200, response.text
        assert response.json()["status"] == "disabled"
        readback = await send(client, fixture["native_requests"][4], monkeypatch)
        assert readback.status_code == 200, readback.text
        assert readback.json()["connection"]["status"] == "disabled"
        assert calls == ["control-write"] and not db.tasks
        assert (await send(client, fixture["native_requests"][3], monkeypatch)).status_code == 401
    await svc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("tamper", ["body", "path", "method", "tenant", "roles", "query"])
async def test_native_signed_control_tamper_denied_before_state_change(monkeypatch, tamper):
    from tests.services.knowledge.test_native_gateway_manual_sync_contract import send

    client, svc, db, state, calls, fixture = await signed_controls(monkeypatch)
    request = copy.deepcopy(fixture["native_requests"][3])
    if tamper == "body":
        request["body"] = request["body"].replace("disabled", "active")
    elif tamper == "path":
        request["path"] = request["path"].replace("/connection/", "/other/")
    elif tamper == "method":
        request["method"] = "PUT"
    elif tamper == "query":
        request["query"] = "offset=50"
    else:
        request["headers"]["x-tenant-id" if tamper == "tenant" else "x-user-roles"] = "other"
    async with client:
        assert (await send(client, request, monkeypatch)).status_code == 401
    assert not calls and db.connection["status"] == "active"
    await svc.close()
