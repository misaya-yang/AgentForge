"""Explicit saved-binding synchronization; no registration-time provider work.

One durable task identity is admitted under a binding row lock. Accepted tasks
are process-owned, not automatically replayed after a crash. A crashed pending
binding remains blocked for operator reconciliation rather than stealing work.
"""

from __future__ import annotations

import asyncio
import inspect
import json
from contextlib import asynccontextmanager
from datetime import datetime, timezone
from typing import Any

from fastapi import HTTPException

from ....services.gateway_actor import resolve_runtime_actor
from .source_bindings import ConfluenceSourceBindings
from .source_connections import ConfluenceSourceConnections
from .sync_service import ConfluenceSyncService

_BINDING_FIELDS = (
    "binding_id",
    "tenant_id",
    "dataset_id",
    "connection_id",
    "owner_id",
    "created_by",
    "root_page_id",
    "root_page_ids",
    "max_depth",
    "include_patterns",
    "exclude_patterns",
    "include_attachments",
    "include_comments",
    "sync_images",
    "image_max_size_bytes",
)
_CONNECTION_FIELDS = (
    "connection_id",
    "tenant_id",
    "owner_id",
    "created_by",
    "status",
    "updated_at",
)


def _owner(row: dict[str, Any]) -> str:
    return str(row.get("owner_id") or row.get("created_by") or "")


def _snapshot(row: dict[str, Any], fields: tuple[str, ...]) -> dict[str, Any]:
    # No credential values are retained in the execution scope or returned.
    import copy

    return {key: copy.deepcopy(row.get(key)) for key in fields}


class _GuardedIO:
    """Recheck authority before each provider or object-storage operation."""

    def __init__(self, target: Any, guard: Any):
        self._target, self._guard = target, guard

    def __getattr__(self, name: str):
        value = getattr(self._target, name)
        if not inspect.iscoroutinefunction(value):
            return value

        async def call(*args, **kwargs):
            await self._guard()
            return await value(*args, **kwargs)

        return call


class _BoundDatabase:
    """Keep late coordinator status writes on the admitted source/task identity."""

    def __init__(self, database, service):
        self.database, self.service = database, service

    def __getattr__(self, name):
        return getattr(self.database, name)

    async def update_confluence_binding(self, binding_id, updates):
        async with self.database._pool.acquire() as conn, conn.transaction():
            current = await conn.fetchrow(
                "SELECT * FROM confluence_space_bindings WHERE binding_id = $1 FOR UPDATE",
                binding_id,
            )
            own_task = await self.database.get_confluence_sync_task(
                self.service.task_id, connection=conn
            )
            other_active = await conn.fetchval(
                """SELECT EXISTS (SELECT 1 FROM confluence_sync_tasks
                   WHERE binding_id = $1 AND task_id <> $2 AND status IN ('pending', 'processing'))""",
                binding_id,
                self.service.task_id,
            )
            if (
                current
                and _snapshot(dict(current), _BINDING_FIELDS) == self.service.binding_scope
                and own_task
                and own_task.get("status") in {"pending", "processing"}
                and own_task.get("owner_id") == self.service.actor.user_id
                and not other_active
            ):
                await self.database.update_confluence_binding(binding_id, updates, connection=conn)

    async def update_confluence_sync_task(self, task_id, updates):
        async with self.database._pool.acquire() as conn, conn.transaction():
            current = await conn.fetchrow(
                "SELECT * FROM confluence_sync_tasks WHERE task_id = $1 FOR UPDATE", task_id
            )
            if (
                task_id == self.service.task_id
                and current
                and current.get("binding_id") == self.service.binding_scope["binding_id"]
                and current.get("owner_id") == self.service.actor.user_id
                and current.get("status") in {"pending", "processing"}
            ):
                await self.database.update_confluence_sync_task(task_id, updates, connection=conn)


