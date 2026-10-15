"""Real reconnect handlers and encryption, synthetic persistence and provider only."""

import copy
from datetime import timedelta
from unittest.mock import AsyncMock

import pytest
from ai_gateway_core.comm import InMemoryIdempotencyStore
from fastapi import HTTPException
from knowledge_service.api.connection_idempotency import ConnectionIdempotencyMiddleware
from knowledge_service.services.knowledge.confluence.credentials import (
    MARKER,
    connection_credentials,
    seal_credentials,
)
from knowledge_service.services.knowledge.confluence.manual_sync import _AuthorizedSync

from tests.services.knowledge.test_confluence_connection_controls import KEY, command
from tests.services.knowledge.test_confluence_connection_controls import workflow as controls
from tests.services.knowledge.test_confluence_manual_workflow import TASK, settle
from tests.services.knowledge.test_confluence_source_binding_workflow import BASE, BINDING, PAYLOAD

URL = BASE + "/connections/connection/credentials"
EMAIL = "replacement@example.invalid"
TOKEN = "synthetic-replacement-only"


def workflow(monkeypatch):
    client, svc, db, user, state, calls = controls(monkeypatch)
    svc.source_connections.encryption_key = KEY
    svc.source_bindings.encryption_key = KEY
    svc._kwargs["settings"].gateway_encryption_key = KEY
    svc.source_connections._verify = AsyncMock()
    old = db.fetchrow

    async def fetchrow(query, *args):
        if "UPDATE confluence_connections SET email" in query:
            assert "transaction_timestamp() > updated_at" in query
            assert "status =" not in query
            assert args[1] == MARKER and EMAIL not in repr(args) and TOKEN not in repr(args)
            if state.get("clock_not_advanced"):
                return None
            db.connection.update(
                email=args[1],
                api_token=args[2],
                updated_at=db.connection["updated_at"] + timedelta(microseconds=1),
            )
            calls.append("replace-credentials")
            if state.get("lost_ack"):
                raise RuntimeError("synthetic lost acknowledgement")
            return copy.deepcopy(db.connection)
        return await old(query, *args)

    db.fetchrow = fetchrow
    return client, svc, db, user, state, calls


def payload(db):
    data = command(db)
    del data["status"]
    return data | {"email": EMAIL, "api_token": TOKEN}


@pytest.mark.parametrize("status", ["active", "disabled"])
@pytest.mark.parametrize("legacy", [True, False])
async def test_verified_atomic_replacement_preserves_status_and_explicit_sync(
    monkeypatch, status, legacy
):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    db.connection["status"] = status
    if not legacy:
        db.connection.update(
            email=MARKER,
            api_token=seal_credentials(
                db.connection, "old@example.invalid", "old-synthetic-token", KEY
            ),
        )
    before = copy.deepcopy(db.connection)
    async with client:
        request = payload(db)
        response = await client.post(URL, json=request)
        assert response.status_code == 200, response.text
        assert (
            response.json()["status"] == status
            and response.json()["revision"] != request["expected_revision"]
        )
        assert db.connection == before | {
            "email": MARKER,
            "api_token": db.connection["api_token"],
            "updated_at": db.connection["updated_at"],
        }
        assert connection_credentials(db.connection, KEY).api_token == TOKEN
        assert not db.tasks and calls == ["replace-credentials"]
        assert EMAIL not in response.text and TOKEN not in response.text
        assert (await client.post(URL, json=request)).status_code == 409
        assert svc.source_connections._verify.await_count == 1
        if status == "disabled":
            assert (
                await client.patch(
                    BASE + "/connections/connection/control", json=command(db, "active")
                )
            ).status_code == 200
        assert (await client.post(BASE + "/bindings", json=PAYLOAD)).status_code == 201
        result = await client.post(BASE + "/" + BINDING + "/sync", json={"request_id": TASK})
        assert result.status_code == 202, result.text
        await settle(svc)
        assert db.tasks[TASK]["status"] == "completed"
    await svc.close()


@pytest.mark.parametrize(
    "problem", ["owner", "tenant", "actor", "editor", "stale", "hostname", "key", "status"]
)
async def test_preflight_refuses_without_transmitting_or_writing(monkeypatch, problem):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    request = payload(db)
    if problem in {"owner", "tenant"}:
        db.connection[problem + "_id"] = "other"
    elif problem in {"actor", "editor"}:
        state["active" if problem == "actor" else "editor"] = False
    elif problem == "stale":
        request["expected_revision"] = "old"
    elif problem == "hostname":
        db.connection["domain"] = "localhost"
    elif problem == "key":
        svc.source_connections.encryption_key = ""
    else:
        db.connection["status"] = "error"
    before = copy.deepcopy(db.connection)
    async with client:
        response = await client.post(URL, json=request)
        assert response.status_code in {403, 404, 409, 422, 503}, response.text
    assert db.connection == before and not calls
    svc.source_connections._verify.assert_not_awaited()
    await svc.close()


