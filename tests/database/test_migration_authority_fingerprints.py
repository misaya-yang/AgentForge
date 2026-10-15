from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from database.authority import commands
from database.authority.adoption import LedgerState
from database.authority.constants import PLATFORM_SCHEMAS
from database.authority.fingerprint import (
    FingerprintError,
    _canonical_row,
    _canonical_value,
    _like_prefix_pattern,
    acl_lines,
    compute_fingerprints,
    extensions_lines,
    reference_data_lines,
    structural_lines,
)
from database.authority.manifest import BaselineManifest, ReferenceDataSet
from database.authority.runner import AuthorityError, AuthorityPaths


def _baseline() -> BaselineManifest:
    return BaselineManifest(
        baseline_id="2026_08_post_kb_v1",
        schema_revision="112",
        source_git_sha="a" * 40,
        last_legacy_change="112_last.sql",
        structural_sha256="1" * 64,
        acl_sha256="2" * 64,
        extensions_sha256="3" * 64,
        reference_data_sha256="4" * 64,
        generator="test",
        generated_at="2026-08-30T00:00:00Z",
        postgres_version="16",
    )


def _marker(baseline: BaselineManifest, manifest_sha: str = "5" * 64) -> dict[str, str]:
    return {
        "baseline_id": baseline.baseline_id,
        "manifest_sha256": manifest_sha,
        "structural_sha256": baseline.structural_sha256,
        "acl_sha256": baseline.acl_sha256,
        "extensions_sha256": baseline.extensions_sha256,
        "reference_data_sha256": baseline.reference_data_sha256,
        "source_git_sha": baseline.source_git_sha,
        "adopted_at": "2026-08-30T00:00:00Z",
    }


class EmptyConnection:
    def __init__(self) -> None:
        self.queries: list[tuple[str, tuple[Any, ...]]] = []
        self.settings: list[tuple[str, tuple[Any, ...]]] = []

    def transaction(self) -> EmptyConnection:
        return self

    async def __aenter__(self) -> EmptyConnection:
        return self

    async def __aexit__(self, *_args: Any) -> None:
        return None

    async def fetchval(self, query: str, *_args: Any) -> str:
        assert query == "SELECT current_setting('search_path')"
        return "pg_catalog, gateway, assistant, knowledge, public"

    async def execute(self, query: str, *args: Any) -> None:
        self.settings.append((query, args))

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        self.queries.append((query, args))
        return []


class AclConnection(EmptyConnection):
    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        self.queries.append((query, args))
        if "pg_get_userbyid(c.relowner) AS owner" in query:
            return [{"schema": "public", "name": "widgets", "owner": "ai_platform_owner"}]
        if "pg_get_userbyid(n.nspowner) AS owner" in query:
            return [{"schema": "public", "owner": "ai_platform_owner"}]
        if "pg_get_userbyid(p.proowner) AS owner" in query:
            return [
                {
                    "schema": "public",
                    "name": "run_widget",
                    "identity_args": "uuid",
                    "owner": "ai_platform_owner",
                }
            ]
        if "pg_get_userbyid(t.typowner) AS owner" in query:
            return [
                {
                    "schema": "public",
                    "name": "widget_state",
                    "type": "e",
                    "owner": "ai_platform_owner",
                }
            ]
        if "aclexplode(COALESCE(c.relacl" in query:
            return [
                {
                    "schema": "public",
                    "name": "widgets",
                    "grantee": "ai_platform_gateway",
                    "grant_option": False,
                    "privileges": "SELECT",
                }
            ]
        if "aclexplode(attribute.attacl)" in query:
            return [
                {
                    "schema": "public",
                    "relation": "widgets",
                    "column": "tenant_id",
                    "grantee": "ai_platform_gateway",
                    "grant_option": False,
                    "privileges": "SELECT",
                }
            ]
        if "aclexplode(COALESCE(p.proacl" in query:
            return [
                {
                    "schema": "public",
                    "name": "run_widget",
                    "identity_args": "uuid",
                    "grantee": "ai_platform_runtime",
                    "grant_option": False,
                    "privileges": "EXECUTE",
                }
            ]
        if "aclexplode(COALESCE(n.nspacl" in query:
            return [
                {
                    "schema": "public",
                    "grantee": "ai_platform_gateway",
                    "grant_option": False,
                    "privileges": "USAGE",
                }
            ]
        if "FROM pg_database AS database" in query:
            return [
                {
                    "grantee": "ai_platform_owner",
                    "grant_option": False,
                    "privileges": "CREATE",
                }
            ]
        if "aclexplode(COALESCE(t.typacl" in query:
            return [
                {
                    "schema": "public",
                    "name": "widget_state",
                    "type": "e",
                    "grantee": "ai_platform_gateway",
                    "grant_option": False,
                    "privileges": "USAGE",
                }
            ]
        if "FROM pg_policy AS policy" in query:
            return [
                {
                    "schema": "public",
                    "relation": "widgets",
                    "name": "tenant_widgets",
                    "permissive": False,
                    "command": "r",
                    "roles": ["ai_platform_gateway"],
                    "using_expr": "tenant_id = current_setting('app.tenant')::uuid",
                    "check_expr": "",
                }
            ]
        if "FROM pg_default_acl AS d" in query:
            return [
                {
                    "grantor": "ai_platform_owner",
                    "object_type": "table",
                    "schema": "public",
                    "grantee": "ai_platform_gateway",
                    "grant_option": False,
                    "privileges": "SELECT",
                }
            ]
        if "FROM pg_roles AS r" in query:
            return [
                {
                    "name": "ai_platform_gateway",
                    "rolsuper": False,
                    "rolcreaterole": False,
                    "rolcreatedb": False,
                    "rolcanlogin": True,
                    "rolinherit": True,
                    "rolreplication": True,
                    "rolbypassrls": True,
                    "config": "",
                    "memberships": ["ai_platform_owner:0:0:1"],
                }
            ]
        raise AssertionError(f"unexpected ACL query: {query}")


