"""Actual routes, credential envelope and provider client with controlled I/O."""

import asyncio
import copy
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_gateway_core.comm import InMemoryIdempotencyStore
from knowledge_service.api.connection_idempotency import ConnectionIdempotencyMiddleware
from knowledge_service.services.knowledge.confluence.credentials import (
    MARKER,
    CredentialUnavailable,
    connection_credentials,
    seal_credentials,
)

from tests.services.knowledge.test_confluence_manual_workflow import TASK, settle
from tests.services.knowledge.test_confluence_source_binding_workflow import (
    BASE,
    BINDING,
    PAYLOAD,
    setup,
)

ID = "6f54b77d-96af-4a3a-a15e-bac5fb09c21c"
EMAIL = "synthetic-secret-email@example.invalid"
TOKEN = "synthetic-secret-token-only"
KEY = "synthetic-test-key-not-production"
BODY = {
    "request_id": ID,
    "name": "Cloud",
    "domain": "fixture.atlassian.net",
    "email": EMAIL,
    "api_token": TOKEN,
}


def workflow(monkeypatch):
    client, svc, db, user, state, calls = setup(monkeypatch)
    svc.source_connections.encryption_key = KEY
    svc.source_bindings.encryption_key = KEY
    svc._kwargs["settings"].gateway_encryption_key = KEY
    rows = {}
    state["rows"] = rows
    old_fetchrow = db.fetchrow

    async def get_connection(connection_id, **kwargs):
        return copy.deepcopy(rows.get(connection_id))

    async def fetchrow(query, *args):
        if "INSERT INTO confluence_connections" in query:
            assert "ON CONFLICT DO NOTHING" in query and "DO UPDATE" not in query
            assert EMAIL not in repr(args) and TOKEN not in repr(args)
            if args[0] in rows:
                return None
            row = dict(
                zip(
                    [
                        "connection_id",
                        "tenant_id",
                        "name",
                        "domain",
                        "email",
                        "api_token",
                        "owner_id",
                    ],
                    args,
                    strict=True,
                )
            )
            row.update(created_by=row["owner_id"], status="active", sync_mode="manual")
            rows[args[0]] = row
            db.connection = row
            calls.append("insert-connection")
            if state.get("uncertain"):
                raise RuntimeError("controlled lost commit acknowledgement")
            return copy.deepcopy(row)
        if "FROM confluence_connections" in query:
            if "FOR UPDATE" in query:
                return await old_fetchrow(query, *args)
            row = rows.get(args[0])
            if row and "SELECT connection_id," in query:
                assert "api_token" not in query and "email" not in query
                return {
                    k: row[k]
                    for k in (
                        "connection_id",
                        "tenant_id",
                        "owner_id",
                        "created_by",
                        "name",
                        "domain",
                        "status",
                        "sync_mode",
                    )
                }
            return copy.deepcopy(row)
        if "SELECT dataset_id FROM datasets" in query:
            if state.get("revoke_on_connection_lock"):
                db.dataset["created_by"] = "someone-else"
            return {"dataset_id": "dataset"}
        return await old_fetchrow(query, *args)

    db.get_confluence_connection = get_connection
    db.fetchrow = fetchrow
    svc.source_connections._verify = AsyncMock()
    return client, svc, db, user, state, calls


