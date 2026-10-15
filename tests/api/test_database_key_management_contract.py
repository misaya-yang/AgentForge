"""Real router/service over a strict synthetic SQL boundary; no DB or credentials."""
from copy import deepcopy
from unittest.mock import Mock

import pytest
from fastapi import FastAPI
from httpx import ASGITransport, AsyncClient

from src.api.deps import get_user_context
from src.api.v1 import api_keys
from src.core.auth.user_resolver import UserContext


class SyntheticDatabase:
    def __init__(self):
        self.rows = [
            {"id": 7, "key_id": None, "name": "duplicate", "user_id": "other-owner", "tenant_id": "tenant-A"},
            {"id": 8, "key_id": "custom-public-id", "name": "duplicate", "user_id": "actor", "tenant_id": "tenant-A"},
            {"id": 9, "key_id": None, "name": "duplicate", "user_id": "foreign-owner", "tenant_id": "tenant-B"},
        ]
        for row in self.rows:
            row.update(is_active=True, created_at="2026-10-13T00:00:00Z", scopes=["future:scope"],
                       roles=["user"], tier="normal", use_count=0)
        self.queries = []

    def select(self, query, params):
        self.queries.append((query, params))
        assert "COALESCE(key_id, ('ak_legacy_' || id::text))" in query
        assert "COALESCE(NULLIF(tenant_id, ''), 'default')" in query
        # Enforce the exact bound predicate shape emitted by the canonical service.
        tenant = params[-1]
        assert f"COALESCE(NULLIF(tenant_id, ''), 'default') = ${len(params)}" in query
        rows = [r for r in self.rows if (r["tenant_id"] or "default") == tenant]
        if "user_id = $" in query:
            index = 1 if query.lstrip().startswith("UPDATE") else 0
            assert f"user_id = ${index + 1}" in query
            rows = [r for r in rows if r["user_id"] == params[index]]
        if query.lstrip().startswith("UPDATE"):
            assert "SET enabled = FALSE, updated_at = NOW()" in query
            assert "WHERE COALESCE(key_id, ('ak_legacy_' || id::text)) = $1" in query
            rows = [r for r in rows if self.public_id(r) == params[0]]
        return rows

    @staticmethod
    def public_id(row):
        return row["key_id"] if row["key_id"] is not None else f"ak_legacy_{row['id']}"

    async def fetchrow(self, query, *params):
        if "information_schema.columns" in query:
            return {"column_name": "permissions"}
        rows = self.select(query, params)
        assert query.lstrip().startswith("UPDATE")
        if not rows:
            return None
        assert len(rows) == 1
        rows[0]["is_active"] = False
        return {"key_id": self.public_id(rows[0])}

    async def fetch(self, query, *params):
        assert "permissions AS scopes" in query
        rows = self.select(query, params)
        return [{**deepcopy(row), "key_id": self.public_id(row)} for row in rows]


def fixture(monkeypatch, *, roles=None, authenticated=True):
    database = SyntheticDatabase()
    app = FastAPI()
    app.include_router(api_keys.router, prefix="/api/v1")
    app.dependency_overrides[api_keys.get_database] = lambda: database
    app.dependency_overrides[get_user_context] = lambda: UserContext(
        user_id="actor", tenant_id="tenant-A", roles=roles or ["user"],
        is_authenticated=authenticated,
    )
    clear = Mock()
    monkeypatch.setattr(api_keys, "clear_service_access_constraint_cache", clear)
    return database, clear, AsyncClient(transport=ASGITransport(app=app), base_url="http://synthetic")


async def test_admin_legacy_database_list_revoke_and_readback(monkeypatch):
    database, clear, client = fixture(monkeypatch, roles=["admin"])
    async with client:
        before = await client.get("/api/v1/api-keys", params={"include_inactive": True})
        assert before.status_code == 200
        assert [r["key_id"] for r in before.json()] == ["ak_legacy_7", "custom-public-id"]
        assert before.json()[0]["scopes"] == ["future:scope"]
        # Use the actual returned public ID, never synthesize it from config IDs.
        target = before.json()[0]["key_id"]
        response = await client.post(f"/api/v1/api-keys/{target}/revoke")
        assert response.status_code == 200 and response.json()["key_id"] == target
        after = await client.get("/api/v1/api-keys", params={"include_inactive": True})
        assert after.json()[0]["is_active"] is False
        assert after.json()[1]["is_active"] is True
        assert database.rows[2]["is_active"] is True
        clear.assert_called_once_with()
        assert "key_hash" not in before.text and '"api_key"' not in before.text


@pytest.mark.parametrize("target", ["ak_legacy_9", "ak_legacy_8", "unknown-id"])
async def test_admin_cannot_revoke_other_tenant_or_guess_nonnull_public_id(monkeypatch, target):
    database, clear, client = fixture(monkeypatch, roles=["admin"])
    async with client:
        response = await client.post(f"/api/v1/api-keys/{target}/revoke")
        assert response.status_code == 404
        assert all(row["is_active"] for row in database.rows)
        clear.assert_not_called()


@pytest.mark.parametrize("roles", [["user"], ["admin:*"], ["platform_admin"]])
async def test_only_literal_admin_bypasses_owner_filter(monkeypatch, roles):
    database, clear, client = fixture(monkeypatch, roles=roles)
    async with client:
        response = await client.get("/api/v1/api-keys", params={"include_inactive": True})
        assert [r["key_id"] for r in response.json()] == ["custom-public-id"]
        denied = await client.post("/api/v1/api-keys/ak_legacy_7/revoke")
        assert denied.status_code == 404
        assert all(row["is_active"] for row in database.rows)
        clear.assert_not_called()


async def test_unauthenticated_user_cannot_list_or_revoke(monkeypatch):
    database, clear, client = fixture(monkeypatch, authenticated=False)
    async with client:
        assert (await client.get("/api/v1/api-keys")).status_code == 401
        assert (await client.post("/api/v1/api-keys/ak_legacy_7/revoke")).status_code == 401
        assert database.queries == []
        clear.assert_not_called()


async def test_owner_revokes_nonnull_public_id_without_touching_legacy_neighbor(monkeypatch):
    database, clear, client = fixture(monkeypatch)
    async with client:
        rows = (await client.get("/api/v1/api-keys", params={"include_inactive": True})).json()
        target = rows[0]["key_id"]
        assert target == "custom-public-id"
        response = await client.post(f"/api/v1/api-keys/{target}/revoke")
        assert response.status_code == 200
        after = (await client.get("/api/v1/api-keys", params={"include_inactive": True})).json()
        assert after[0]["key_id"] == target and after[0]["is_active"] is False
        assert database.rows[0]["is_active"] is True
        assert database.rows[2]["is_active"] is True
        clear.assert_called_once_with()