@pytest.mark.parametrize(
    "problem", ["verify", "owner", "tenant", "actor", "editor", "revision", "key", "grant", "clock"]
)
async def test_post_verification_fresh_authority_cas_and_encryption_fail_closed(
    monkeypatch, problem
):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    secret_bytes = (db.connection["email"], db.connection["api_token"])

    async def verify(*args):
        if problem == "verify":
            raise HTTPException(502, "Verification failed")
        elif problem in {"owner", "tenant"}:
            db.connection[problem + "_id"] = "other"
        elif problem in {"actor", "editor"}:
            state["active" if problem == "actor" else "editor"] = False
        elif problem == "revision":
            db.connection["updated_at"] += timedelta(seconds=1)
        elif problem == "key":
            svc.source_connections.encryption_key = ""
        elif problem == "grant":
            state["revoke_at_lock"] = True
        elif problem == "clock":
            state["clock_not_advanced"] = True

    svc.source_connections._verify.side_effect = verify
    async with client:
        response = await client.post(URL, json=payload(db))
        assert response.status_code in {403, 404, 409, 502, 503}, response.text
    assert secret_bytes == (db.connection["email"], db.connection["api_token"]) and not calls
    await svc.close()


@pytest.mark.parametrize(
    "field,value",
    [
        ("email", "secret-invalid-email"),
        ("api_token", {"secret": "raw-body"}),
        ("extra-secret", "raw-extra"),
        ("expected_status", "secret-status"),
        ("expected_revision", {}),
    ],
)
async def test_raw_validation_never_echoes_any_input(monkeypatch, field, value, caplog):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    async with client:
        response = await client.post(URL, json=payload(db) | {field: value})
        assert response.status_code == 422
        assert response.json() == {"detail": "Invalid replacement credential fields"}
        assert EMAIL not in response.text + caplog.text and TOKEN not in response.text + caplog.text
    assert not calls
    svc.source_connections._verify.assert_not_awaited()
    await svc.close()


async def test_lost_acknowledgement_metadata_only_readback(monkeypatch):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    state["lost_ack"] = True
    async with client:
        request = payload(db)
        assert (await client.post(URL, json=request)).status_code == 500
        svc.source_connections.encryption_key = ""
        response = await client.get(BASE + "/connections/connection/control")
        assert (
            response.status_code == 200
            and response.json()["connection"]["revision"] != request["expected_revision"]
        )
        assert (
            "email" not in response.text
            and "api_token" not in response.text
            and TOKEN not in response.text
        )
        assert calls == ["replace-credentials"]
    await svc.close()


async def test_replacement_invalidates_old_task_authority(monkeypatch):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    db.bindings["first"] = {
        "binding_id": "first",
        "connection_id": "connection",
        "dataset_id": "dataset",
        "tenant_id": "tenant",
        "created_by": "owner",
    }
    binding = db.bindings["first"]
    authority = _AuthorizedSync(
        binding=binding,
        connection=db.connection,
        actor=user,
        task_id=TASK,
        actor_resolver=svc.actor_resolver,
        **svc._kwargs,
    )
    async with client:
        assert (await client.post(URL, json=payload(db))).status_code == 200
        with pytest.raises(HTTPException):
            await authority._require_sync_authority()
        async with authority._sync_authority_connection(db):
            with pytest.raises(HTTPException):
                await authority._require_sync_authority()
    await authority.close()
    await svc.close()


async def test_secret_mutation_bypasses_response_cache():
    calls = []

    async def app(scope, receive, send):
        calls.append(scope["method"])
        await send({"type": "http.response.start", "status": 200, "headers": []})
        await send({"type": "http.response.body", "body": b"{}"})

    middleware = ConnectionIdempotencyMiddleware(app, store=InMemoryIdempotencyStore())

    async def receive():
        return {"type": "http.request", "body": b"synthetic-secret", "more_body": False}

    async def send(message):
        pass

    scope = {
        "type": "http",
        "method": "POST",
        "path": URL,
        "headers": [(b"idempotency-key", b"same")],
        "query_string": b"",
    }
    await middleware(scope, receive, send)
    await middleware(scope, receive, send)
    assert calls == ["POST", "POST"]