def test_like_role_prefix_is_literal_not_a_wildcard() -> None:
    assert _like_prefix_pattern("ai_platform_") == r"ai\_platform\_%"
    assert _like_prefix_pattern("a_%\\") == r"a\_\%\\%"


async def test_structural_queries_filter_temporary_schemas() -> None:
    conn = EmptyConnection()

    fingerprints = await structural_lines(conn)

    assert fingerprints == []
    schema_query = next(query for query, _ in conn.queries if "FROM pg_namespace AS n" in query)
    sequence_query = next(query for query, _ in conn.queries if "FROM pg_sequence AS s" in query)
    for query in (schema_query, sequence_query):
        assert "n.nspname = ANY($1)" in query
        assert "n.nspname <> ALL($1)" not in query
        assert "NOT LIKE 'pg_temp_%'" in query
        assert "NOT LIKE 'pg_toast_temp_%'" in query
    for _query, args in conn.queries:
        assert args == (list(PLATFORM_SCHEMAS),)
    assert any("JOIN pg_aggregate AS a" in query for query, _args in conn.queries)
    assert any("pg_get_viewdef" in query for query, _args in conn.queries)
    assert any("pg_get_triggerdef" in query for query, _args in conn.queries)
    assert any("type_relation.relkind = 'c'" in query for query, _args in conn.queries)
    relation_query = next(
        query for query, _args in conn.queries if "c.relpersistence AS persistence" in query
    )
    assert "c.relrowsecurity AS row_security" in relation_query
    assert "c.relreplident AS replica_identity" in relation_query
    assert "pg_get_partkeydef" in relation_query
    column_query = next(
        query for query, _args in conn.queries if "a.attidentity AS identity" in query
    )
    assert "a.attgenerated AS generated" in column_query
    assert "a.attcompression AS compression" in column_query
    assert "pg_collation AS coll" in column_query
    assert "pg_collation AS collation" not in column_query
    type_query = next(query for query, _args in conn.queries if "t.typbasetype = 0" in query)
    assert "pg_collation AS coll" in type_query
    assert "pg_collation AS collation" not in type_query
    trigger_query = next(query for query, _args in conn.queries if "pg_get_triggerdef" in query)
    assert "trigger.tgenabled AS enabled" in trigger_query
    for query, _args in conn.queries:
        if "FROM pg_class AS c" in query or "FROM pg_proc AS p" in query:
            assert "extension_dependency.deptype = 'e'" in query