class _AuthorizedSync(ConfluenceSyncService):
    def __init__(self, *, binding, connection, actor, task_id, actor_resolver, **kwargs):
        super().__init__(**kwargs)
        self.binding_scope = _snapshot(binding, _BINDING_FIELDS)
        self.connection_scope = _snapshot(connection, _CONNECTION_FIELDS)
        self.actor = actor
        self.task_id = task_id
        self.actor_resolver = actor_resolver
        self._authority_connection = None
        self._authority_lock = asyncio.Lock()
        self.db = _BoundDatabase(self.db, self)
        if self._image_storage_service is not None:
            self._image_storage_service = _GuardedIO(
                self._image_storage_service,
                self._require_sync_authority,
            )

    @asynccontextmanager
    async def _sync_authority_connection(self, connection):
        previous = self._authority_connection
        self._authority_connection = connection
        try:
            yield
        finally:
            self._authority_connection = previous

    async def _require_sync_authority(self, **scope):
        async with self._authority_lock:
            return await self._check_authority(scope)

    async def _check_authority(self, scope):
        expected = self.binding_scope
        for key in ("binding_id", "dataset_id", "connection_id"):
            if key in scope and scope[key] != expected[key]:
                raise HTTPException(403, "Confluence execution scope changed")
        conn = self._authority_connection
        current = await self.db.get_confluence_binding(expected["binding_id"], connection=conn)
        connection = await self.db.get_confluence_connection(
            expected["connection_id"], connection=conn
        )
        if (
            not current
            or _snapshot(current, _BINDING_FIELDS) != expected
            or not connection
            or _snapshot(connection, _CONNECTION_FIELDS) != self.connection_scope
        ):
            raise HTTPException(403, "Confluence execution authority changed")
        actor = await self.actor_resolver(
            tenant_id=self.actor.tenant_id, user_id=self.actor.user_id
        )
        if (
            not actor.is_authenticated
            or actor.user_id != self.actor.user_id
            or actor.tenant_id != self.actor.tenant_id
        ):
            raise HTTPException(403, "Confluence actor authority changed")
        if conn is None:
            dataset = await self.knowledge_service.require_dataset_access(
                actor, expected["dataset_id"], required="editor"
            )
        else:
            # Same-connection read + canonical permission policy avoids nested
            # pool acquisition while the document publication lease is held.
            dataset = await self.db.get_dataset(expected["dataset_id"], connection=conn)
            grants = await conn.fetch(
                "SELECT subject_type, subject_id, permission FROM dataset_permissions WHERE dataset_id = $1",
                expected["dataset_id"],
            )
            grant_map = {
                (
                    "role" if row["subject_type"] == "tenant_role" else row["subject_type"],
                    row["subject_id"],
                ): {"permission": row["permission"]}
                for row in grants
            }
            permission = await self.knowledge_service.dataset_service._effective_dataset_permission(
                dataset or {}, actor, grants=grant_map
            )
            if permission not in {"editor", "owner"}:
                raise HTTPException(403, "Confluence dataset permission changed")
        if not dataset or dataset.get("tenant_id") != actor.tenant_id:
            raise HTTPException(403, "Confluence dataset authority changed")
        task = await self.db.get_confluence_sync_task(self.task_id, connection=conn)
        if (
            not task
            or task.get("binding_id") != expected["binding_id"]
            or task.get("owner_id") != actor.user_id
            or task.get("status") not in {"pending", "processing"}
        ):
            raise HTTPException(409, "Confluence task ownership changed")
        document_id = scope.get("document_id")
        if document_id:
            document = await self.db.get_document(document_id, connection=conn)
            if document is not None and document.get("dataset_id") != expected["dataset_id"]:
                raise HTTPException(403, "Confluence document scope changed")
        return actor

    async def _get_client(self, connection_id):
        await self._require_sync_authority(connection_id=connection_id)
        return _GuardedIO(await super()._get_client(connection_id), self._require_sync_authority)


