"""Explicit binding of an existing owned connection; never saves credentials or syncs."""

from __future__ import annotations

from fastapi import HTTPException

from .client import ConfluenceAPIError, ConfluenceClient
from .credentials import CredentialUnavailable, connection_credentials
from .sync_service import ConfluenceSyncError, ConfluenceSyncService


def _owner(row):
    return row.get("owner_id") or row.get("created_by")


def _connection_scope(row):
    return tuple(
        row.get(k)
        for k in (
            "connection_id",
            "tenant_id",
            "owner_id",
            "created_by",
            "status",
            "updated_at",
        )
    )


class ConfluenceSourceBindings:
    """A bounded, manual-only source setup path over the existing canonical tables."""

    def __init__(self, *, database, knowledge_service, actor_resolver, encryption_key=""):
        self.encryption_key = encryption_key
        self.db = database
        self.knowledge_service = knowledge_service
        self.actor_resolver = actor_resolver

    async def _actor(self, user, dataset_id, *, editor=True, writable=True):
        if not user.is_authenticated or not user.user_id or not user.tenant_id:
            raise HTTPException(401, "Authentication required")
        actor = await self.actor_resolver(
            tenant_id=user.tenant_id, user_id=user.user_id
        )
        if (
            not actor.is_authenticated
            or actor.user_id != user.user_id
            or actor.tenant_id != user.tenant_id
        ):
            raise HTTPException(403, "Current source actor is unavailable")
        try:
            dataset = await self.knowledge_service.require_dataset_access(
                actor, dataset_id, required="editor" if editor else "viewer"
            )
        except Exception as exc:
            raise HTTPException(403, "Current dataset access required") from exc
        if dataset.get("tenant_id") != actor.tenant_id:
            raise HTTPException(404, "Dataset is unavailable")
        if editor and writable:
            self._writable(dataset)
        return actor

    @staticmethod
    def _writable(dataset):
        try:
            ConfluenceSyncService._validate_dataset_index_writable(dataset)
        except (ConfluenceSyncError, ValueError) as exc:
            raise HTTPException(409, "Dataset index is not writable") from exc

    async def available(self, user, dataset_id):
        try:
            await self._actor(user, dataset_id)
            return True
        except Exception:
            return False

    async def _connection(self, actor, connection_id, *, connection=None):
        row = await self.db.get_confluence_connection(
            connection_id, connection=connection
        )
        if (
            not row
            or row.get("tenant_id") != actor.tenant_id
            or _owner(row) != actor.user_id
            or row.get("status") != "active"
        ):
            raise HTTPException(404, "Saved connection is unavailable to this owner")
        return row

    async def connections(self, user, dataset_id, *, offset=0):
        actor = await self._actor(user, dataset_id)
        # Filter before pagination; never read or serialize stored credentials.
        async with self.db._pool.acquire() as conn:
            rows = await conn.fetch(
                """SELECT connection_id, name FROM confluence_connections
                   WHERE tenant_id = $1 AND COALESCE(NULLIF(owner_id, ''), created_by) = $2
                     AND status = 'active'
                   ORDER BY connection_id LIMIT 51 OFFSET $3""",
                actor.tenant_id,
                actor.user_id,
                offset,
            )
        return {
            "connections": [dict(row) for row in rows[:50]],
            "next_offset": offset + 50 if len(rows) > 50 else None,
        }

    async def space(self, user, dataset_id, connection_id, space_key):
        actor = await self._actor(user, dataset_id)
        connection = await self._connection(actor, connection_id)
        # Exact-key lookup avoids pretending that the dormant first-page-only
        # discover_spaces helper is a complete space catalogue.
        try:
            async with ConfluenceClient(
                connection_credentials(connection, self.encryption_key)
            ) as client:
                space = await client.get_space_by_key(space_key)
                if not space.homepage_id:
                    raise HTTPException(422, "Space has no homepage to use as a source root")
                root = await client.get_page(str(space.homepage_id), include_labels=False)
        except CredentialUnavailable:
            raise HTTPException(
                503, "Saved credentials cannot be decrypted; check GATEWAY_ENCRYPTION_KEY"
            ) from None
        except (ConfluenceAPIError, KeyError, ValueError) as exc:
            raise HTTPException(
                502,
                "Space could not be verified; check the saved connection and space key",
            ) from exc
        actor = await self._actor(user, dataset_id)
        current = await self._connection(actor, connection_id)
        if _connection_scope(current) != _connection_scope(connection):
            raise HTTPException(409, "Saved connection changed; verify the space again")
        if (
            space.space_key != space_key
            or not space.space_id
            or space.space_id == "None"
            or space.status != "current"
            or not space.name
            or str(root.space_id) != space.space_id
            or str(root.page_id) != str(space.homepage_id)
        ):
            raise HTTPException(502, "Provider did not return the exact current space")
        return {
            "connection_id": connection_id,
            "space_key": space.space_key,
            "space_id": space.space_id,
            "space_name": space.name,
            "root_page_id": str(root.page_id),
            "root_page_title": root.title,
        }

    @staticmethod
    def _receipt(row):
        return {
            k: row.get(k)
            for k in (
                "binding_id",
                "dataset_id",
                "connection_id",
                "space_key",
                "space_id",
                "space_name",
                "root_page_id",
                "root_page_title",
            )
        } | {"sync_mode": row.get("sync_mode", "manual")}

    async def status(self, user, dataset_id, binding_id):
        actor = await self._actor(user, dataset_id, editor=False)
        row = await self.db.get_confluence_binding(binding_id)
        if (
            not row
            or row.get("dataset_id") != dataset_id
            or row.get("tenant_id") != actor.tenant_id
            or _owner(row) != actor.user_id
            or row.get("created_by") != actor.user_id
        ):
            raise HTTPException(404, "Binding receipt is unavailable")
        return self._receipt(row)

    @staticmethod
    def _same_request(
        row, actor, dataset_id, connection_id, space_key, space_id, root_page_id
    ):
        return (
            all(
                row.get(key) == value
                for key, value in {
                    "dataset_id": dataset_id,
                    "connection_id": connection_id,
                    "space_key": space_key,
                    "space_id": space_id,
                    "root_page_id": root_page_id,
                    "tenant_id": actor.tenant_id,
                    "created_by": actor.user_id,
                }.items()
            )
            and _owner(row) == actor.user_id
        )

    async def create(
        self,
        user,
        dataset_id,
        binding_id,
        connection_id,
        space_key,
        space_id,
        root_page_id,
    ):
        actor = await self._actor(user, dataset_id)
        connection = await self._connection(actor, connection_id)
        prior = await self.db.get_confluence_binding(binding_id)
        if prior:
            if not self._same_request(
                prior,
                actor,
                dataset_id,
                connection_id,
                space_key,
                space_id,
                root_page_id,
            ):
                raise HTTPException(409, "Binding request identity already used")
            return self._receipt(prior)
        observed = await self.space(user, dataset_id, connection_id, space_key)
        if observed["space_id"] != space_id or observed["root_page_id"] != root_page_id:
            raise HTTPException(409, "Space identity changed; verify the space again")
        actor = await self._actor(user, dataset_id)
        async with self.db._pool.acquire() as conn, conn.transaction():
            locked = await conn.fetchrow(
                "SELECT * FROM confluence_connections WHERE connection_id = $1 FOR UPDATE",
                connection_id,
            )
            if not locked or _connection_scope(locked) != _connection_scope(connection):
                raise HTTPException(409, "Saved connection changed before binding")
            # Reuse the held connection and canonical policy; no nested acquisition.
            dataset = await self.db.get_dataset(dataset_id, connection=conn)
            grants = await conn.fetch(
                "SELECT subject_type, subject_id, permission FROM dataset_permissions WHERE dataset_id = $1",
                dataset_id,
            )
            grant_map = {
                (
                    "role"
                    if row["subject_type"] == "tenant_role"
                    else row["subject_type"],
                    row["subject_id"],
                ): {"permission": row["permission"]}
                for row in grants
            }
            permission = await self.knowledge_service.dataset_service._effective_dataset_permission(
                dataset or {}, actor, grants=grant_map
            )
            if (
                not dataset
                or dataset.get("tenant_id") != actor.tenant_id
                or permission not in {"editor", "owner"}
            ):
                raise HTTPException(403, "Current dataset editor access required")
            self._writable(dataset)
            # One INSERT, immutable caller-retained identity and existing natural
            # unique constraint. Never upsert/reset an existing binding or task.
            row = await conn.fetchrow(
                """INSERT INTO confluence_space_bindings
                   (binding_id, connection_id, tenant_id, dataset_id, space_key, space_id,
                    space_name, owner_id, created_by, status, sync_mode, sync_enabled,
                    sync_images, include_attachments, include_comments, root_page_id, root_page_title)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8, 'pending', 'manual', false,
                           false, false, false, $9, $10)
                   ON CONFLICT DO NOTHING RETURNING *""",
                binding_id,
                connection_id,
                actor.tenant_id,
                dataset_id,
                space_key,
                space_id,
                observed["space_name"],
                actor.user_id,
                root_page_id,
                observed["root_page_title"],
            )
            if not row:
                prior = await self.db.get_confluence_binding(
                    binding_id, connection=conn
                )
                if prior and self._same_request(
                    prior,
                    actor,
                    dataset_id,
                    connection_id,
                    space_key,
                    space_id,
                    root_page_id,
                ):
                    return self._receipt(prior)
                raise HTTPException(
                    409,
                    "Binding already exists or request identity is in use; refresh sources",
                )
        return self._receipt(row)