async def test_acl_covers_all_owner_acl_classes_and_default_privileges() -> None:
    conn = AclConnection()

    lines = await acl_lines(conn, role_prefix="ai_platform_")

    relation_acl_query = next(
        query for query, _args in conn.queries if "aclexplode(COALESCE(c.relacl" in query
    )
    assert 'END)::"char"' in relation_acl_query
    for query, _args in conn.queries:
        if "aclexplode" in query:
            assert ".grant_option" not in query
            assert ".is_grantable" in query

    assert "owner:public.widgets:owner" in lines
    assert "schema_owner:public:owner" in lines
    assert "function_owner:public.run_widget(uuid):owner" in lines
    assert "type_owner:public.widget_state:e:owner" in lines
    assert "acl:public.widgets:gateway:SELECT:grantopt=0" in lines
    assert "column_acl:public.widgets.tenant_id:gateway:SELECT:grantopt=0" in lines
    assert "function_acl:public.run_widget(uuid):runtime:EXECUTE:grantopt=0" in lines
    assert "schema_acl:public:gateway:USAGE:grantopt=0" in lines
    assert "database_acl:owner:CREATE:grantopt=0" in lines
    assert "type_acl:public.widget_state:e:gateway:USAGE:grantopt=0" in lines
    assert (
        "policy:public.widgets.tenant_widgets:permissive=0:command=r:roles=gateway:"
        "using=tenant_id = current_setting('app.tenant')::uuid:check="
    ) in lines
    assert "default_privilege:owner:table:public:gateway:SELECT:grantopt=0" in lines
    assert "role:gateway:LIPB::memberships=owner:admin=0:inherit=0:set=1" in lines

    default_query, default_args = next(
        (query, args) for query, args in conn.queries if "FROM pg_default_acl AS d" in query
    )
    roles_query, roles_args = next(
        (query, args) for query, args in conn.queries if "FROM pg_roles AS r" in query
    )
    assert "GROUP BY d.defaclrole, d.defaclobjtype, n.nspname" in default_query
    assert "a.grantee, a.is_grantable" in default_query
    assert default_args[0] == list(PLATFORM_SCHEMAS)
    assert "NOT LIKE 'pg_temp_%'" in default_query
    assert "ESCAPE E'\\\\'" in default_query
    assert default_args[1] == r"ai\_platform\_%"
    assert "ESCAPE E'\\\\'" in roles_query
    assert "FROM unnest(r.rolconfig) AS option" in roles_query
    assert roles_args == (r"ai\_platform\_%",)


def test_reference_row_serialization_has_no_delimiter_or_type_collisions() -> None:
    assert _canonical_row(["a|b", "c"]) != _canonical_row(["a", "b|c"])
    assert _canonical_value(None) != _canonical_value("\x00null")
    assert _canonical_value(True) != _canonical_value("true")
    assert _canonical_value(1) != _canonical_value("1")
    assert _canonical_value({"b": [2], "a": 1}) == _canonical_value({"a": 1, "b": [2]})


def test_reference_serialization_rejects_unknown_driver_types() -> None:
    with pytest.raises(FingerprintError, match="unsupported reference-data value type"):
        _canonical_value(object())


class ReferenceConnection:
    def __init__(self, row: dict[str, Any]) -> None:
        self.row = row

    async def fetch(self, _query: str, *_args: Any) -> list[dict[str, Any]]:
        return [self.row]


class ExtensionConnection:
    def __init__(self, name: str) -> None:
        self.name = name

    async def fetch(self, _query: str, *_args: Any) -> list[dict[str, Any]]:
        return [
            {
                "extname": self.name,
                "extversion": "1.0",
                "schema": "public",
                "owner": "ai_platform_owner",
            }
        ]


async def test_extension_fingerprint_binds_schema_owner_and_allowlist() -> None:
    assert await extensions_lines(ExtensionConnection("pgcrypto"), role_prefix="ai_platform_") == [
        "extension:pgcrypto:1.0:schema=public:owner=owner"
    ]

    with pytest.raises(FingerprintError, match="not in the allowlist"):
        await extensions_lines(ExtensionConnection("unreviewed"), role_prefix="ai_platform_")


async def test_reference_fingerprint_rows_preserve_column_boundaries() -> None:
    reference = ReferenceDataSet(
        table="public.system_values",
        natural_key=("key",),
        immutable_columns=("value",),
    )

    left = await reference_data_lines(
        ReferenceConnection({"key": "a|b", "value": "c"}),
        [reference],
    )
    right = await reference_data_lines(
        ReferenceConnection({"key": "a", "value": "b|c"}),
        [reference],
    )

    assert left != right


