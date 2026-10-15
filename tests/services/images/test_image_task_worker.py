from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from ai_gateway_core.image.image_state import compute_owner_scope, compute_request_hash

from src.services.images import service as image_service
from src.services.images.service import ImageGenerationService
from src.services.images.worker import ImageTaskWorker


class _Context:
    def __init__(self, value):
        self.value = value

    async def __aenter__(self):
        return self.value

    async def __aexit__(self, *_args):
        return None


class _ClaimConnection:
    def __init__(self) -> None:
        self.statements: list[str] = []

    def transaction(self) -> _Context:
        return _Context(None)

    async def execute(self, sql: str, *_args):
        self.statements.append(sql)
        return "UPDATE 0"

    async def fetch(self, sql: str, *_args):
        self.statements.append(sql)
        return [{"task_id": "imt_a", "status": "claimed"}]


class _ClaimPool:
    def __init__(self) -> None:
        self.connection = _ClaimConnection()

    def acquire(self) -> _Context:
        return _Context(self.connection)


@pytest.mark.asyncio
async def test_claim_retries_only_pre_dispatch_work() -> None:
    service = object.__new__(ImageGenerationService)
    service.pool = _ClaimPool()

    rows = await service.claim_pending(limit=1, visibility_seconds=30)

    assert rows[0]["status"] == "claimed"
    sql = "\n".join(service.pool.connection.statements)
    assert "status='claimed'" in sql
    assert "status='running'" in sql and "worker lost after provider dispatch" in sql
    assert "status='running' AND locked_until < NOW()" not in sql


@pytest.mark.asyncio
async def test_execute_claimed_uses_atomic_dispatch_fence(monkeypatch) -> None:
    body_payload = {"prompt": "draw", "model_id": "image-model"}
    body = SimpleNamespace(model_dump=lambda **_kwargs: body_payload)
    user = SimpleNamespace(tenant_id="tenant-a", user_id="user-a")
    row = {
        "task_id": "imt_a",
        "status": "claimed",
        "tenant_id": user.tenant_id,
        "user_id": user.user_id,
        "runtime_scope_version": 1,
        "owner_scope": compute_owner_scope(
            user.user_id,
            app_tenant_id=user.tenant_id,
            app_user_id=user.user_id,
        ),
        "request_hash": compute_request_hash(body_payload),
        "turn_id": "itn_a",
        "session_id": "img_a",
    }
    monkeypatch.setattr(image_service, "get_image_task", AsyncMock(return_value=row))
    service = object.__new__(ImageGenerationService)
    service.pool = SimpleNamespace(execute=AsyncMock(return_value="UPDATE 1"))
    service.user = user
    service.request = SimpleNamespace()
    service.generate = AsyncMock(return_value={"success": True})

    result = await service.execute_claimed(body, "imt_a")

    assert result == {"success": True}
    dispatch_sql = service.pool.execute.await_args.args[0]
    assert "status='claimed'" in dispatch_sql
    assert "status='running'" in dispatch_sql
    service.generate.assert_awaited_once_with(
        body,
        task_id="imt_a",
        turn_id_override="itn_a",
        session_id_override="img_a",
    )


@pytest.mark.asyncio
async def test_execute_claimed_uses_task_row_identity_not_worker_user(monkeypatch) -> None:
    body_payload = {"prompt": "draw", "model_id": "image-model"}
    body = SimpleNamespace(model_dump=lambda **_kwargs: body_payload)
    row = {
        "task_id": "imt_a",
        "status": "claimed",
        "tenant_id": "row-tenant",
        "user_id": "row-user",
        "runtime_scope_version": 1,
        "owner_scope": compute_owner_scope(
            "row-user", app_tenant_id="row-tenant", app_user_id="row-user"
        ),
        "request_hash": compute_request_hash(body_payload),
        "turn_id": "itn_a",
        "session_id": "img_a",
    }
    monkeypatch.setattr(image_service, "get_image_task", AsyncMock(return_value=row))
    service = object.__new__(ImageGenerationService)
    service.pool = SimpleNamespace(execute=AsyncMock(return_value="UPDATE 1"))
    service.user = SimpleNamespace(tenant_id="wrong-tenant", user_id="wrong-user")
    service.request = SimpleNamespace()
    captured: list[object] = []

    async def generate(self, _body, **_kwargs):
        captured.append(self.user)
        return {"success": True}

    monkeypatch.setattr(image_service.ImageGenerationService, "generate", generate)

    await service.execute_claimed(body, "imt_a")

    assert captured and captured[0].tenant_id == "row-tenant"
    assert captured[0].user_id == "row-user"


@pytest.mark.asyncio
async def test_worker_start_drain_shutdown_is_idempotent() -> None:
    service = SimpleNamespace(claim_pending=AsyncMock(return_value=[]))
    worker = ImageTaskWorker(service, lambda task: task)

    await worker.start(poll_interval=0.05)
    await worker.start(poll_interval=0.05)
    assert worker._loop_task is not None
    await worker.shutdown()
    await worker.shutdown()
    assert worker._loop_task.done()