@pytest.mark.asyncio
async def test_create_verify_readback_bind_sync_and_idempotent_intent(monkeypatch, caplog):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    async with client:
        created = await client.post(BASE + "/connections", json=BODY)
        assert created.status_code == 201, created.text
        assert created.json() == {
            "connection_id": ID,
            "name": "Cloud",
            "domain": BODY["domain"],
            "status": "active",
            "sync_mode": "manual",
        }
        import json
        from pathlib import Path

        fixture = json.loads(
            (
                Path(__file__).resolve().parents[3]
                / "web/src/pages/knowledge/sources/sourceConnectionContract.fixture.json"
            ).read_text()
        )
        assert created.json() == fixture["receipt"]
        assert {k: BODY[k] for k in ("request_id", "name", "domain")} == fixture["request"]
        assert not db.tasks and not db.bindings and calls == ["insert-connection"]
        stored = state["rows"][ID]
        assert stored["email"] == MARKER and stored["api_token"].startswith("cfcred:v1:enc:")
        assert EMAIL not in repr(stored) and TOKEN not in repr(stored)
        assert connection_credentials(stored, KEY).api_token == TOKEN
        assert TOKEN not in repr(connection_credentials(stored, KEY))
        assert (await client.get(BASE + "/connections/" + ID)).json() == created.json()
        assert (await client.post(BASE + "/connections", json=BODY)).json() == created.json()
        svc.source_connections._verify.assert_awaited_once()
        # Connections are owner-global, reusable across authorized datasets.
        assert (
            await client.post(BASE.replace("dataset", "second-dataset") + "/connections", json=BODY)
        ).json() == created.json()
        for field, value in [
            ("name", "Changed"),
            ("domain", "different.atlassian.net"),
            ("email", "different@example.invalid"),
            ("api_token", "different-token"),
        ]:
            assert (
                await client.post(BASE + "/connections", json=BODY | {field: value})
            ).status_code == 409
        binding = PAYLOAD | {"connection_id": ID}
        assert (
            await client.get(BASE + f"/connections/{ID}/space", params={"space_key": "SPACE"})
        ).status_code == 200
        assert (await client.post(BASE + "/bindings", json=binding)).status_code == 201
        db.document_id = str(
            __import__("uuid").uuid5(
                __import__("uuid").NAMESPACE_URL, f"ai-platform:confluence:dataset:{ID}:root"
            )
        )
        sync = BASE + f"/{BINDING}/sync"
        assert (await client.post(sync, json={"request_id": TASK})).status_code == 202
        await settle(svc)
        assert (await client.get(sync + "/" + TASK)).json()["status"] == "completed"
        assert "enqueue" in calls
        svc.source_connections.encryption_key = ""
        assert (await client.get(BASE + "/connections/" + ID)).json() == created.json()
        state["rows"][ID]["owner_id"] = "different"
        assert (await client.get(BASE + "/connections/" + ID)).status_code == 404
    assert EMAIL not in caplog.text and TOKEN not in caplog.text
    await svc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "update",
    [
        {"request_id": "bad"},
        {"extra": TOKEN},
        {TOKEN: EMAIL},
        {"email": [EMAIL]},
        {"email": "bad:auth@example.invalid"},
        {"email": "bad"},
        {"api_token": "bad\n" + TOKEN},
        {"api_token": {"token": TOKEN}},
        {"domain": "evil.invalid"},
        {"domain": "x.atlassian.net@evil.invalid"},
        {"domain": "x.atlassian.net/other"},
        {"domain": "x.atlassian.net:443"},
        {"domain": "x.y.atlassian.net"},
    ],
)
async def test_validation_never_echoes_credentials(monkeypatch, update, caplog):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    async with client:
        response = await client.post(BASE + "/connections", json=BODY | update)
        assert response.status_code == 422
        assert EMAIL not in response.text and TOKEN not in response.text
        assert not state["rows"] and not calls
        svc.source_connections._verify.assert_not_awaited()
    assert EMAIL not in caplog.text and TOKEN not in caplog.text
    await svc.close()


@pytest.mark.asyncio
async def test_missing_key_revocation_and_unknown_admission(monkeypatch):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    async with client:
        svc.source_connections.encryption_key = ""
        assert await svc.source_connections.capability(user, "dataset") == {
            "available": False,
            "reason": "encryption_key_required",
        }
        response = await client.post(BASE + "/connections", json=BODY)
        assert response.status_code == 503 and "GATEWAY_ENCRYPTION_KEY" in response.text
        svc.source_connections._verify.assert_not_awaited()
        svc.source_connections.encryption_key = KEY
        state["editor"] = False
        assert (await client.post(BASE + "/connections", json=BODY)).status_code == 403
        state["editor"] = True
        state["revoke_on_connection_lock"] = True
        assert (await client.post(BASE + "/connections", json=BODY)).status_code == 403
        assert not state["rows"]
        state["revoke_on_connection_lock"] = False
        db.dataset["created_by"] = "owner"
        state["uncertain"] = True
        response = await client.post(BASE + "/connections", json=BODY)
        assert response.status_code == 500 and "NOT_ACCEPTED" not in response.text
        assert (await client.get(BASE + "/connections/" + ID)).status_code == 200
        state["active"] = False
        assert (await client.get(BASE + "/connections/" + ID)).status_code == 403
    await svc.close()


@pytest.mark.asyncio
async def test_concurrent_duplicate_does_not_overwrite(monkeypatch):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    gate = asyncio.Event()
    entered = 0

    async def verify(*args):
        nonlocal entered
        entered += 1
        if entered == 2:
            gate.set()
        await gate.wait()

    svc.source_connections._verify = verify
    async with client:
        responses = await asyncio.gather(
            client.post(BASE + "/connections", json=BODY),
            client.post(BASE + "/connections", json=BODY | {"api_token": "other-token"}),
        )
        assert sorted(r.status_code for r in responses) == [201, 409]
        assert calls == ["insert-connection"]
    await svc.close()