async def test_reference_fingerprint_is_independent_of_database_collation_order() -> None:
    reference = ReferenceDataSet(
        table="public.system_values",
        natural_key=("key",),
        immutable_columns=("value",),
    )

    class OrderedConnection:
        def __init__(self, rows: list[dict[str, str]]) -> None:
            self.rows = rows

        async def fetch(self, query: str, *_args: Any) -> list[dict[str, str]]:
            assert "ORDER BY" not in query
            return self.rows

    rows = [{"key": "ä", "value": "2"}, {"key": "z", "value": "1"}]

    assert await reference_data_lines(OrderedConnection(rows), [reference]) == (
        await reference_data_lines(OrderedConnection(list(reversed(rows))), [reference])
    )


async def test_compute_fingerprints_always_returns_the_four_named_classes() -> None:
    conn = EmptyConnection()
    computed = await compute_fingerprints(conn, role_prefix="ai_platform_")

    assert set(computed) == {"structural", "acl", "extensions", "reference_data"}
    assert all(len(digest) == 64 for digest in computed.values())
    assert conn.settings == [
        ("SELECT set_config('search_path', 'pg_catalog', true)", ()),
        (
            "SELECT set_config('search_path', $1, true)",
            ("pg_catalog, gateway, assistant, knowledge, public",),
        ),
    ]


@pytest.mark.parametrize(
    "field",
    [
        "baseline_id",
        "manifest_sha256",
        "structural_sha256",
        "acl_sha256",
        "extensions_sha256",
        "reference_data_sha256",
        "source_git_sha",
    ],
)
def test_existing_adoption_marker_must_match_every_frozen_field(field: str) -> None:
    baseline = _baseline()
    marker = _marker(baseline)
    commands._validate_adoption_marker(marker, baseline, "5" * 64)
    marker[field] = "drift"

    with pytest.raises(AuthorityError, match=field):
        commands._validate_adoption_marker(marker, baseline, "5" * 64)


class CommandConnection:
    def __init__(self) -> None:
        self.closed = False
        self.executed: list[str] = []

    async def close(self) -> None:
        self.closed = True

    async def execute(self, query: str, *_args: Any) -> None:
        self.executed.append(query)

    async def fetch(self, _query: str, *_args: Any) -> list[dict[str, Any]]:
        return []


class CommandAuthority:
    def __init__(self, tmp_path: Path, marker: dict[str, str]) -> None:
        self.paths = AuthorityPaths(database_dir=tmp_path / "database")
        self.role_prefix = "ai_platform_"
        self.marker = marker
        self.lock_conn = CommandConnection()
        self.conn = CommandConnection()
        self.read_only_flags: list[bool] = []
        self.epoch_called = False

    async def connect(self, *, read_only: bool = False) -> CommandConnection:
        self.read_only_flags.append(read_only)
        return self.conn if read_only or len(self.read_only_flags) > 1 else self.lock_conn

    async def acquire_lock(self, _conn: CommandConnection) -> None:
        return None

    async def release_lock(self, conn: CommandConnection) -> None:
        await conn.close()

    async def adopted_baseline(self, _conn: CommandConnection) -> dict[str, str]:
        return self.marker

    async def apply_epoch(self, *_args: Any, **_kwargs: Any) -> list[str]:
        self.epoch_called = True
        return []


