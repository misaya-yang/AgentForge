#!/usr/bin/env python3
"""Freeze ARC-03 baseline artifacts from two explicitly isolated PostgreSQL DBs.

This tool never creates, drops, truncates or resets a database.  It requires a
clean pending source commit, a converged source scratch database, and a second
empty verification scratch database.  The repository manifest is replaced
last, so incomplete file writes remain visibly ``pending-live-freeze``.
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import parse_qsl, unquote, urlsplit

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parents[1]
INVENTORY_DIR = REPO_ROOT / "scripts/inventory"
sys.path.insert(0, str(REPO_ROOT))
sys.path.insert(0, str(INVENTORY_DIR))
sys.path.insert(0, str(HERE))

import database_policy  # noqa: E402
import generate_database_grants  # noqa: E402
import render_baseline_contract  # noqa: E402

from database.authority.bootstrap import (  # noqa: E402
    fresh_install,
    provision_extensions_admin,
    provision_roles_admin,
    verify_baseline_sql_file,
)
from database.authority.constants import (  # noqa: E402
    DEFAULT_BASELINE_ID,
    DEFAULT_ROLE_PREFIX,
)
from database.authority.credentials import (  # noqa: E402
    ADMIN,
    DSN_ENV_BY_PRINCIPAL,
    MIGRATOR,
    role_dsns,
    verify_role_connections,
)
from database.authority.fingerprint import compute_fingerprints  # noqa: E402
from database.authority.manifest import (  # noqa: E402
    BASELINE_MANIFEST_SCHEMA,
    BASELINE_STATE_FROZEN,
    ReferenceDataSet,
    file_sha256,
    load_baseline_manifest,
    load_legacy_manifest,
)
from database.authority.runner import AuthorityError, AuthorityPaths  # noqa: E402

SOURCE_DSN_ENV = "ARC03_FREEZE_SOURCE_MIGRATOR_DSN"
VERIFY_ADMIN_DSN_ENV = "ARC03_FREEZE_VERIFY_ADMIN_DSN"
VERIFY_MIGRATOR_DSN_ENV = "ARC03_FREEZE_VERIFY_MIGRATOR_DSN"
SQL_FILES = (
    "cutover_convergence.sql",
    "grants.sql",
    "init.sql",
    "reference_data.sql",
    "verify.sql",
)
POLICY_FILES = (
    "data-access-inventory.json",
    "ownership-policy.json",
    "grants-policy.json",
)


class FreezeError(RuntimeError):
    """Live state did not meet the immutable freeze contract."""


@dataclass(frozen=True)
class ReferenceSpec:
    table: str
    natural_key: tuple[str, ...]
    immutable_columns: tuple[str, ...]
    where: str
    array_columns: tuple[str, ...] = ()

    @property
    def columns(self) -> tuple[str, ...]:
        return tuple(dict.fromkeys((*self.natural_key, *self.immutable_columns)))

    def manifest_entry(self) -> dict[str, object]:
        return {
            "table": self.table,
            "natural_key": list(self.natural_key),
            "immutable_columns": list(self.immutable_columns),
            "where": self.where,
        }

    def fingerprint_spec(self) -> ReferenceDataSet:
        return ReferenceDataSet(
            table=self.table,
            natural_key=self.natural_key,
            immutable_columns=self.immutable_columns,
            where=self.where,
        )


REFERENCE_SPECS = (
    ReferenceSpec(
        table="gateway.permissions",
        natural_key=("permission_code",),
        immutable_columns=(
            "name",
            "description",
            "category",
            "resource",
            "action",
            "is_system",
        ),
        where="is_system IS TRUE",
    ),
    ReferenceSpec(
        table="gateway.rbac_roles",
        natural_key=("role_name",),
        immutable_columns=("description", "permissions", "is_system"),
        where="is_system IS TRUE",
        array_columns=("permissions",),
    ),
    ReferenceSpec(
        table="gateway.role_permissions",
        natural_key=("role_name", "permission_code"),
        # Key-only associations are immutable; overlap makes that explicit to
        # the v1 manifest model without adding volatile ids/timestamps.
        immutable_columns=("role_name", "permission_code"),
        where=(
            "role_name IN (SELECT role_name FROM gateway.rbac_roles "
            "WHERE is_system IS TRUE)"
        ),
    ),
)


def _run(*args: str, cwd: Path = REPO_ROOT) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(args),
        cwd=cwd,
        capture_output=True,
        text=True,
        check=False,
    )


def _required_env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise FreezeError(f"required secret environment variable {name} is absent")
    return value


def _source_commit(baseline_dir: Path) -> tuple[str, str]:
    status = _run("git", "status", "--porcelain=v1", "--untracked-files=normal")
    if status.returncode != 0 or status.stdout:
        raise FreezeError("baseline freeze requires a clean source commit")
    head = _run("git", "rev-parse", "HEAD")
    if head.returncode != 0 or re.fullmatch(r"[0-9a-f]{40}", head.stdout.strip()) is None:
        raise FreezeError("cannot resolve an exact source Git commit")
    manifest = baseline_dir / "manifest.json"
    try:
        pending = json.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise FreezeError("pending baseline manifest is absent or invalid") from exc
    if not isinstance(pending, dict) or pending.get("state") != "pending-live-freeze":
        raise FreezeError("source commit must carry the pending-live-freeze manifest")
    committed = _run("git", "show", f"HEAD:{manifest.relative_to(REPO_ROOT).as_posix()}")
    if committed.returncode != 0:
        raise FreezeError("pending manifest is not committed at source HEAD")
    try:
        committed_pending = json.loads(committed.stdout)
    except json.JSONDecodeError as exc:
        raise FreezeError("committed pending manifest is invalid") from exc
    if committed_pending != pending:
        raise FreezeError("working pending manifest differs from source HEAD")
    timestamp = _run("git", "show", "-s", "--format=%cI", "HEAD")
    if timestamp.returncode != 0 or not timestamp.stdout.strip():
        raise FreezeError("cannot read deterministic source commit timestamp")
    return head.stdout.strip(), timestamp.stdout.strip()


def _scratch_name(value: str, label: str) -> str:
    if re.fullmatch(r"arc03_[a-z0-9_]{1,48}", value) is None:
        raise FreezeError(f"{label} must be an explicit arc03_* scratch database name")
    return value


async def _assert_database(conn: Any, expected: str, label: str) -> str:
    row = await conn.fetchrow(
        "SELECT current_database() AS database_name, "
        "current_setting('server_version_num') AS version_num"
    )
    if row is None or str(row["database_name"]) != expected:
        raise FreezeError(f"{label} DSN does not target its declared scratch database")
    version_num = str(row["version_num"])
    if not version_num.startswith("16") or not version_num.isdigit():
        raise FreezeError(f"{label} must run PostgreSQL 16")
    numeric = int(version_num)
    return f"{numeric // 10000}.{numeric % 10000}"


def _pg_dump_version(binary: str) -> str:
    result = _run(binary, "--version")
    match = re.search(r"\b16(?:\.\d+)*\b", result.stdout)
    if result.returncode != 0 or match is None:
        raise FreezeError("pg_dump 16 is required for deterministic baseline output")
    return match.group(0)


def normalize_pg_dump(raw: str) -> str:
    """Remove environment/session noise while retaining schema semantics."""
    lines: list[str] = []
    skip_patterns = (
        re.compile(r"^--"),
        re.compile(r"^\\"),
        re.compile(r"^SET\s+", re.I),
        re.compile(r"^SELECT pg_catalog\.set_config", re.I),
        re.compile(r"^CREATE EXTENSION\b", re.I),
        re.compile(r"^COMMENT ON EXTENSION\b", re.I),
        re.compile(r"^CREATE SCHEMA\b", re.I),
        re.compile(r"^ALTER SCHEMA\b.*\bOWNER TO\b", re.I),
    )
    for raw_line in raw.replace("\r\n", "\n").splitlines():
        line = raw_line.rstrip()
        if any(pattern.search(line) for pattern in skip_patterns):
            continue
        if not line and (not lines or not lines[-1]):
            continue
        lines.append(line)
    while lines and not lines[-1]:
        lines.pop()
    body = "\n".join(lines) + "\n"
    forbidden = (
        "OWNER TO",
        "GRANT ",
        "REVOKE ",
        "CREATE DATABASE",
        "DROP DATABASE",
        *LEDGER_NAMES,
    )
    upper = body.upper()
    bad = [token for token in forbidden if token.upper() in upper]
    if bad:
        raise FreezeError(f"normalized init contains forbidden dump constructs: {sorted(bad)}")
    if "CREATE TABLE" not in upper:
        raise FreezeError("normalized init contains no tables")
    return (
        "-- Generated by scripts/database/freeze_baseline.py from converged scratch PG.\n"
        "-- roles.sql and extensions.sql run before this owner-executed file.\n\n"
        "SET LOCAL check_function_bodies = false;\n\n"
        + body
    )


LEDGER_NAMES = (
    "platform_schema_baselines",
    "platform_schema_changes",
    "platform_schema_change_attempts",
    "schema_migrations",
    "schema_migrations_meta",
)


_PG_QUERY_ENV = {
    "application_name": "PGAPPNAME",
    "channel_binding": "PGCHANNELBINDING",
    "connect_timeout": "PGCONNECT_TIMEOUT",
    "gssencmode": "PGGSSENCMODE",
    "options": "PGOPTIONS",
    "sslcert": "PGSSLCERT",
    "sslcrl": "PGSSLCRL",
    "sslkey": "PGSSLKEY",
    "sslmode": "PGSSLMODE",
    "sslrootcert": "PGSSLROOTCERT",
    "target_session_attrs": "PGTARGETSESSIONATTRS",
}


def _pg_dump_environment(dsn: str) -> dict[str, str]:
    """Translate a PostgreSQL URI into libpq variables without exposing it in argv."""
    try:
        parsed = urlsplit(dsn)
        port = parsed.port
    except ValueError as exc:
        raise FreezeError("source DSN is not a valid PostgreSQL URI") from exc
    if parsed.scheme not in {"postgres", "postgresql"}:
        raise FreezeError("source DSN must be a postgres:// or postgresql:// URI")
    database = unquote(parsed.path.removeprefix("/"))
    if not database:
        raise FreezeError("source DSN must name a database")

    environment = os.environ.copy()
    for name in {
        "PGAPPNAME",
        "PGCHANNELBINDING",
        "PGCONNECT_TIMEOUT",
        "PGDATABASE",
        "PGGSSENCMODE",
        "PGHOST",
        "PGOPTIONS",
        "PGPASSWORD",
        "PGPORT",
        "PGSSLCERT",
        "PGSSLCRL",
        "PGSSLKEY",
        "PGSSLMODE",
        "PGSSLROOTCERT",
        "PGTARGETSESSIONATTRS",
        "PGUSER",
    }:
        environment.pop(name, None)
    environment["PGDATABASE"] = database
    if parsed.hostname is not None:
        environment["PGHOST"] = unquote(parsed.hostname)
    if port is not None:
        environment["PGPORT"] = str(port)
    if parsed.username is not None:
        environment["PGUSER"] = unquote(parsed.username)
    if parsed.password is not None:
        environment["PGPASSWORD"] = unquote(parsed.password)
    for key, value in parse_qsl(parsed.query, keep_blank_values=True):
        env_name = _PG_QUERY_ENV.get(key)
        if env_name is not None:
            environment[env_name] = value
    return environment


def _dump_schema(dsn: str, pg_dump: str) -> str:
    environment = _pg_dump_environment(dsn)
    args = [
        pg_dump,
        "--schema-only",
        "--no-owner",
        "--no-privileges",
        "--no-comments",
        "--no-security-labels",
        "--no-publications",
        "--no-subscriptions",
    ]
    for schema in ("public", "gateway", "assistant", "knowledge"):
        args.extend(("--schema", schema))
    for table in LEDGER_NAMES:
        args.extend(("--exclude-table", f"public.{table}"))
    result = subprocess.run(
        args,
        cwd=REPO_ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise FreezeError("pg_dump failed; inspect a secret-safe local invocation")
    return normalize_pg_dump(result.stdout)


def _literal(value: object, *, array: bool = False) -> str:
    if value is None:
        return "NULL"
    if array:
        if not isinstance(value, (list, tuple)):
            raise FreezeError("reference array column returned a non-array value")
        values = ", ".join(_literal(str(item)) for item in value)
        return f"ARRAY[{values}]::varchar(100)[]"
    if isinstance(value, bool):
        return "TRUE" if value else "FALSE"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, str):
        return "'" + value.replace("'", "''") + "'"
    raise FreezeError(f"unsupported reference-data value type {type(value).__name__}")


async def render_reference_data(conn: Any) -> str:
    statements = [
        "-- Generated by scripts/database/freeze_baseline.py; system-owned rows only.",
        "-- Tenant/provider/model/rate-limit/admin-editable data is deliberately excluded.",
        "",
    ]
    for spec in REFERENCE_SPECS:
        columns = ", ".join(spec.columns)
        order = ", ".join(spec.natural_key)
        rows = await conn.fetch(
            f"SELECT {columns} FROM {spec.table} WHERE {spec.where} ORDER BY {order}"
        )
        if not rows:
            raise FreezeError(f"reference dataset {spec.table} has no protected system rows")
        rendered_rows = []
        seen: set[tuple[object, ...]] = set()
        for row in rows:
            key = tuple(row[column] for column in spec.natural_key)
            if key in seen:
                raise FreezeError(f"reference dataset {spec.table} has duplicate natural keys")
            seen.add(key)
            rendered_rows.append(
                "(" + ", ".join(
                    _literal(row[column], array=column in spec.array_columns)
                    for column in spec.columns
                ) + ")"
            )
        statements.extend(
            (
                f"INSERT INTO {spec.table} ({columns}) VALUES",
                "    " + ",\n    ".join(rendered_rows) + ";",
                "",
            )
        )
    return "\n".join(statements).rstrip() + "\n"


def _policy_bytes(source_sha: str) -> dict[str, bytes]:
    inventory, ownership, grants = database_policy.build(source_sha)
    return {
        "data-access-inventory.json": database_policy._serialized(inventory),
        "ownership-policy.json": database_policy._serialized(ownership),
        "grants-policy.json": database_policy._serialized(grants),
    }


def _manifest(
    *,
    baseline_dir: Path,
    source_sha: str,
    generated_at: str,
    postgres_version: str,
    pg_dump_version: str,
    fingerprints: dict[str, str],
    legacy_freeze_point: str,
    artifacts: dict[str, bytes],
) -> bytes:
    files_sha = {
        name: hashlib.sha256(artifacts[name]).hexdigest() for name in SQL_FILES
    }
    policy_sha = {
        name: hashlib.sha256(artifacts[name]).hexdigest() for name in POLICY_FILES
    }
    payload = {
        "schema": BASELINE_MANIFEST_SCHEMA,
        "state": BASELINE_STATE_FROZEN,
        "baseline_id": baseline_dir.name,
        "schema_revision": str(int(legacy_freeze_point[:3])),
        "source_git_sha": source_sha,
        "last_legacy_change": legacy_freeze_point,
        "structural_sha256": fingerprints["structural"],
        "acl_sha256": fingerprints["acl"],
        "extensions_sha256": fingerprints["extensions"],
        "reference_data_sha256": fingerprints["reference_data"],
        "generator": "scripts/database/freeze_baseline.py",
        "generator_sha256": file_sha256(Path(__file__)),
        "generated_at": generated_at,
        "postgres_version": postgres_version,
        "pg_dump_version": pg_dump_version,
        "files_sha256": files_sha,
        "policy_files_sha256": policy_sha,
        "reference_data": [spec.manifest_entry() for spec in REFERENCE_SPECS],
    }
    return (json.dumps(payload, indent=2, sort_keys=True) + "\n").encode()


def _write_staging(root: Path, artifacts: dict[str, bytes], manifest: bytes) -> Path:
    database_dir = root / "database"
    baseline = database_dir / "baselines" / DEFAULT_BASELINE_ID
    baseline.mkdir(parents=True)
    bootstrap = database_dir / "bootstrap"
    shutil.copytree(REPO_ROOT / "database/bootstrap", bootstrap)
    for name, content in artifacts.items():
        (baseline / name).write_bytes(content)
    (baseline / "manifest.json").write_bytes(manifest)
    return database_dir


def _publish(baseline_dir: Path, artifacts: dict[str, bytes], manifest: bytes) -> None:
    with tempfile.TemporaryDirectory(prefix="arc03-publish-") as temporary:
        staged = Path(temporary)
        for name, content in artifacts.items():
            (staged / name).write_bytes(content)
        (staged / "manifest.json").write_bytes(manifest)
        for name in (*POLICY_FILES, *SQL_FILES):
            os.replace(staged / name, baseline_dir / name)
        # Activation marker is deliberately last.
        os.replace(staged / "manifest.json", baseline_dir / "manifest.json")


async def freeze(args: argparse.Namespace) -> None:
    import asyncpg

    baseline_dir = args.baseline_dir.resolve()
    expected_dir = (
        REPO_ROOT / "database/baselines" / DEFAULT_BASELINE_ID
    ).resolve()
    if baseline_dir != expected_dir:
        raise FreezeError(f"output must be the canonical {DEFAULT_BASELINE_ID} baseline")
    source_sha, generated_at = _source_commit(baseline_dir)
    source_database = _scratch_name(args.source_database, "source database")
    verify_database = _scratch_name(args.verify_database, "verify database")
    if source_database == verify_database:
        raise FreezeError("source and verification databases must be distinct")

    source_dsn = _required_env(SOURCE_DSN_ENV)
    verify_admin_dsn = _required_env(VERIFY_ADMIN_DSN_ENV)
    verify_migrator_dsn = _required_env(VERIFY_MIGRATOR_DSN_ENV)
    configured_dsns = role_dsns(os.environ)
    if configured_dsns[ADMIN] != verify_admin_dsn:
        raise FreezeError(
            f"{DSN_ENV_BY_PRINCIPAL[ADMIN]} must match {VERIFY_ADMIN_DSN_ENV}"
        )
    if configured_dsns[MIGRATOR] != verify_migrator_dsn:
        raise FreezeError(
            f"{DSN_ENV_BY_PRINCIPAL[MIGRATOR]} must match {VERIFY_MIGRATOR_DSN_ENV}"
        )
    if source_dsn in set(configured_dsns.values()):
        raise FreezeError("source DSN must not be reused by verification roles")

    pg_dump = shutil.which(args.pg_dump)
    if pg_dump is None:
        raise FreezeError("pg_dump executable is unavailable")
    dump_version = _pg_dump_version(pg_dump)
    policies = _policy_bytes(source_sha)
    inventory = policies["data-access-inventory.json"]
    ownership = policies["ownership-policy.json"]
    grants = generate_database_grants.generate_grants_sql(
        inventory,
        policies["grants-policy.json"],
    ).encode()
    policy_objects = render_baseline_contract.load_policy_bytes(inventory, ownership)
    grant_matrix = render_baseline_contract.load_grant_matrix(
        inventory,
        policies["grants-policy.json"],
    )
    cutover_path = baseline_dir / "cutover_convergence.sql"
    cutover = render_baseline_contract.replace_relocation(
        cutover_path.read_text(encoding="utf-8"),
        render_baseline_contract.render_relocation(policy_objects),
    ).encode()
    verify = render_baseline_contract.render_verify(policy_objects, grant_matrix).encode()

    source = await asyncpg.connect(
        source_dsn,
        server_settings={"application_name": "arc03_baseline_freeze_source"},
    )
    admin = await asyncpg.connect(
        verify_admin_dsn,
        server_settings={"application_name": "arc03_baseline_freeze_role_admin"},
    )
    try:
        postgres_version = await _assert_database(source, source_database, "source")
        await _assert_database(admin, verify_database, "verify admin")
        await provision_roles_admin(
            admin,
            AuthorityPaths(REPO_ROOT / "database"),
            DEFAULT_ROLE_PREFIX,
        )
        await provision_extensions_admin(
            admin,
            AuthorityPaths(REPO_ROOT / "database"),
        )
        with tempfile.TemporaryDirectory(prefix="arc03-verify-sql-") as temporary:
            verify_path = Path(temporary) / "verify.sql"
            verify_path.write_bytes(verify)
            await verify_baseline_sql_file(source, verify_path)
        reference_sql = await render_reference_data(source)
        reference_sets = tuple(spec.fingerprint_spec() for spec in REFERENCE_SPECS)
        fingerprints = await compute_fingerprints(
            source,
            role_prefix=DEFAULT_ROLE_PREFIX,
            reference_sets=reference_sets,
        )
        init = _dump_schema(source_dsn, pg_dump).encode()
    finally:
        await source.close()
        await admin.close()

    artifacts = {
        **policies,
        "cutover_convergence.sql": cutover,
        "grants.sql": grants,
        "init.sql": init,
        "reference_data.sql": reference_sql.encode(),
        "verify.sql": verify,
    }
    legacy = load_legacy_manifest(REPO_ROOT / "database/migrations/legacy-manifest.yml")
    manifest = _manifest(
        baseline_dir=baseline_dir,
        source_sha=source_sha,
        generated_at=generated_at,
        postgres_version=postgres_version,
        pg_dump_version=dump_version,
        fingerprints=fingerprints,
        legacy_freeze_point=legacy.freeze_point,
        artifacts=artifacts,
    )

    with tempfile.TemporaryDirectory(prefix="arc03-fresh-verify-") as temporary:
        database_dir = _write_staging(Path(temporary), artifacts, manifest)
        frozen = load_baseline_manifest(
            database_dir / "baselines" / DEFAULT_BASELINE_ID / "manifest.json"
        )
        verify_conn = await asyncpg.connect(
            verify_migrator_dsn,
            server_settings={"application_name": "arc03_baseline_freeze_fresh"},
        )
        try:
            await _assert_database(verify_conn, verify_database, "verify migrator")
            reproduced = await fresh_install(
                verify_conn,
                AuthorityPaths(database_dir),
                frozen,
                hashlib.sha256(manifest).hexdigest(),
                role_prefix=DEFAULT_ROLE_PREFIX,
            )
            if reproduced != fingerprints:
                raise FreezeError("verification database fingerprints differ from source")
        finally:
            await verify_conn.close()
    await verify_role_connections(
        asyncpg,
        os.environ,
        role_prefix=DEFAULT_ROLE_PREFIX,
        expected_database=verify_database,
    )
    if not args.write:
        raise FreezeError("all live checks passed but --write was not supplied; no files changed")
    _publish(baseline_dir, artifacts, manifest)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--baseline-dir",
        type=Path,
        default=REPO_ROOT / "database/baselines" / DEFAULT_BASELINE_ID,
    )
    parser.add_argument("--source-database", required=True)
    parser.add_argument("--verify-database", required=True)
    parser.add_argument("--pg-dump", default="pg_dump")
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    try:
        asyncio.run(freeze(args))
    except (FreezeError, AuthorityError) as exc:
        print(f"BLOCKED: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
