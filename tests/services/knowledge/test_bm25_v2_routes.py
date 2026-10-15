from __future__ import annotations

from types import SimpleNamespace

import pytest
from fastapi import HTTPException
from knowledge_service.api.routes import bm25_v2 as bm25_routes
from knowledge_service.api.routes.bm25_v2 import (
    Bm25CutoverSchema,
    Bm25RollbackSchema,
    cancel_bm25_v2_rebuild_job,
    cutover_bm25_v2,
    dry_run_bm25_v2_cutover,
    enqueue_bm25_v2_rebuild,
    get_bm25_v2_rebuild_job,
    get_bm25_v2_state,
    rollback_bm25_v2,
    verify_bm25_v2,
)
from knowledge_service.auth.user_context import UserContext


class Lifecycle:
    def __init__(self) -> None:
        self.calls: list[tuple] = []

    async def get_lifecycle_state(self, dataset_id: str):
        self.calls.append(("get", dataset_id))
        return {"dataset_id": dataset_id, "state": "shadow", "content_revision": 7}

    async def verify_cross_authority(self, dataset_id: str):
        self.calls.append(("verify", dataset_id))
        return {"dataset_id": dataset_id, "agreement": True}

    async def cutover(self, dataset_id: str, **kwargs):
        self.calls.append(("cutover", dataset_id, kwargs))
        return {"dataset_id": dataset_id, "applied": kwargs["apply"]}

    async def rollback(self, dataset_id: str, **kwargs):
        self.calls.append(("rollback", dataset_id, kwargs))
        return {"dataset_id": dataset_id, "applied": kwargs["apply"]}


class Service:
    def __init__(self, *, allowlist: str = "") -> None:
        self.bm25_v2_lifecycle_service = Lifecycle()
        self.settings = SimpleNamespace(
            knowledge=SimpleNamespace(
                qdrant=SimpleNamespace(bm25_v2_cutover_test_tenants=allowlist)
            )
        )
        self.db = SimpleNamespace(_pool=object())

    async def require_dataset_access(self, user, dataset_id: str, *, required: str):
        assert required == "owner"
        assert user.tenant_id == "tenant-a"
        return {"dataset_id": dataset_id, "tenant_id": "tenant-a"}


OWNER = UserContext(user_id="owner-a", tenant_id="tenant-a")
OPERATOR = UserContext(
    user_id="operator-a",
    tenant_id="tenant-a",
    user_tier="admin",
    roles=["operator"],
)


class BatchStore:
    def __init__(self) -> None:
        self.operations: dict[str, dict] = {}
        self.create_calls: list[dict] = []

    async def create_operation(self, **kwargs):
        self.create_calls.append(kwargs)
        operation_id = kwargs["operation_id"]
        return self.operations.setdefault(
            operation_id,
            {
                "operation_id": operation_id,
                "tenant_id": kwargs["tenant_id"],
                "dataset_id": kwargs["dataset_id"],
                "operation": kwargs["operation"],
                "status": "pending",
            },
        )

    async def get_operation(self, **kwargs):
        return self.operations.get(kwargs["operation_id"])

    async def cancel_operation(self, **kwargs):
        operation = self.operations.get(kwargs["operation_id"])
        if operation is not None:
            operation = {**operation, "status": "failed", "error": "cancelled by operator"}
            self.operations[kwargs["operation_id"]] = operation
        return operation


@pytest.mark.asyncio
async def test_owner_can_inspect_verify_and_dry_run_but_not_cut_over() -> None:
    svc = Service(allowlist="tenant-a")
    state = await get_bm25_v2_state("dataset-a", svc=svc, user=OWNER)
    verify = await verify_bm25_v2("dataset-a", svc=svc, user=OWNER)
    dry_run = await dry_run_bm25_v2_cutover(
        "dataset-a",
        body=Bm25CutoverSchema(),
        svc=svc,
        user=OWNER,
    )
    assert state["release_status"] == "BLOCKED"
    assert verify["agreement"] is True
    assert dry_run == {
        "dataset_id": "dataset-a",
        "applied": False,
        "release_status": "BLOCKED",
    }

    with pytest.raises(HTTPException) as error:
        await cutover_bm25_v2(
            "dataset-a",
            body=Bm25CutoverSchema(),
            svc=svc,
            user=OWNER,
        )
    assert error.value.status_code == 403


@pytest.mark.asyncio
async def test_bm25_rebuild_job_is_versioned_idempotent_pollable_and_cancellable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    store = BatchStore()
    monkeypatch.setattr(bm25_routes, "_batch_store", lambda _svc: store)
    svc = Service()

    first = await enqueue_bm25_v2_rebuild("dataset-a", svc=svc, user=OWNER)
    replay = await enqueue_bm25_v2_rebuild("dataset-a", svc=svc, user=OWNER)
    fetched = await get_bm25_v2_rebuild_job(
        "dataset-a", first["job_id"], svc=svc, user=OWNER
    )
    cancelled = await cancel_bm25_v2_rebuild_job(
        "dataset-a", first["job_id"], svc=svc, user=OWNER
    )

    assert first["job_id"] == replay["job_id"] == fetched["job_id"]
    assert first["content_revision"] == 7
    assert first["execution_id"] == first["job_id"]
    assert first["job_url"].endswith(f"/rebuild/jobs/{first['job_id']}")
    assert store.create_calls[0]["all_documents"] is True
    assert store.create_calls[0]["operation"] == "reembed"
    assert cancelled["status"] == "failed"


@pytest.mark.asyncio
async def test_operator_cutover_is_test_allowlisted_but_rollback_is_always_available() -> None:
    blocked = Service(allowlist="tenant-b")
    with pytest.raises(HTTPException) as error:
        await cutover_bm25_v2(
            "dataset-a",
            body=Bm25CutoverSchema(),
            svc=blocked,
            user=OPERATOR,
        )
    assert error.value.status_code == 403
    assert blocked.bm25_v2_lifecycle_service.calls == []

    allowed = Service(allowlist="tenant-a, tenant-b")
    result = await cutover_bm25_v2(
        "dataset-a",
        body=Bm25CutoverSchema(),
        svc=allowed,
        user=OPERATOR,
    )
    assert result["release_status"] == "BLOCKED"
    assert result["scope"] == "test_tenant_only"

    # Rollback intentionally ignores the release allowlist and kill switch.
    rollback = await rollback_bm25_v2(
        "dataset-a",
        body=Bm25RollbackSchema(keep_shadow_writes=False),
        svc=blocked,
        user=OPERATOR,
    )
    assert rollback["applied"] is True
    assert blocked.bm25_v2_lifecycle_service.calls == [
        (
            "rollback",
            "dataset-a",
            {"apply": True, "keep_shadow_writes": False},
        )
    ]