def test_envelope_fail_closed_and_legacy_unchanged(monkeypatch):
    row = {
        "connection_id": ID,
        "tenant_id": "tenant",
        "owner_id": "owner",
        "domain": BODY["domain"],
        "email": MARKER,
    }
    row["api_token"] = seal_credentials(row, EMAIL, TOKEN, KEY)
    for changed, key in [
        ({}, ""),
        ({}, "wrong-key"),
        ({"domain": "other.atlassian.net"}, KEY),
        ({"tenant_id": "other"}, KEY),
        ({"owner_id": "other"}, KEY),
        ({"connection_id": "other"}, KEY),
        ({"api_token": "plain-corruption"}, KEY),
        ({"email": EMAIL}, KEY),
        ({"api_token": "cfcred:v2:enc:bad"}, KEY),
        ({"api_token": "enc:bad", "email": EMAIL}, KEY),
    ]:
        with pytest.raises(CredentialUnavailable):
            connection_credentials(row | changed, key)
    legacy = row | {"email": EMAIL, "api_token": TOKEN}
    before = copy.deepcopy(legacy)
    assert connection_credentials(legacy, "").api_token == TOKEN and legacy == before
    import knowledge_service.services.knowledge.confluence.credentials as module

    monkeypatch.setattr(module, "encrypt_value", lambda text, _key: text)
    with pytest.raises(CredentialUnavailable):
        seal_credentials(row, EMAIL, TOKEN, KEY)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "status,body",
    [
        (200, {"type": "known", "accountId": "synthetic-id"}),
        (200, {"type": "anonymous"}),
        (200, None),
        (200, {"type": "known", "accountId": ""}),
        (302, {"type": "known", "accountId": "synthetic-id"}),
        (401, {"secret": TOKEN}),
        (500, {"email": EMAIL}),
    ],
)
async def test_real_verifier_fixed_origin_no_redirect_or_secret_error(
    monkeypatch, status, body, caplog
):
    from knowledge_service.services.knowledge.confluence.source_connections import (
        ConfluenceSourceConnections,
    )

    caplog.set_level("DEBUG")
    original = httpx.AsyncClient
    calls = []

    async def transport(request):
        calls.append(request)
        assert str(request.url) == "https://fixture.atlassian.net/wiki/rest/api/user/current"
        assert request.method == "GET" and request.headers["authorization"].startswith("Basic ")
        return httpx.Response(status, json=body, headers={"location": "https://untrusted.invalid/"})

    def client(**kwargs):
        assert kwargs["follow_redirects"] is False and kwargs["trust_env"] is False
        return original(**kwargs, transport=httpx.MockTransport(transport))

    monkeypatch.setattr(httpx, "AsyncClient", client)
    owner = ConfluenceSourceConnections(bindings=SimpleNamespace(db=None), encryption_key=KEY)
    if status == 200 and isinstance(body, dict) and body.get("accountId"):
        await owner._verify(BODY["domain"], EMAIL, TOKEN)
    else:
        from fastapi import HTTPException

        with pytest.raises(HTTPException) as exc:
            await owner._verify(BODY["domain"], EMAIL, TOKEN)
        assert EMAIL not in str(exc.value) and TOKEN not in str(exc.value)
    assert len(calls) == 1 and EMAIL not in caplog.text and TOKEN not in caplog.text


@pytest.mark.asyncio
async def test_actual_idempotency_middleware_never_caches_connection_post(monkeypatch):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    # Same production replacement and order: cache wraps route; signature
    # ingress (when enabled) is outside this cache, never bypassed by it.
    store = InMemoryIdempotencyStore()
    store.get_cached = AsyncMock(side_effect=AssertionError("connection request entered cache"))
    client._transport.app.add_middleware(ConnectionIdempotencyMiddleware, store=store)
    async with client:
        assert (
            await client.post(
                BASE + "/connections", json=BODY, headers={"Idempotency-Key": "fixture"}
            )
        ).status_code == 201
        state["active"] = False
        assert (
            await client.post(
                BASE + "/connections", json=BODY, headers={"Idempotency-Key": "fixture"}
            )
        ).status_code == 403
    store.get_cached.assert_not_awaited()
    await svc.close()