class ConfluenceManualSync:
    """Application lifecycle owner for explicitly submitted manual full syncs."""

    def __init__(
        self,
        *,
        settings,
        database,
        knowledge_service,
        knowledge_worker,
        image_storage_service=None,
        vlm_service=None,
        actor_resolver=resolve_runtime_actor,
    ):
        self.db = database
        self.knowledge_service = knowledge_service
        self.actor_resolver = actor_resolver
        self._kwargs = {
            "settings": settings,
            "database": database,
            "knowledge_service": knowledge_service,
            "knowledge_worker": knowledge_worker,
            "image_storage_service": image_storage_service,
            "vlm_service": vlm_service,
        }
        self.source_bindings = ConfluenceSourceBindings(
            database=database,
            knowledge_service=knowledge_service,
            actor_resolver=actor_resolver,
            encryption_key=getattr(settings, "gateway_encryption_key", ""),
        )
        self.source_connections = ConfluenceSourceConnections(
            bindings=self.source_bindings,
            encryption_key=getattr(settings, "gateway_encryption_key", ""),
        )
        self._tasks: dict[str, asyncio.Task] = {}
        self._services: dict[str, tuple[_AuthorizedSync, str]] = {}
        self._admissions: set[asyncio.Task] = set()
        self._closing = False

    async def _authorize(self, user, dataset_id, binding_id, *, mutation=True):
        if not user.is_authenticated or not user.user_id or not user.tenant_id:
            raise HTTPException(401, "Authentication required")
        actor = await self.actor_resolver(tenant_id=user.tenant_id, user_id=user.user_id)
        if (
            actor.user_id != user.user_id
            or actor.tenant_id != user.tenant_id
            or not actor.is_authenticated
        ):
            raise HTTPException(403, "Confluence actor unavailable")
        dataset = await self.knowledge_service.require_dataset_access(
            actor, dataset_id, required="editor" if mutation else "viewer"
        )
        binding = await self.db.get_confluence_binding(binding_id)
        if (
            not binding
            or binding.get("dataset_id") != dataset_id
            or binding.get("tenant_id") != actor.tenant_id
            or dataset.get("tenant_id") != actor.tenant_id
            or _owner(binding) != actor.user_id
            or binding.get("created_by") != actor.user_id
        ):
            raise HTTPException(404, "Confluence binding not available")
        if not mutation:
            return actor, binding, None
        connection = await self.db.get_confluence_connection(binding.get("connection_id"))
        if (
            not connection
            or connection.get("tenant_id") != actor.tenant_id
            or _owner(connection) != actor.user_id
            or connection.get("status") != "active"
        ):
            raise HTTPException(403, "Confluence connection not available to this owner")
        ConfluenceSyncService._validate_dataset_index_writable(dataset)
        return actor, binding, connection

    async def list_bindings(self, user, dataset_id):
        await self.knowledge_service.require_dataset_access(user, dataset_id, required="viewer")
        try:
            await self.knowledge_service.require_dataset_access(user, dataset_id, required="editor")
            editor = True
        except Exception:
            editor = False
        rows = await self.db.list_confluence_bindings(
            tenant_id=user.tenant_id, dataset_id=dataset_id
        )
        visible = []
        for row in rows:
            if (
                row.get("tenant_id") != user.tenant_id
                or row.get("dataset_id") != dataset_id
                or _owner(row) != user.user_id
            ):
                continue
            item = dict(row)
            connection = await self.db.get_confluence_connection(row.get("connection_id"))
            item["manual_sync_available"] = bool(
                editor
                and connection
                and connection.get("status") == "active"
                and connection.get("tenant_id") == user.tenant_id
                and _owner(connection) == user.user_id
                and row.get("created_by") == user.user_id
            )
            async with self.db._pool.acquire() as conn:
                latest = await conn.fetchrow(
                    """SELECT * FROM confluence_sync_tasks WHERE binding_id = $1 AND owner_id = $2
                       ORDER BY created_at DESC, task_id DESC LIMIT 1""",
                    row["binding_id"],
                    user.user_id,
                )
            item["current_sync_task"] = self._receipt(dict(latest)) if latest else None
            visible.append(item)
        return visible

    async def start(self, user, dataset_id, binding_id, task_id):
        admission = asyncio.current_task()
        self._admissions.add(admission)
        try:
            return await self._start(user, dataset_id, binding_id, task_id)
        finally:
            self._admissions.discard(admission)

    async def _start(self, user, dataset_id, binding_id, task_id):
        if self._closing:
            raise HTTPException(503, "Confluence manual sync is shutting down")
        actor, binding, connection = await self._authorize(user, dataset_id, binding_id)
        # Short transaction only. Provider/actor calls never hold this admission lock.
        async with self.db._pool.acquire() as conn, conn.transaction():
            row = await conn.fetchrow(
                "SELECT * FROM confluence_space_bindings WHERE binding_id = $1 FOR UPDATE",
                binding_id,
            )
            if not row or _snapshot(dict(row), _BINDING_FIELDS) != _snapshot(
                binding, _BINDING_FIELDS
            ):
                raise HTTPException(409, "Confluence binding changed before admission")
            previous = await conn.fetchrow(
                "SELECT * FROM confluence_sync_tasks WHERE task_id = $1", task_id
            )
            if previous:
                if previous["binding_id"] != binding_id or previous["owner_id"] != actor.user_id:
                    raise HTTPException(409, "Sync request identity already used")
                return self._receipt(dict(previous))
            active = await conn.fetchval(
                """SELECT EXISTS (SELECT 1 FROM confluence_sync_tasks
                   WHERE binding_id = $1 AND status IN ('pending', 'processing'))""",
                binding_id,
            )
            if row["status"] == "syncing" or active:
                raise HTTPException(
                    409, "Confluence sync is active or requires interruption reconciliation"
                )
            await conn.execute(
                """INSERT INTO confluence_sync_tasks
                   (task_id, binding_id, task_type, priority, status, retry_count,
                    max_retries, progress, total_items, processed_items, owner_id)
                   VALUES ($1, $2, 'full_sync', 1, 'pending', 0, 0, 0, 0, 0, $3)""",
                task_id,
                binding_id,
                actor.user_id,
            )
            await conn.execute(
                "UPDATE confluence_space_bindings SET status = 'syncing', updated_at = NOW() WHERE binding_id = $1",
                binding_id,
            )
        # No await between admission completion, close check and task registration.
        if self._closing:
            await self._interrupted(binding_id, task_id, scope=_snapshot(binding, _BINDING_FIELDS))
            raise HTTPException(503, "Confluence sync interrupted during shutdown")
        service = _AuthorizedSync(
            binding=binding,
            connection=connection,
            actor=actor,
            task_id=task_id,
            actor_resolver=self.actor_resolver,
            **self._kwargs,
        )
        self._services[task_id] = (service, binding_id)
        task = asyncio.create_task(
            self._run(service, binding_id, task_id), name=f"manual-confluence-{task_id}"
        )
        self._tasks[task_id] = task

        def finished(done):
            # Retrieve exceptions even if a terminal database write failed.
            if not done.cancelled():
                done.exception()
            self._tasks.pop(task_id, None)

        task.add_done_callback(finished)
        return self._receipt({"task_id": task_id, "binding_id": binding_id, "status": "pending"})

    async def _interrupted(self, binding_id, task_id, *, scope=None):
        now = datetime.now(timezone.utc).replace(tzinfo=None)
        owned = self._services.get(task_id)
        expected = scope or (owned[0].binding_scope if owned else None)
        # Settle only our still-active task, and only its unchanged binding.
        # Failure recording itself does not require newly revoked editor access.
        async with self.db._pool.acquire() as conn, conn.transaction():
            changed = await conn.fetchrow(
                """UPDATE confluence_sync_tasks SET status = 'failed',
                   error = 'Manual sync interrupted; inspect source status before retrying',
                   completed_at = $2, updated_at = NOW()
                   WHERE task_id = $1 AND binding_id = $3 AND status IN ('pending', 'processing')
                   RETURNING owner_id""",
                task_id,
                now,
                binding_id,
            )
            if changed and expected is not None:
                binding = await conn.fetchrow(
                    "SELECT * FROM confluence_space_bindings WHERE binding_id = $1 FOR UPDATE",
                    binding_id,
                )
                other_active = await conn.fetchval(
                    """SELECT EXISTS (SELECT 1 FROM confluence_sync_tasks
                       WHERE binding_id = $1 AND task_id <> $2 AND status IN ('pending', 'processing'))""",
                    binding_id,
                    task_id,
                )
                if (
                    binding
                    and _snapshot(dict(binding), _BINDING_FIELDS) == expected
                    and binding["status"] == "syncing"
                    and not other_active
                ):
                    await conn.execute(
                        """UPDATE confluence_space_bindings SET status = 'error',
                           last_error = 'Manual sync interrupted; inspect source status before retrying', updated_at = NOW()
                           WHERE binding_id = $1""",
                        binding_id,
                    )

    async def _run(self, service, binding_id, task_id):
        try:
            await service._require_sync_authority()
            await service._sync_space_pages(binding_id, task_id)
        except asyncio.CancelledError:
            await self._interrupted(binding_id, task_id)
        except Exception:
            await self._interrupted(binding_id, task_id)
        finally:
            await service.close()
            self._services.pop(task_id, None)

    @staticmethod
    def _receipt(row):
        result = row.get("result") or {}
        if isinstance(result, str):
            try:
                result = json.loads(result)
            except ValueError:
                result = {}
        if not isinstance(result, dict):
            result = {}
        receipt = {
            key: row.get(key)
            for key in (
                "task_id",
                "binding_id",
                "status",
                "progress",
                "total_items",
                "processed_items",
                "started_at",
                "completed_at",
            )
        }
        receipt.update(
            synced_items=result.get("synced_pages"),
            failed_items=result.get("failed_pages"),
            error_count=len(result["errors"]) if isinstance(result.get("errors"), list) else None,
            error_summary="Sync failed or was interrupted. Review source status before retrying."
            if row.get("status") == "failed"
            else None,
        )
        return receipt

    async def status(self, user, dataset_id, binding_id, task_id):
        await self._authorize(user, dataset_id, binding_id, mutation=False)
        row = await self.db.get_confluence_sync_task(task_id)
        if not row or row.get("binding_id") != binding_id or row.get("owner_id") != user.user_id:
            raise HTTPException(404, "Confluence sync task not available")
        return self._receipt(row)

    async def close(self):
        self._closing = True
        # Drain admissions before snapshotting tasks; a request may have been
        # awaiting current-actor lookup or its atomic admission transaction.
        admissions = [task for task in self._admissions if task is not asyncio.current_task()]
        if admissions:
            await asyncio.gather(*admissions, return_exceptions=True)
        tasks = list(self._tasks.values())
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        # A task cancelled before its first dispatch never enters _run/finally.
        for task_id, (service, binding_id) in list(self._services.items()):
            try:
                await self._interrupted(binding_id, task_id)
            finally:
                await service.close()
                self._services.pop(task_id, None)


def build_manual_sync(
    *, settings, database, knowledge_service, knowledge_worker, image_storage_service
):
    """Passive startup registration; neither reads bindings nor schedules work."""
    if settings.runtime_role == "worker" or not settings.confluence.manual_sync_enabled:
        return None
    return ConfluenceManualSync(
        settings=settings,
        database=database,
        knowledge_service=knowledge_service,
        knowledge_worker=knowledge_worker,
        image_storage_service=image_storage_service,
    )
