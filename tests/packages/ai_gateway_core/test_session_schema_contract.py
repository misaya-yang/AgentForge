from __future__ import annotations

import ast
import re
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_core.persistence.database import DatabaseStorage
from ai_gateway_core.persistence.repositories.session_repository import (
    DatabaseSessionRepository,
)

ROOT = Path(__file__).resolve().parents[3]
CANONICAL_SESSION_SQL_SOURCES = (
    ROOT
    / "packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py",
    ROOT
    / "packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/session_repository.py",
    ROOT
    / "packages/ai-gateway-core/src/ai_gateway_core/session/database_manager.py",
    ROOT
    / "packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py",
    ROOT / "src/api/v1/conversation_shares.py",
)
UNQUALIFIED_SESSION_RELATION = re.compile(
    r"\b(?:FROM|JOIN|INSERT\s+INTO|UPDATE|DELETE\s+FROM|REFERENCES)\s+"
    r"(?:ONLY\s+)?sessions\b",
    re.IGNORECASE,
)
UNQUALIFIED_SESSION_REFERENCE = re.compile(r"(?<![\w.])sessions\.", re.IGNORECASE)


class _Connection:
    """Resolve bare relations through search_path like PostgreSQL does."""

    def __init__(self, search_path: tuple[str, ...]) -> None:
        self.search_path = search_path
        self.queries: list[str] = []
        self.execute_args: list[tuple[Any, ...]] = []
        self.rows = {
            "gateway.sessions": {
                "session_id": "session-1",
                "history": [],
                "metadata": {"source": "shadow"},
            },
            "assistant.sessions": {
                "session_id": "session-1",
                "history": [{"role": "user", "content": "canonical"}],
                "metadata": {"source": "canonical"},
            },
        }

    def _relation(self, query: str, operation: str) -> str:
        normalized = " ".join(query.split())
        match = re.search(rf"\b{operation}\s+([a-z_.]+)", normalized, re.IGNORECASE)
        assert match is not None, normalized
        relation = match.group(1).lower()
        if "." in relation:
            return relation
        for schema in self.search_path:
            candidate = f"{schema}.{relation}"
            if candidate in self.rows:
                return candidate
        raise AssertionError(f"unresolved relation for search_path={self.search_path}: {query}")

    async def fetchrow(self, query: str, *_args: Any) -> dict[str, Any] | None:
        self.queries.append(query)
        return self.rows.get(self._relation(query, "FROM"))

    async def execute(self, query: str, *args: Any) -> str:
        self.queries.append(query)
        self.execute_args.append(args)
        self._relation(query, "INSERT INTO")
        return "INSERT 0 1"


class _Acquire:
    def __init__(self, connection: _Connection) -> None:
        self.connection = connection

    async def __aenter__(self) -> _Connection:
        return self.connection

    async def __aexit__(self, *_args: object) -> None:
        return None


class _Pool:
    def __init__(self, connection: _Connection) -> None:
        self.connection = connection

    def acquire(self) -> _Acquire:
        return _Acquire(self.connection)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "search_path",
    (("gateway", "assistant", "public"), ("assistant", "gateway", "public")),
)
async def test_shared_session_storage_ignores_search_path_shadow(
    search_path: tuple[str, ...],
) -> None:
    connection = _Connection(search_path)
    pool = _Pool(connection)
    database = DatabaseStorage.__new__(DatabaseStorage)
    database._pool = pool

    await database.save_session(
        {
            "session_id": "session-1",
            "user_id": "user-1",
            "tenant_id": "tenant-1",
        }
    )
    direct = await database.get_session("session-1")
    repository = DatabaseSessionRepository(
        SimpleNamespace(enabled=True, _pool=pool),
    )
    extracted = await repository.get("session-1")

    assert direct is not None and direct["metadata"]["source"] == "canonical"
    assert extracted is not None and extracted["metadata"]["source"] == "canonical"
    assert all("assistant.sessions" in query for query in connection.queries)
    assert len(connection.execute_args[0]) == 10
    assert connection.execute_args[0][8] == "active"
    assert connection.execute_args[0][9] is None


@pytest.mark.parametrize("source", CANONICAL_SESSION_SQL_SOURCES, ids=lambda path: path.name)
def test_canonical_session_sql_never_relies_on_search_path(source: Path) -> None:
    contents = source.read_text(encoding="utf-8")
    tree = ast.parse(contents, filename=str(source))
    sql_literals = "\n".join(
        node.value
        for node in ast.walk(tree)
        if isinstance(node, ast.Constant) and isinstance(node.value, str)
    )

    relation_matches = UNQUALIFIED_SESSION_RELATION.findall(sql_literals)
    reference_matches = UNQUALIFIED_SESSION_REFERENCE.findall(sql_literals)

    assert relation_matches == [], f"unqualified session relations in {source}: {relation_matches}"
    assert reference_matches == [], f"unqualified session references in {source}: {reference_matches}"
