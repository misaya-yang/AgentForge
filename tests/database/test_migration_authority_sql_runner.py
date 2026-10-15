from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from database.authority import bootstrap


async def test_failed_role_sql_preserves_original_error_without_reset(tmp_path: Path) -> None:
    sql_path = tmp_path / "failing.sql"
    sql_path.write_text("-- FAIL DDL\n", encoding="utf-8")

    class Transaction:
        async def __aenter__(self) -> Transaction:
            return self

        async def __aexit__(self, *_args: Any) -> None:
            return None

    class Connection:
        def __init__(self) -> None:
            self.executed: list[str] = []

        def transaction(self) -> Transaction:
            return Transaction()

        async def execute(self, query: str, *_args: Any) -> None:
            self.executed.append(query)
            if query == "-- FAIL DDL\n":
                raise RuntimeError("original DDL failure")
            if query == "RESET ROLE":
                raise AssertionError("RESET ROLE must not run in an aborted transaction")

    conn = Connection()
    with pytest.raises(RuntimeError, match="original DDL failure"):
        await bootstrap.run_baseline_sql_file(
            conn,
            sql_path,
            execution_role="ai_gateway_owner",
        )

    assert conn.executed == ['SET LOCAL ROLE "ai_gateway_owner"', "-- FAIL DDL\n"]


async def test_empty_preflight_ignores_only_allowlisted_extension_members() -> None:
    class EmptyProbeConnection:
        def __init__(self) -> None:
            self.query = ""
            self.args: tuple[Any, ...] = ()

        async def fetchval(self, query: str, *args: Any) -> int:
            self.query = query
            self.args = args
            return 0

    conn = EmptyProbeConnection()

    assert await bootstrap.database_empty(conn, allowed_empty_schemas=("knowledge",))
    for catalog in ("pg_class", "pg_proc", "pg_type"):
        assert f"dependency.classid = '{catalog}'::regclass" in conn.query
    assert conn.args[0] == ["knowledge"]
    assert set(conn.args[1]) == {"plpgsql", "uuid-ossp", "pgcrypto", "pg_trgm", "vector"}
