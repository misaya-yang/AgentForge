"""Bounded catalog grants preserve static decisions, without resource-read caching."""

from __future__ import annotations

from contextlib import asynccontextmanager
from datetime import datetime, timezone
from itertools import product
from types import SimpleNamespace
from unittest import IsolatedAsyncioTestCase

from knowledge_service.persistence.datasets import DatasetPersistenceMixin
from knowledge_service.services.knowledge.common import permission_rank as _permission_rank
from knowledge_service.services.knowledge.dataset_service import DatasetService


def actor(**changes):
    return SimpleNamespace(
        **{
            "user_id": "reader",
            "tenant_id": "t",
            "tier": "normal",
            "roles": ["analyst"],
            "is_authenticated": True,
            **changes,
        }
    )


def row(**changes):
    return {
        "dataset_id": "d",
        "tenant_id": "t",
        "created_by": "other",
        "visibility": "tenant",
        "created_at": datetime(2026, 1, 1, tzinfo=timezone.utc),
        **changes,
    }


class GrantStore(DatasetPersistenceMixin):
    def __init__(self, rows=(), grants=None):
        self.rows = list(rows)
        self.grants = grants or {}
        self.serial_calls = []
        self.batch_calls = []
        self.scans = 0
        self.stats_calls = 0
        self.fail = False
        self._pool = self

    @asynccontextmanager
    async def acquire(self):
        yield self

    async def fetch(self, sql, ids, user_id, role_ids, roles):
        self.batch_calls.append((sql, ids, user_id, role_ids, roles))
        if self.fail:
            raise RuntimeError("grant read failed")
        return [
            {
                "dataset_id": dataset_id,
                "subject_type": kind,
                "subject_id": subject,
                "permission": rec["permission"],
            }
            for (dataset_id, kind, subject), rec in self.grants.items()
            if dataset_id in ids
            and (
                (kind == "user" and subject == user_id)
                or (kind == "role" and dataset_id in role_ids and subject in roles)
            )
        ]

    async def get_dataset_permission(self, dataset_id, kind, subject):
        self.serial_calls.append((dataset_id, kind, subject))
        return self.grants.get((dataset_id, kind, subject))

    async def list_datasets(self, *, limit, before_dataset_id=None, **_kwargs):
        self.scans += 1
        return [
            r.copy()
            for r in self.rows
            if before_dataset_id is None or r["dataset_id"] < before_dataset_id
        ][:limit]

    async def get_datasets_statistics_batch(self, _ids):
        self.stats_calls += 1
        return {}


def service(store):
    result = object.__new__(DatasetService)
    result.db = store
    return result


async def _legacy_permission(
    self, dataset: dict, user: SimpleNamespace, *, allow_archived_owner: bool = False
) -> str | None:
    if dataset.get("is_archived") and (not allow_archived_owner):
        return None
    if not user.is_authenticated:
        return "viewer" if str(dataset.get("visibility") or "").lower() == "public" else None
    dataset_tenant_id = str(dataset.get("tenant_id") or "").strip()
    user_tenant_id = str(user.tenant_id or "").strip()
    same_tenant = bool(
        dataset_tenant_id and user_tenant_id and (dataset_tenant_id == user_tenant_id)
    )
    if same_tenant and (user.tier == "admin" or "admin" in (user.roles or [])):
        return "owner"
    created_by = str(dataset.get("created_by") or "")
    if created_by and created_by == user.user_id:
        return "owner"
    rec = await self.db.get_dataset_permission(dataset.get("dataset_id"), "user", user.user_id)
    best = str(rec.get("permission")) if rec else None
    for role in user.roles or [] if same_tenant else []:
        r = await self.db.get_dataset_permission(dataset.get("dataset_id"), "role", role)
        p = str(r.get("permission")) if r else None
        if _permission_rank(p) > _permission_rank(best):
            best = p
    if best:
        return best
    visibility = str(dataset.get("visibility") or "private").lower()
    if visibility == "public":
        return "viewer"
    if visibility == "tenant" and same_tenant:
        return "viewer"
    return None


