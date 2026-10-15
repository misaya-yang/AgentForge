from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import pytest
from ai_gateway_contracts.agent_runtime import agent_memory_principal
from ai_gateway_core.agents.deletion import build_runtime_cleanup_plan

from src.services.agent_runtime_cleanup import (
    AgentRuntimeCleanupClient,
    AgentRuntimeCleanupClientError,
    _LegacyMemoryFiles,
)


class _MemoryDatabase:
    def __init__(self, row: dict[str, Any]) -> None:
        self.row = row
        self.deleted = False

    async def fetch(self, _query: str, *_args: Any) -> list[dict[str, Any]]:
        query = _query
        if "assistant_memory_chunks" in query:
            return []
        if query.lstrip().startswith("SELECT source_id FROM assistant_memory_sources"):
            return [] if self.deleted else [self.row]
        return [] if self.deleted else [self.row]

    async def execute(self, _query: str, *_args: Any) -> str:
        self.deleted = True
        return "DELETE 1"


def _plan(principal: str) -> dict[str, Any]:
    return build_runtime_cleanup_plan(
        deletion_id="00000000-0000-0000-0000-000000000001",
        tenant_id="tenant-a",
        agent_id="00000000-0000-0000-0000-000000000002",
        scope="user",
        subject_user_id="user-a",
        cutoff_at=datetime.now(timezone.utc).isoformat(),
        principal_handles=[principal],
    )


def _row(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    principal: str,
    *,
    present: bool = True,
) -> tuple[dict[str, Any], Path, str]:
    monkeypatch.setenv("AGENT_RUNTIME_MEMORY_DIR", str(tmp_path))
    files = _LegacyMemoryFiles()
    root = tmp_path / files._legacy_component("tenant-a") / files._legacy_component(principal)
    path = root / "memory" / "daily.md"
    path.parent.mkdir(parents=True)
    if present:
        path.write_text("private memory", encoding="utf-8")
    handle = (
        files._generation(path, Path("memory/daily.md"), True)
        if present
        else "memsrc_" + "1" * 32
    )
    row = {
        "source_id": "00000000-0000-0000-0000-000000000003",
        "tenant_id": "tenant-a",
        "user_id": principal,
        "source_path": str(path),
        "source_type": "daily",
        "content_hash": "abc",
        "metadata": {"source_handle": handle, "vector_collections": []},
        "created_at": datetime.now(timezone.utc),
        "updated_at": datetime.now(timezone.utc),
    }
    return row, path, handle


@pytest.mark.asyncio
async def test_cleanup_deletes_proven_legacy_file_and_sql_row(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    principal = agent_memory_principal("user", "agent", "version:v1")
    row, path, _ = _row(tmp_path, monkeypatch, principal)
    plan = _plan(principal)
    database = _MemoryDatabase(row)

    client = AgentRuntimeCleanupClient(database)
    inventory = await client.inspect(plan)
    receipt = await client.execute(plan_value=plan, inventory_value=inventory)

    assert receipt["completed"] is True
    assert database.deleted is True
    assert not path.exists()


def test_legacy_file_symlink_is_rejected(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("AGENT_RUNTIME_MEMORY_DIR", str(tmp_path))
    files = _LegacyMemoryFiles()
    root = tmp_path / files._legacy_component("tenant-a") / files._legacy_component("principal")
    root.mkdir(parents=True)
    target = tmp_path / "outside.md"
    target.write_text("outside", encoding="utf-8")
    link = root / "memory.md"
    link.symlink_to(target)

    with pytest.raises(AgentRuntimeCleanupClientError) as error:
        files.resolve("tenant-a", "principal", str(link), "memsrc_" + "1" * 32, allow_absent=False)
    assert error.value.code == "memory_source_file_unsafe"


@pytest.mark.asyncio
async def test_active_sql_source_with_missing_file_fails_closed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    principal = agent_memory_principal("user", "agent", "version:v1")
    row, _, _ = _row(tmp_path, monkeypatch, principal, present=False)
    with pytest.raises(AgentRuntimeCleanupClientError) as error:
        await AgentRuntimeCleanupClient(_MemoryDatabase(row)).inspect(_plan(principal))
    assert error.value.code == "memory_source_file_missing"
