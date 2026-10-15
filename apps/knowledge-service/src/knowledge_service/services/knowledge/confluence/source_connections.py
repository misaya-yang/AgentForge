"""Explicit Cloud connection verification and insert-only, metadata-safe receipts."""

from __future__ import annotations

import hmac
import re

import httpx
from fastapi import HTTPException

from .credentials import (
    MARKER,
    CredentialUnavailable,
    connection_credentials,
    require_key,
    seal_credentials,
)

_METADATA = "connection_id, tenant_id, owner_id, created_by, name, domain, status, sync_mode"


class ConfluenceSourceConnections:
    def __init__(self, *, bindings, encryption_key):
        self.bindings = bindings
        self.db = bindings.db
        self.encryption_key = encryption_key

    async def capability(self, user, dataset_id, *, reconnect=False):
        available = await self.controls_available(user, dataset_id) if reconnect else await self.bindings.available(user, dataset_id)
        if not available:
            return {"available": False, "reason": "current_editor_required"}
        try:
            seal_credentials(
                {"connection_id": "readiness", "tenant_id": "readiness", "owner_id": "readiness", "domain": "readiness.atlassian.net"},
                "readiness@example.invalid", "readiness-probe", self.encryption_key,
            )
        except CredentialUnavailable:
            return {"available": False, "reason": "encryption_key_required"}
        return {"available": True, "reason": None}

    @staticmethod
    def _owned(row, actor):
        return bool(
            row
            and row.get("tenant_id") == actor.tenant_id
            and (row.get("owner_id") or row.get("created_by")) == actor.user_id
        )

    @staticmethod
    def _receipt(row):
        return {k: row[k] for k in ("connection_id", "name", "domain", "status", "sync_mode")}

    async def status(self, user, dataset_id, connection_id):
        actor = await self.bindings._actor(user, dataset_id, editor=False)
        # Readback needs neither a key nor secret columns, even after key loss.
        async with self.db._pool.acquire() as conn:
            row = await conn.fetchrow(
                f"SELECT {_METADATA} FROM confluence_connections WHERE connection_id = $1",
                connection_id,
            )
        if not self._owned(row, actor):
            raise HTTPException(404, "Connection receipt is unavailable")
        return self._receipt(row)

    def _existing(self, row, actor, payload):
        if not self._owned(row, actor):
            raise HTTPException(409, "Connection request identity is already in use")
        if (
            row.get("name"),
            row.get("domain"),
            row.get("status"),
            row.get("sync_mode"),
            row.get("email"),
        ) != (payload.name, payload.domain, "active", "manual", MARKER):
            raise HTTPException(409, "Connection request identity is already in use")
        try:
            credentials = connection_credentials(row, self.encryption_key)
        except CredentialUnavailable:
            raise HTTPException(
                503, "Saved credentials cannot be decrypted; check GATEWAY_ENCRYPTION_KEY"
            ) from None
        if not (
            hmac.compare_digest(
                credentials.email.encode(), payload.email.get_secret_value().encode()
            )
            and hmac.compare_digest(
                credentials.api_token.encode(), payload.api_token.get_secret_value().encode()
            )
        ):
            raise HTTPException(409, "Connection request identity is already in use")
        return self._receipt(row)

    async def _verify(self, domain, email, token):
        # One fixed, authenticated identity read. Never follow redirects or use
        # environment proxies; never include provider bodies/errors in output.
        try:
            async with httpx.AsyncClient(
                timeout=15, follow_redirects=False, trust_env=False
            ) as client:
                response = await client.get(
                    f"https://{domain}/wiki/rest/api/user/current",
                    auth=httpx.BasicAuth(email, token),
                )
            if response.status_code != 200:
                raise ValueError()
            identity = response.json()
            if (
                identity.get("type") != "known"
                or not isinstance(identity.get("accountId"), str)
                or not identity["accountId"]
            ):
                raise ValueError()
        except Exception:
            raise HTTPException(
                502,
                "Confluence Cloud verification failed; check hostname, account email and API token",
            ) from None

    async def create(self, user, dataset_id, payload):
        actor = await self.bindings._actor(user, dataset_id)
        try:
            require_key(self.encryption_key)
        except CredentialUnavailable:
            raise HTTPException(
                503,
                "Configure GATEWAY_ENCRYPTION_KEY for Knowledge API and workers before saving connections",
            ) from None
        connection_id = str(payload.request_id)
        prior = await self.db.get_confluence_connection(connection_id)
        if prior:
            return self._existing(prior, actor, payload)
        row = {
            "connection_id": connection_id,
            "tenant_id": actor.tenant_id,
            "owner_id": actor.user_id,
            "created_by": actor.user_id,
            "name": payload.name,
            "domain": payload.domain,
            "email": MARKER,
            "sync_mode": "manual",
            "status": "active",
        }
        try:
            encrypted = seal_credentials(
                row,
                payload.email.get_secret_value(),
                payload.api_token.get_secret_value(),
                self.encryption_key,
            )
        except CredentialUnavailable:
            raise HTTPException(
                503, "Credential encryption is unavailable; check GATEWAY_ENCRYPTION_KEY"
            ) from None
        await self._verify(
            payload.domain, payload.email.get_secret_value(), payload.api_token.get_secret_value()
        )
        actor = await self.bindings._actor(user, dataset_id)
        async with self.db._pool.acquire() as conn, conn.transaction():
            # Match binding admission's canonical current grants on the held
            # connection. Lock the dataset for the short admission transaction.
            await conn.fetchrow(
                "SELECT dataset_id FROM datasets WHERE dataset_id = $1 FOR UPDATE", dataset_id
            )
            dataset = await self.db.get_dataset(dataset_id, connection=conn)
            grants = await conn.fetch(
                "SELECT subject_type, subject_id, permission FROM dataset_permissions WHERE dataset_id = $1",
                dataset_id,
            )
            grant_map = {
                (
                    "role" if r["subject_type"] == "tenant_role" else r["subject_type"],
                    r["subject_id"],
                ): {"permission": r["permission"]}
                for r in grants
            }
            permission = (
                await self.bindings.knowledge_service.dataset_service._effective_dataset_permission(
                    dataset or {}, actor, grants=grant_map
                )
            )
            if (
                not dataset
                or dataset.get("tenant_id") != actor.tenant_id
                or permission not in {"editor", "owner"}
            ):
                raise HTTPException(403, "Current dataset editor access required")
            self.bindings._writable(dataset)
            saved = await conn.fetchrow(
                f"""INSERT INTO confluence_connections
                (connection_id, tenant_id, name, domain, email, api_token, sync_mode, status, created_by, owner_id)
                VALUES ($1, $2, $3, $4, $5, $6, 'manual', 'active', $7, $7)
                ON CONFLICT DO NOTHING RETURNING {_METADATA}""",
                connection_id,
                actor.tenant_id,
                payload.name,
                payload.domain,
                MARKER,
                encrypted,
                actor.user_id,
            )
            if not saved:
                prior = await self.db.get_confluence_connection(connection_id, connection=conn)
                return self._existing(prior, actor, payload)
            receipt = self._receipt(saved)
        # Nothing that yields a definitive rejection may run after commit.
        return receipt


    @staticmethod
    def _control_receipt(row):
        revision = row["updated_at"].isoformat()
        return ConfluenceSourceConnections._receipt(row) | {"revision": revision}

    async def controls_available(self, user, dataset_id):
        try:
            await self.bindings._actor(user, dataset_id, writable=False)
            return True
        except Exception:
            return False

    async def managed(self, user, dataset_id, *, offset=0):
        actor = await self.bindings._actor(user, dataset_id, writable=False)
        async with self.db._pool.acquire() as conn:
            rows = await conn.fetch(
                f"""SELECT {_METADATA}, updated_at FROM confluence_connections
                WHERE tenant_id = $1 AND COALESCE(NULLIF(owner_id, ''), created_by) = $2
                ORDER BY connection_id LIMIT 51 OFFSET $3""",
                actor.tenant_id, actor.user_id, offset,
            )
        return {"connections": [self._control_receipt(row) for row in rows[:50]],
                "next_offset": offset + 50 if len(rows) > 50 else None}

    async def impact(self, user, dataset_id, connection_id, *, offset=0):
        actor = await self.bindings._actor(user, dataset_id, editor=False)
        async with self.db._pool.acquire() as conn:
            row = await conn.fetchrow(
                f"SELECT {_METADATA}, updated_at FROM confluence_connections WHERE connection_id = $1",
                connection_id,
            )
            if not self._owned(row, actor):
                raise HTTPException(404, "Connection controls are unavailable")
            total = await conn.fetchval(
                "SELECT COUNT(*) FROM confluence_space_bindings WHERE connection_id = $1 AND tenant_id = $2",
                connection_id, actor.tenant_id,
            )
            rows = await conn.fetch(
                """SELECT binding_id, dataset_id, owner_id, created_by, space_name, status
                FROM confluence_space_bindings WHERE connection_id = $1 AND tenant_id = $2
                ORDER BY binding_id LIMIT 51 OFFSET $3""",
                connection_id, actor.tenant_id, offset,
            )
        visible, hidden = [], 0
        for binding in rows[:50]:
            if (binding.get("owner_id") or binding.get("created_by")) != actor.user_id:
                hidden += 1
                continue
            try:
                dataset = await self.bindings.knowledge_service.require_dataset_access(
                    actor, binding["dataset_id"], required="viewer"
                )
                if dataset.get("tenant_id") != actor.tenant_id:
                    raise ValueError()
            except Exception:
                hidden += 1
                continue
            visible.append({k: binding.get(k) for k in
                            ("binding_id", "dataset_id", "space_name", "status")})
        return {"connection": self._control_receipt(row), "binding_count": total,
                "bindings": visible, "hidden_count": hidden,
                "next_offset": offset + 50 if len(rows) > 50 else None}

    async def reconnect(self, user, dataset_id, connection_id, payload):
        actor = await self.bindings._actor(user, dataset_id, writable=False)
        # Metadata-only preflight prevents credential transmission for a foreign,
        # stale or unsupported connection. Never decrypt the old credentials.
        async with self.db._pool.acquire() as conn:
            row = await conn.fetchrow(
                f"SELECT {_METADATA}, updated_at FROM confluence_connections WHERE connection_id = $1",
                connection_id,
            )
        if not self._owned(row, actor):
            raise HTTPException(404, "Connection controls are unavailable")
        if (row["updated_at"].isoformat() != payload.expected_revision
                or row["status"] != payload.expected_status):
            raise HTTPException(409, "Connection changed; read current impact before deciding again")
        if not re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.atlassian\.net", row["domain"]):
            raise HTTPException(422, "Replacement requires an Atlassian Cloud hostname")
        try:
            seal_credentials(row, payload.email.get_secret_value(),
                             payload.api_token.get_secret_value(), self.encryption_key)
        except CredentialUnavailable:
            raise HTTPException(503, "Credential encryption is unavailable; check GATEWAY_ENCRYPTION_KEY") from None
        await self._verify(row["domain"], payload.email.get_secret_value(), payload.api_token.get_secret_value())
        return await self.control(user, dataset_id, connection_id, payload, replacement=payload)

    async def control(self, user, dataset_id, connection_id, payload, *, replacement=None):
        # Pause is available even while the dataset's index is not writable.
        # The public pause/resume branch changes admission only; the private
        # replacement branch shares its locking and exact authority checks.
        actor = await self.bindings._actor(user, dataset_id, writable=False)
        async with self.db._pool.acquire() as conn, conn.transaction():
            # Re-resolve the current authenticated actor after provider verification.
            if replacement is not None:
                actor = await self.bindings._actor(user, dataset_id, writable=False)
            # Match binding admission: lock connection before dataset/FK work.
            row = await conn.fetchrow(
                f"SELECT {_METADATA}, updated_at FROM confluence_connections WHERE connection_id = $1 FOR UPDATE",
                connection_id,
            )
            if not self._owned(row, actor):
                raise HTTPException(404, "Connection controls are unavailable")
            await conn.fetchrow(
                "SELECT dataset_id FROM datasets WHERE dataset_id = $1 FOR UPDATE", dataset_id
            )
            dataset = await self.db.get_dataset(dataset_id, connection=conn)
            grants = await conn.fetch(
                "SELECT subject_type, subject_id, permission FROM dataset_permissions WHERE dataset_id = $1",
                dataset_id,
            )
            grant_map = {
                ("role" if r["subject_type"] == "tenant_role" else r["subject_type"], r["subject_id"]):
                {"permission": r["permission"]} for r in grants
            }
            permission = await self.bindings.knowledge_service.dataset_service._effective_dataset_permission(
                dataset or {}, actor, grants=grant_map
            )
            if not dataset or dataset.get("tenant_id") != actor.tenant_id or permission not in {"editor", "owner"}:
                raise HTTPException(403, "Current dataset editor access required")
            if (row["updated_at"].isoformat() != payload.expected_revision
                    or row["status"] != payload.expected_status):
                raise HTTPException(409, "Connection changed; read current impact before deciding again")
            if replacement is not None:
                try:
                    encrypted = seal_credentials(
                        row, replacement.email.get_secret_value(),
                        replacement.api_token.get_secret_value(), self.encryption_key,
                    )
                except CredentialUnavailable:
                    raise HTTPException(503, "Credential encryption is unavailable; check GATEWAY_ENCRYPTION_KEY") from None
                changed = await conn.fetchrow(
                    f"""UPDATE confluence_connections SET email = $2, api_token = $3, updated_at = NOW()
                    WHERE connection_id = $1 AND transaction_timestamp() > updated_at
                    RETURNING {_METADATA}, updated_at""",
                    connection_id, MARKER, encrypted,
                )
                if not changed:
                    raise HTTPException(409, "Connection revision cannot advance; read current status before retrying")
                receipt = self._control_receipt(changed)
            elif row["status"] == payload.status:
                return self._control_receipt(row)
            else:
                # updated_at is the existing execution fence. The table trigger also
                # updates it; credentials, bindings and tasks remain byte-for-byte untouched.
                changed = await conn.fetchrow(
                    f"""UPDATE confluence_connections SET status = $2, updated_at = NOW()
                    WHERE connection_id = $1 AND transaction_timestamp() > updated_at
                    RETURNING {_METADATA}, updated_at""",
                    connection_id, payload.status,
                )
                if not changed:
                    raise HTTPException(409, "Connection revision cannot advance; read current status before retrying")
                receipt = self._control_receipt(changed)
        # Unknown commit errors stay unknown. Never return a definitive rejection
        # for any work after commit, and never retry the mutation automatically.
        return receipt
