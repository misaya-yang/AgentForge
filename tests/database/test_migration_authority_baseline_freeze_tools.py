from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
BASELINE = ROOT / "database/baselines/2026_08_post_kb_v1"
sys.path.insert(0, str(ROOT / "scripts/inventory"))
sys.path.insert(0, str(ROOT / "scripts/database"))

import data_access  # noqa: E402
import freeze_baseline  # noqa: E402
import generate_database_grants  # noqa: E402
import render_baseline_contract  # noqa: E402

from database.authority.commands import baseline_ready  # noqa: E402
from database.authority.runner import AuthorityPaths  # noqa: E402


def test_frozen_baseline_is_bound_and_activates() -> None:
    manifest = json.loads((BASELINE / "manifest.json").read_text(encoding="utf-8"))

    assert manifest["state"] == "frozen"
    assert len(manifest["source_git_sha"]) == 40
    for field in (
        "structural_sha256",
        "acl_sha256",
        "extensions_sha256",
        "reference_data_sha256",
    ):
        assert len(manifest[field]) == 64
    assert baseline_ready(AuthorityPaths(ROOT / "database"))
    for field, filenames in (
        ("files_sha256", freeze_baseline.SQL_FILES),
        ("policy_files_sha256", freeze_baseline.POLICY_FILES),
    ):
        assert set(manifest[field]) == set(filenames)
        for filename in filenames:
            assert manifest[field][filename] == hashlib.sha256(
                (BASELINE / filename).read_bytes()
            ).hexdigest()


def test_static_policy_and_sql_regenerate_exactly() -> None:
    manifest = json.loads((BASELINE / "manifest.json").read_text(encoding="utf-8"))
    # Epochs deliberately change the live read/write inventory. Reproduce the
    # immutable baseline from its hash-bound policy, not today's source labelled
    # with an old Git SHA. The test above checks every policy file's frozen hash.
    generated = {
        filename: (BASELINE / filename).read_bytes()
        for filename in ("data-access-inventory.json", "ownership-policy.json", "grants-policy.json")
    }
    for filename, content in generated.items():
        assert hashlib.sha256(content).hexdigest() == manifest["policy_files_sha256"][filename]

    grants = generate_database_grants.generate_grants_sql(
        generated["data-access-inventory.json"],
        generated["grants-policy.json"],
    )
    assert (BASELINE / "grants.sql").read_text(encoding="utf-8") == grants
    objects = render_baseline_contract.load_policy_bytes(
        generated["data-access-inventory.json"],
        generated["ownership-policy.json"],
    )
    matrix = render_baseline_contract.load_grant_matrix(
        generated["data-access-inventory.json"],
        generated["grants-policy.json"],
    )
    cutover = (BASELINE / "cutover_convergence.sql").read_text(encoding="utf-8")
    assert render_baseline_contract.replace_relocation(
        cutover,
        render_baseline_contract.render_relocation(objects),
    ) == cutover
    assert render_baseline_contract.render_verify(objects, matrix) == (
        BASELINE / "verify.sql"
    ).read_text(encoding="utf-8")

    sql_inputs = {path.relative_to(ROOT).as_posix() for path in data_access._sql_files()}
    assert "database/schema.sql" in sql_inputs
    assert all(
        path == "database/schema.sql" or path.startswith("database/migrations/")
        for path in sql_inputs
    )
    assert not any(path.startswith("database/baselines/") for path in sql_inputs)

    # Three authority/legacy ledgers are deliberately outside business-object
    # relocation, leaving 163 managed tables from the 166-table inventory.
    assert len([item for item in objects if item.kind == "table"]) == 163
    assert len([item for item in objects if item.kind == "function"]) == 45
    assert len([item for item in objects if item.kind == "sequence"]) == 26
    assert len([item for item in objects if item.kind == "view"]) == 2

    relocation = render_baseline_contract.render_relocation(objects)
    table_order = relocation.index("WHEN 'table' THEN 0")
    view_order = relocation.index("WHEN 'view' THEN 1")
    sequence_order = relocation.index("WHEN 'sequence' THEN 2")
    assert table_order < view_order < sequence_order
    assert "oidvectortypes(procedure.proargtypes)" in relocation
    assert "pg_get_function_identity_arguments" not in relocation

    rendered_verify = render_baseline_contract.render_verify(objects, matrix)
    assert "oidvectortypes(procedure.proargtypes)" in rendered_verify
    assert "pg_get_function_identity_arguments" not in rendered_verify


