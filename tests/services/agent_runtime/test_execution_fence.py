"""Original model lease/budget survives recovery, while stale owners fail closed."""

from __future__ import annotations

import uuid
from datetime import datetime, timedelta, timezone
from hashlib import sha256
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from ai_gateway_contracts.agent_runtime_lease import (
    RuntimeModelLeaseClaims,
    RuntimeModelLeaseSigner,
)

from src.services.agent_runtime import model_plane
from src.services.agent_runtime.model import authorization
from src.services.agent_runtime.model_plane import AgentModelPlaneError


class _DB:
    def __init__(self):
        now = datetime.now(timezone.utc)
        self.row = {
            "schema_version": "agent-runtime-model-lease/v1", "lease_id": uuid.uuid4(),
            "snapshot_id": uuid.uuid4(), "run_id": uuid.uuid4(), "runtime_thread_id": uuid.uuid4(),
            "tenant_id": "tenant-a", "user_id": "user-a", "session_id": "session-a",
            "provider_id": "provider-a", "model_id": "model-a", "provider_revision": "pinned-revision",
            "capability_revision": 1, "issued_at": now, "expires_at": now + timedelta(minutes=10),
            "nonce_sha256": "a" * 64, "snapshot": {}, "snapshot_sha256": sha256(b"{}").hexdigest(),
            "execution_owner_id": uuid.uuid4(), "execution_fence": 2,
        }
        self.reservations = []

    async def fetchrow(self, sql, *args):
        if "FROM assistant_runtime_model_leases" in sql:
            assert "o.lease_until > NOW()" in sql
            return self.row
        assert "reserve_assistant_runtime_model_call" in sql
        if any(previous[2] == args[2] for previous in self.reservations):
            raise RuntimeError("MODEL_CALL_REPLAYED")
        self.reservations.append(args)
        return None


def _fixture():
    db = _DB()
    signer = RuntimeModelLeaseSigner("fixture-only-model-signing-key-32")
    row = db.row
    claims = RuntimeModelLeaseClaims(**{
        **{key: str(row[key]) for key in (
            "schema_version", "lease_id", "snapshot_id", "run_id", "runtime_thread_id",
            "tenant_id", "user_id", "session_id", "provider_id", "model_id", "nonce_sha256",
        )},
        "capability_revision": 1,
        "issued_at_ms": int(row["issued_at"].timestamp() * 1000),
        "expires_at_ms": int(row["expires_at"].timestamp() * 1000),
    })
    plane = SimpleNamespace(database=db, lease_signer=signer, _validate_turn_thread_scope=AsyncMock())
    metadata = {
        "ai_platform_lease_id": claims.lease_id, "ai_platform_lease_signature": signer.sign(claims),
        "ai_platform_execution_owner": str(row["execution_owner_id"]), "ai_platform_execution_fence": "2",
    }
    return db, plane, metadata


@pytest.mark.asyncio
async def test_stale_owner_is_rejected_before_model_reservation():
    db, plane, metadata = _fixture()
    metadata["ai_platform_execution_owner"] = str(uuid.uuid4())
    with pytest.raises(AgentModelPlaneError, match="RUNTIME_EXECUTION_FENCE_LOST"):
        await authorization.authorize_and_reserve(plane, body={"model": "model-a", "input": "original"}, turn_metadata=metadata, _helpers=model_plane)
    assert db.reservations == []
    plane._validate_turn_thread_scope.assert_not_awaited()


@pytest.mark.asyncio
async def test_recovery_generation_keeps_original_lease_and_replay_budget():
    db, plane, metadata = _fixture()
    body = {"model": "model-a", "input": "original", "stream": True}
    first = await authorization.authorize_and_reserve(plane, body=body, turn_metadata=metadata, _helpers=model_plane)
    with pytest.raises(AgentModelPlaneError, match="RUNTIME_MODEL_CALL_REPLAYED"):
        await authorization.authorize_and_reserve(plane, body=body, turn_metadata=metadata, _helpers=model_plane)
    db.row["execution_fence"] = 3
    metadata["ai_platform_execution_fence"] = "3"
    second = await authorization.authorize_and_reserve(plane, body=body, turn_metadata=metadata, _helpers=model_plane)
    assert first.lease_id == second.lease_id == db.row["lease_id"]
    assert first.run_id == second.run_id and first.provider_revision == second.provider_revision
    assert len(db.reservations) == 2
    assert db.reservations[0][2] != db.reservations[1][2]
    assert db.reservations[0][3:6] == db.reservations[1][3:6]
    assert db.reservations[-1][6:] == (db.row["execution_owner_id"], 3)