async def test_reconnect_readiness_does_not_require_writable_index_and_probes_cipher(monkeypatch):
    from knowledge_service.services.knowledge.confluence import credentials

    client, svc, db, user, state, calls = workflow(monkeypatch)
    svc.source_bindings._writable = lambda _: (_ for _ in ()).throw(HTTPException(409, "blocked"))
    assert (await svc.source_connections.capability(user, "dataset", reconnect=True))["available"]
    assert not (await svc.source_connections.capability(user, "dataset"))["available"]
    monkeypatch.setattr(credentials, "encrypt_value", lambda plain, _key: plain)
    assert (await svc.source_connections.capability(user, "dataset", reconnect=True)) == {
        "available": False,
        "reason": "encryption_key_required",
    }
    assert await svc.source_connections.controls_available(user, "dataset")
    async with client:
        response = await client.post(URL, json=payload(db))
        assert response.status_code == 503 and not calls
    svc.source_connections._verify.assert_not_awaited()
    await svc.close()


@pytest.mark.parametrize("status", [200, 401, 302, 500])
async def test_actual_reconnect_provider_transport_redacts_failures(monkeypatch, status, caplog):
    import httpx
    from knowledge_service.services.knowledge.confluence.source_connections import (
        ConfluenceSourceConnections,
    )

    client, svc, db, user, state, calls = workflow(monkeypatch)
    svc.source_connections._verify = ConfluenceSourceConnections._verify.__get__(
        svc.source_connections
    )
    original = httpx.AsyncClient
    requests = []
    before = copy.deepcopy(db.connection)

    async def transport(request):
        requests.append(request)
        assert str(request.url) == "https://fixture.atlassian.net/wiki/rest/api/user/current"
        return httpx.Response(
            status,
            json={"type": "known", "accountId": "fixture", "private": TOKEN},
            headers={"location": "https://untrusted.invalid"},
        )

    def provider(**kwargs):
        assert kwargs["follow_redirects"] is False and kwargs["trust_env"] is False
        return original(**kwargs, transport=httpx.MockTransport(transport))

    monkeypatch.setattr(httpx, "AsyncClient", provider)
    caplog.set_level("DEBUG")
    async with client:
        response = await client.post(URL, json=payload(db))
        assert response.status_code == (200 if status == 200 else 502)
        assert TOKEN not in response.text + caplog.text and EMAIL not in response.text + caplog.text
    assert len(requests) == 1
    if status != 200:
        assert db.connection == before and not calls
    await svc.close()


async def signed_reconnect(monkeypatch):
    import json
    from pathlib import Path
    from types import SimpleNamespace

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


async def test_real_native_signed_replacement_post_and_replay(monkeypatch):
    from tests.services.knowledge.test_native_gateway_manual_sync_contract import send

    client, svc, db, state, calls, fixture = await signed_reconnect(monkeypatch)
    async with client:
        response = await send(client, fixture["native_requests"][5], monkeypatch)
        assert response.status_code == 200, response.text
        assert response.json()["status"] == "active"
        assert connection_credentials(db.connection, KEY).api_token == TOKEN
        assert calls == ["replace-credentials"] and not db.tasks
        assert TOKEN not in response.text and EMAIL not in response.text
        assert (await send(client, fixture["native_requests"][5], monkeypatch)).status_code == 401
    await svc.close()


@pytest.mark.parametrize("tamper", ["body", "path", "method", "tenant", "roles", "query"])
async def test_real_native_signed_replacement_tamper_prevents_verification(monkeypatch, tamper):
    from tests.services.knowledge.test_native_gateway_manual_sync_contract import send

    client, svc, db, state, calls, fixture = await signed_reconnect(monkeypatch)
    request = copy.deepcopy(fixture["native_requests"][5])
    if tamper == "body":
        request["body"] = request["body"].replace(TOKEN, "different-synthetic")
    elif tamper == "path":
        request["path"] = request["path"].replace("/connection/", "/other/")
    elif tamper == "method":
        request["method"] = "PUT"
    elif tamper == "query":
        request["query"] = "offset=50"
    else:
        request["headers"]["x-tenant-id" if tamper == "tenant" else "x-user-roles"] = "other"
    async with client:
        response = await send(client, request, monkeypatch)
        assert response.status_code == 401
        assert TOKEN not in response.text and EMAIL not in response.text
    assert not calls
    svc.source_connections._verify.assert_not_awaited()
    await svc.close()