def test_ownership_and_grant_inputs_fail_closed_on_drift() -> None:
    inventory = (BASELINE / "data-access-inventory.json").read_bytes()
    ownership = json.loads((BASELINE / "ownership-policy.json").read_bytes())
    ownership["objects"].pop()
    with pytest.raises(render_baseline_contract.RenderContractError, match="coverage mismatch"):
        render_baseline_contract.load_policy_bytes(
            inventory,
            (json.dumps(ownership, sort_keys=True) + "\n").encode(),
        )

    grants = json.loads((BASELINE / "grants-policy.json").read_bytes())
    grants["inventory_sha256"] = "0" * 64
    with pytest.raises(generate_database_grants.GrantContractError, match="does not bind"):
        generate_database_grants.generate_grants_sql(
            inventory,
            (json.dumps(grants, sort_keys=True) + "\n").encode(),
        )


def test_pg_dump_normalization_rejects_acl_and_ledger_bypass() -> None:
    normalized = freeze_baseline.normalize_pg_dump(
        "-- noise\n"
        "\\restrict random\n"
        "SET search_path = public;\n"
        "CREATE SCHEMA gateway;\n"
        "CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA public;\n"
        "CREATE TABLE gateway.widgets (id integer);\n"
        "\\unrestrict random\n"
    )
    assert "random" not in normalized
    assert "CREATE SCHEMA" not in normalized
    assert "CREATE EXTENSION" not in normalized
    assert "CREATE TABLE gateway.widgets" in normalized
    assert "SET LOCAL check_function_bodies = false;" in normalized

    for forbidden in (
        "CREATE TABLE gateway.widgets (id integer);\nGRANT SELECT ON gateway.widgets TO x;\n",
        "CREATE TABLE public.schema_migrations (version text);\n",
    ):
        with pytest.raises(freeze_baseline.FreezeError, match="forbidden"):
            freeze_baseline.normalize_pg_dump(forbidden)


def test_pg_dump_uses_secret_free_argv_and_libpq_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: dict[str, object] = {}

    def fake_run(args: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        captured["args"] = args
        captured["env"] = kwargs["env"]
        return subprocess.CompletedProcess(
            args,
            0,
            stdout="CREATE TABLE public.widgets (id integer);\n",
            stderr="",
        )

    monkeypatch.setattr(freeze_baseline.subprocess, "run", fake_run)

    result = freeze_baseline._dump_schema(
        "postgresql://alice:s3cret@db.example:5544/sample?sslmode=require",
        "/usr/bin/pg_dump",
    )

    args = captured["args"]
    environment = captured["env"]
    assert isinstance(args, list)
    assert isinstance(environment, dict)
    assert not any("s3cret" in argument for argument in args)
    assert environment["PGHOST"] == "db.example"
    assert environment["PGPORT"] == "5544"
    assert environment["PGDATABASE"] == "sample"
    assert environment["PGUSER"] == "alice"
    assert environment["PGPASSWORD"] == "s3cret"
    assert environment["PGSSLMODE"] == "require"
    assert "CREATE TABLE public.widgets" in result


def test_pg_dump_rejects_non_uri_source_dsn() -> None:
    with pytest.raises(freeze_baseline.FreezeError, match="postgres://"):
        freeze_baseline._pg_dump_environment("dbname=sample host=localhost")


def test_reference_policy_excludes_admin_editable_catalogs() -> None:
    tables = {spec.table for spec in freeze_baseline.REFERENCE_SPECS}

    assert tables == {
        "gateway.permissions",
        "gateway.rbac_roles",
        "gateway.role_permissions",
    }
    assert not any(
        name in table
        for table in tables
        for name in ("provider", "model", "rate_limit", "tenant", "user_roles")
    )