@pytest.mark.asyncio
async def test_actor_revoked_during_provider_verify(monkeypatch):
    client, svc, db, user, state, calls = workflow(monkeypatch)

    async def verify(*args):
        state["active"] = False

    svc.source_connections._verify = verify
    async with client:
        response = await client.post(BASE + "/connections", json=BODY)
        assert response.status_code == 403
        assert response.json()["detail"]["accepted"] is False
        assert not state["rows"] and not calls
    await svc.close()


@pytest.mark.asyncio
async def test_metadata_cross_tenant_and_no_key_read(monkeypatch):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    async with client:
        saved = await client.post(BASE + "/connections", json=BODY)
        assert saved.status_code == 201
        row = state["rows"][ID]
        row["status"] = "disabled"
        svc.source_connections.encryption_key = ""
        result = await client.get(BASE + "/connections/" + ID)
        assert result.status_code == 200
        assert result.json()["status"] == "disabled"
        assert EMAIL not in result.text and TOKEN not in result.text
        row["tenant_id"] = "other-tenant"
        assert (await client.get(BASE + "/connections/" + ID)).status_code == 404
    await svc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "body",
    [
        b'{"email":"synthetic-secret-email@example.invalid",',
        b"null",
        b"[]",
        b'"synthetic-secret-token-only"',
    ],
)
async def test_malformed_json_secret_free(monkeypatch, body, caplog):
    client, svc, db, user, state, calls = workflow(monkeypatch)
    async with client:
        result = await client.post(
            BASE + "/connections", content=body, headers={"content-type": "application/json"}
        )
        assert result.status_code == 422
        assert EMAIL not in result.text and TOKEN not in result.text
        assert not state["rows"]
    assert EMAIL not in caplog.text and TOKEN not in caplog.text
    await svc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", [None, "unsigned", "body", "path", "replay"])
async def test_signed_ingress_on_new_connection_route(monkeypatch, failure):
    import json

    from ai_gateway_core.auth.gateway_secret import GatewaySecret
    from ai_gateway_core.auth.gateway_secret_middleware import GatewaySecretAuthMiddleware

    client, svc, db, user, state, calls = workflow(monkeypatch)
    secret = GatewaySecret(secret="synthetic-review-signing-key", audience="knowledge-service")
    store = InMemoryIdempotencyStore()
    store.get_cached = AsyncMock(side_effect=AssertionError("secret request entered cache"))
    client._transport.app.add_middleware(ConnectionIdempotencyMiddleware, store=store)
    client._transport.app.add_middleware(
        GatewaySecretAuthMiddleware, gateway_secret=secret, allow_anonymous=False
    )
    path = BASE + "/connections"
    body = json.dumps(BODY).encode()
    headers = {"Content-Type": "application/json", "Idempotency-Key": "synthetic-cache-key"}
    if failure != "unsigned":
        headers["X-Gateway-Secret"] = secret.sign(
            method="POST", path=path if failure != "path" else path + "/wrong", body=body
        )
    if failure == "body":
        body = body.replace(b"Cloud", b"Other")
    async with client:
        result = await client.post(path, content=body, headers=headers)
        if failure == "replay":
            assert result.status_code == 201
            result = await client.post(path, content=body, headers=headers)
        assert result.status_code == (201 if failure is None else 401)
        assert len(state["rows"]) == (1 if failure in (None, "replay") else 0)
        assert EMAIL not in result.text and TOKEN not in result.text
    await svc.close()


@pytest.mark.asyncio
async def test_encrypted_sync_client_and_legacy_dataclass_decode(monkeypatch):
    from knowledge_service.services.knowledge.confluence.models import ConfluenceConnection
    from knowledge_service.services.knowledge.confluence.sync_service import ConfluenceSyncService

    client, svc, db, user, state, calls = workflow(monkeypatch)
    async with client:
        assert (await client.post(BASE + "/connections", json=BODY)).status_code == 201
        sync = ConfluenceSyncService(**svc._kwargs)
        provider = await sync._get_client(ID)
        assert provider.credentials.email == EMAIL and provider.credentials.api_token == TOKEN
        await sync.close()
        row = state["rows"][ID]
        model = ConfluenceConnection(
            **{
                k: row[k]
                for k in (
                    "connection_id",
                    "tenant_id",
                    "name",
                    "domain",
                    "email",
                    "api_token",
                    "created_by",
                )
            }
        )
        assert model.to_credentials(KEY).api_token == TOKEN
        with pytest.raises(CredentialUnavailable):
            model.to_credentials()
        with pytest.raises(Exception, match="rotation is not supported"):
            await sync.update_connection(ID, "tenant", api_token="synthetic-change")
    await svc.close()
