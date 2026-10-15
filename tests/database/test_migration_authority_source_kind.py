from __future__ import annotations

from typing import Any

import pytest

from database.authority import commands
from database.authority.adoption import LedgerState


class _Connection:
    def __init__(self) -> None:
        self.closed = False

    async def close(self) -> None:
        self.closed = True


class _Authority:
    def __init__(self, marker: dict[str, str] | None = None) -> None:
        self.marker = marker
        self.conn = _Connection()

    async def connect(self, *, read_only: bool = False) -> _Connection:
        assert read_only is True
        return self.conn

    async def adopted_baseline(self, _conn: Any) -> dict[str, str] | None:
        return self.marker


@pytest.mark.parametrize(
    ("marker", "empty", "state", "base_present", "expected"),
    [
        ({"baseline_id": "v1"}, False, LedgerState(), False, "adopted"),
        (None, True, LedgerState(), False, "empty"),
        (None, False, LedgerState(filename_ledger=True), True, "tracked-legacy"),
        (None, False, LedgerState(), True, "ledgerless-platform"),
    ],
)
async def test_source_kind_is_stable_and_read_only(
    monkeypatch: pytest.MonkeyPatch,
    marker: dict[str, str] | None,
    empty: bool,
    state: LedgerState,
    base_present: bool,
    expected: str,
) -> None:
    authority = _Authority(marker)

    async def database_is_empty(_conn: Any, **_kwargs: Any) -> bool:
        return empty

    async def known_database(_conn: Any) -> None:
        return None

    async def legacy_state(_conn: Any) -> LedgerState:
        return state

    async def base_schema_present(_conn: Any) -> bool:
        return base_present

    monkeypatch.setattr(commands, "database_empty", database_is_empty)
    monkeypatch.setattr(commands, "_guard_known_database", known_database)
    monkeypatch.setattr(commands, "detect_legacy_state", legacy_state)
    monkeypatch.setattr(commands.legacy, "base_schema_present", base_schema_present)
    messages: list[str] = []

    assert await commands.command_source_kind(authority, log=messages.append) == 0
    assert messages == [expected]
    assert authority.conn.closed is True