class CatalogPermissionBatchTests(IsolatedAsyncioTestCase):
    async def test_static_decision_matrix_matches_original_and_resource_reads(self):
        # Includes service identities, anonymous actors, role order, duplicate
        # roles, tenant whitespace, unknown/empty/upper-case persisted grants.
        actors = [
            actor(),
            actor(roles=[]),
            actor(roles=["analyst", "analyst"]),
            actor(roles=["analyst", "other"]),
            actor(roles=["other", "analyst"]),
            actor(tier="admin"),
            actor(roles=["admin"]),
            actor(tenant_id="foreign"),
            actor(tenant_id=""),
            actor(tenant_id=" t "),
            actor(user_id="owner"),
            actor(is_authenticated=False),
            actor(user_id="service", roles=["service"]),
        ]
        values = [None, "", "unknown", "viewer", "VIEWER", "editor", "owner"]
        cases = 0
        for user, tenant, visibility, archived, allow, user_grant, role_grant in product(
            actors,
            ["t", "foreign", "", " t "],
            ["private", "tenant", "PUBLIC"],
            [False, True],
            [False, True],
            values,
            values,
        ):
            dataset = row(
                tenant_id=tenant, visibility=visibility, is_archived=archived, created_by="owner"
            )
            grants = {}
            if user_grant is not None:
                grants[("d", "user", user.user_id)] = {"permission": user_grant}
            if role_grant is not None:
                grants[("d", "role", "analyst")] = {"permission": role_grant}
                grants[("d", "role", "other")] = {"permission": "editor"}
            store = GrantStore(grants=grants)
            svc = service(store)
            expected = await _legacy_permission(svc, dataset, user, allow_archived_owner=allow)
            normal = await svc._effective_dataset_permission(
                dataset, user, allow_archived_owner=allow
            )
            snapshot = {
                (kind, subject): rec for (ds, kind, subject), rec in grants.items() if ds == "d"
            }
            before = len(store.serial_calls)
            batched = await svc._effective_dataset_permission(
                dataset, user, allow_archived_owner=allow, grants=snapshot
            )
            self.assertEqual((normal, batched), (expected, expected))
            self.assertEqual(len(store.serial_calls), before)
            cases += 1
        self.assertEqual(cases, 30576)

    async def test_200_rows_use_one_grant_statement_and_no_serial_grants(self):
        store = GrantStore([row(dataset_id=f"d-{i:03}") for i in range(200, 0, -1)])
        legacy = GrantStore(store.rows)
        await service(legacy).list_datasets(actor())
        self.assertEqual((legacy.scans, len(legacy.serial_calls), legacy.stats_calls), (1, 400, 1))
        self.assertEqual(legacy.batch_calls, [])
        result = await service(store).list_datasets_page(actor())
        self.assertEqual(len(result["items"]), 200)
        self.assertIsNone(result["next_cursor"])
        self.assertEqual((store.scans, len(store.batch_calls), store.stats_calls), (1, 1, 1))
        self.assertEqual(store.serial_calls, [])
        sql, ids, user, role_ids, roles = store.batch_calls[0]
        self.assertEqual((len(ids), role_ids, user, roles), (200, ids, "reader", ["analyst"]))
        self.assertIn("dataset_id = ANY($1::text[])", sql)
        self.assertIn("dataset_id = ANY($3::text[])", sql)
        self.assertIn("subject_id = ANY($4::text[])", sql)
        # Subsequent resource checks must read again, not use the catalog grant snapshot.
        await service(store)._effective_dataset_permission(store.rows[0], actor())
        self.assertEqual(len(store.serial_calls), 2)

    async def test_all_short_circuit_pages_do_not_read_grants(self):
        for user, rows, required in [
            (actor(), [row(created_by="reader")], "viewer"),
            (actor(tier="admin"), [row()], "viewer"),
            (actor(roles=["admin"]), [row()], "viewer"),
            (
                actor(is_authenticated=False),
                [row(visibility="public"), row(visibility="private")],
                "viewer",
            ),
            (actor(), [row(is_archived=True)], "viewer"),
            (actor(), [row(created_by="reader", is_archived=True)], "owner"),
            (actor(), [], "viewer"),
        ]:
            store = GrantStore(rows)
            store.fail = True
            await service(store)._visible_dataset_rows(
                user, rows, required=required, batch_permissions=True
            )
            self.assertEqual(store.batch_calls, [])
            self.assertEqual(store.serial_calls, [])

    async def test_mixed_page_reads_only_needed_ids_and_tenant_roles(self):
        rows = [
            row(dataset_id="own", created_by="reader"),
            row(dataset_id="same"),
            row(dataset_id="foreign", tenant_id="other", visibility="public"),
            row(dataset_id="archived", is_archived=True),
        ]
        store = GrantStore(rows, {("foreign", "role", "analyst"): {"permission": "owner"}})
        result = await service(store)._visible_dataset_rows(actor(), rows, batch_permissions=True)
        self.assertEqual([d["my_permission"] for d in result], ["owner", "viewer", "viewer"])
        self.assertEqual(store.batch_calls[0][1], ["same", "foreign"])
        self.assertEqual(store.batch_calls[0][3], ["same"])
        self.assertEqual(store.serial_calls, [])

    async def test_failure_is_not_visibility_fallback_and_each_page_rechecks(self):
        store = GrantStore([row()])
        svc = service(store)
        await svc.list_datasets_page(actor())
        store.fail = True
        with self.assertRaisesRegex(RuntimeError, "grant read failed"):
            await svc.list_datasets_page(actor())
        self.assertEqual(len(store.batch_calls), 2)
        self.assertEqual(store.stats_calls, 1)

    async def test_hidden_page_continues_and_lookahead_does_not_enter_grant_batch(self):
        rows = [
            row(dataset_id=f"d-{i:03}", visibility="private" if i > 1 else "tenant")
            for i in range(201, 0, -1)
        ]
        store = GrantStore(rows)
        svc = service(store)
        first = await svc.list_datasets_page(actor())
        self.assertEqual(first["items"], [])
        self.assertIsNotNone(first["next_cursor"])
        self.assertNotIn("d-001", store.batch_calls[0][1])
        second = await svc.list_datasets_page(actor(), cursor=first["next_cursor"])
        self.assertEqual([d["dataset_id"] for d in second["items"]], ["d-001"])
        self.assertIsNone(second["next_cursor"])
        self.assertEqual([len(call[1]) for call in store.batch_calls], [200, 1])

    async def test_archived_page_requires_owner_and_default_resource_stays_denied(self):
        store = GrantStore(
            [row(is_archived=True)], {("d", "user", "reader"): {"permission": "editor"}}
        )
        svc = service(store)
        self.assertEqual((await svc.list_datasets_page(actor(), archived_only=True))["items"], [])
        store.grants[("d", "user", "reader")] = {"permission": "owner"}
        self.assertEqual(
            (await svc.list_datasets_page(actor(), archived_only=True))["items"][0][
                "my_permission"
            ],
            "owner",
        )
        self.assertIsNone(await svc._effective_dataset_permission(store.rows[0], actor()))

    async def test_bounds_and_empty_batch_never_acquire(self):
        store = GrantStore()
        self.assertEqual(
            await store.get_dataset_permissions_batch(
                [], user_id="u", role_dataset_ids=[], roles=[]
            ),
            {},
        )
        for ids, role_ids in [(["d"] * 201, []), (["d"], ["d"] * 201)]:
            with self.assertRaises(ValueError):
                await store.get_dataset_permissions_batch(
                    ids, user_id="u", role_dataset_ids=role_ids, roles=[]
                )
        self.assertEqual(store.batch_calls, [])