async def test_migrate_rejects_existing_marker_manifest_drift_before_epoch(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    baseline = _baseline()
    marker = _marker(baseline)
    marker["manifest_sha256"] = "bad"
    authority = CommandAuthority(tmp_path, marker)
    monkeypatch.setattr(commands, "baseline_ready", lambda *_args: True)
    monkeypatch.setattr(
        commands,
        "load_baseline",
        lambda *_args: (baseline, "5" * 64),
    )

    with pytest.raises(AuthorityError, match="manifest_sha256"):
        await commands.command_migrate(authority)

    assert not authority.epoch_called
    assert authority.conn.closed
    assert authority.lock_conn.closed


async def test_migrate_routes_empty_database_to_frozen_fresh_baseline(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    baseline = _baseline()
    authority = CommandAuthority(tmp_path, _marker(baseline))
    authority.marker = None  # type: ignore[assignment]
    calls: list[str] = []
    monkeypatch.setattr(commands, "baseline_ready", lambda *_args: True)
    monkeypatch.setattr(
        commands,
        "load_baseline",
        lambda *_args: (baseline, "5" * 64),
    )

    async def empty(_conn: Any, **_kwargs: Any) -> bool:
        return True

    async def fresh(*_args: Any, **_kwargs: Any) -> dict[str, str]:
        calls.append("fresh")
        return baseline.fingerprints

    async def forbidden_legacy(*_args: Any, **_kwargs: Any) -> None:
        raise AssertionError("empty frozen-baseline installs must not execute legacy SQL")

    monkeypatch.setattr(commands, "database_empty", empty)
    monkeypatch.setattr(commands, "fresh_install", fresh)
    monkeypatch.setattr(commands.legacy, "apply_legacy_chain", forbidden_legacy)

    result = await commands.command_migrate(authority, log=calls.append)

    assert result.exit_code == 0
    assert calls[0] == "fresh"
    assert "fresh install on baseline" in calls[-1]
    assert authority.conn.closed
    assert authority.lock_conn.closed


async def test_migrate_rejects_ledgerless_schema_without_adoption_or_legacy_replay(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    baseline = _baseline()
    authority = CommandAuthority(tmp_path, _marker(baseline))
    authority.marker = None  # type: ignore[assignment]
    monkeypatch.setattr(commands, "baseline_ready", lambda *_args: True)
    monkeypatch.setattr(
        commands,
        "load_baseline",
        lambda *_args: (baseline, "5" * 64),
    )

    async def not_empty(_conn: Any, **_kwargs: Any) -> bool:
        return False

    async def known_database(_conn: Any) -> None:
        return None

    async def ledgerless(_conn: Any) -> LedgerState:
        return LedgerState()

    async def forbidden_legacy(*_args: Any, **_kwargs: Any) -> None:
        raise AssertionError("ledgerless schema must not replay historical migrations")

    monkeypatch.setattr(commands, "database_empty", not_empty)
    monkeypatch.setattr(commands, "_guard_known_database", known_database)
    monkeypatch.setattr(commands, "detect_legacy_state", ledgerless)
    monkeypatch.setattr(commands.legacy, "apply_legacy_chain", forbidden_legacy)
    monkeypatch.setattr(commands.legacy, "apply_per_service_chain", forbidden_legacy)

    with pytest.raises(AuthorityError, match="cannot stop before adoption"):
        await commands.command_migrate(
            authority,
            allow_adoption=False,
        )


async def test_ledgerless_cutover_requires_explicit_mode_and_base_objects(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    async def ledgerless(_conn: Any) -> LedgerState:
        return LedgerState()

    async def base_present(_conn: Any) -> bool:
        return True

    monkeypatch.setattr(commands, "detect_legacy_state", ledgerless)
    monkeypatch.setattr(commands.legacy, "base_schema_present", base_present)

    with pytest.raises(AuthorityError, match="no legacy ledger"):
        await commands._verify_legacy_cutover_ready(
            object(),
            AuthorityPaths(tmp_path / "database"),
        )

    messages: list[str] = []
    result = await commands._verify_legacy_cutover_ready(
        object(),
        AuthorityPaths(tmp_path / "database"),
        allow_ledgerless_schema=True,
        log=messages.append,
    )
    assert result is None
    assert any("frozen fingerprints" in message for message in messages)


async def test_legacy_base_schema_never_replays_schema_snapshot(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    async def base_absent(_conn: Any) -> bool:
        return False

    monkeypatch.setattr(commands.legacy, "base_schema_present", base_absent)

    with pytest.raises(AuthorityError, match="schema.sql replay is retired"):
        await commands.legacy.ensure_base_schema(
            object(),
            AuthorityPaths(tmp_path / "database"),
        )


async def test_cutover_matching_existing_marker_is_a_read_only_noop(tmp_path: Path) -> None:
    baseline = _baseline()
    authority = CommandAuthority(tmp_path, _marker(baseline))
    messages: list[str] = []

    await commands._cutover_and_adopt(
        authority.conn,
        authority,
        baseline,
        "5" * 64,
        log=messages.append,
    )

    assert authority.conn.executed == []
    assert messages == [f"authority: baseline already adopted ({baseline.baseline_id})"]


async def test_verify_is_read_only_and_rejects_source_marker_drift(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    baseline = _baseline()
    marker = _marker(baseline)
    marker["source_git_sha"] = "b" * 40
    authority = CommandAuthority(tmp_path, marker)
    monkeypatch.setattr(commands, "baseline_ready", lambda *_args: True)
    monkeypatch.setattr(
        commands,
        "load_baseline",
        lambda *_args: (baseline, "5" * 64),
    )

    with pytest.raises(AuthorityError, match="source_git_sha"):
        await commands.command_verify(authority)

    assert authority.read_only_flags == [True]
    assert authority.conn.executed == []
    assert authority.conn.closed
