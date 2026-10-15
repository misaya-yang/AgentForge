from __future__ import annotations

import subprocess
import sys
from types import ModuleType

import pytest

from database import cli, migrate_per_service, run_migration


def test_legacy_runner_refuses_before_reading_dsn(monkeypatch):
    monkeypatch.setenv("DATABASE_URL", "postgresql://user:secret-value@db/private")

    with pytest.raises(
        run_migration.RetiredMigrationRunnerError,
        match="retired and cannot execute SQL",
    ):
        run_migration.get_dsn()


async def test_legacy_runner_programmatic_surface_never_executes_sql():
    with pytest.raises(
        run_migration.RetiredMigrationRunnerError,
        match="use `make migrate`",
    ):
        await run_migration.run_migration(
            "database/migrations/100_kb_dataset_query_telemetry.sql",
            "postgresql://user:secret-value@db/private",
        )


def test_legacy_runner_cli_fails_closed_for_every_argument(tmp_path):
    sql_file = tmp_path / "single.sql"
    sql_file.write_text("SELECT pg_sleep(60);\n")

    result = subprocess.run(
        [sys.executable, "database/run_migration.py", str(sql_file)],
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 2
    assert "retired and cannot execute SQL" in result.stderr
    assert "pg_sleep" not in result.stdout + result.stderr


def test_per_service_runner_uses_authority_dsn_resolver(monkeypatch):
    monkeypatch.setattr(
        migrate_per_service,
        "get_dsn",
        lambda: "postgresql://authority.example/gateway",
    )

    assert migrate_per_service._dsn() == "postgresql://authority.example/gateway"


def test_cli_get_dsn_requires_explicit_migrator_url(monkeypatch):
    monkeypatch.setenv(
        "AI_GATEWAY_DATABASE_MIGRATOR_DSN",
        "postgresql://migrator.example/test",
    )
    monkeypatch.setenv("DATABASE_URL", "postgresql://shared.example/test")

    assert cli.get_dsn() == "postgresql://migrator.example/test"


def test_cli_get_dsn_fails_closed_without_hardcoded_password(monkeypatch, capsys):
    monkeypatch.delenv("DATABASE_URL", raising=False)
    monkeypatch.delenv("GATEWAY_DATABASE__DSN", raising=False)
    monkeypatch.delenv("AI_GATEWAY_DATABASE_MIGRATOR_DSN", raising=False)

    fake_settings = ModuleType("src.config.settings")

    class ExplodingSettings:
        def __init__(self):
            raise RuntimeError("postgresql://user:secret-value@db.example/private")

    fake_settings.Settings = ExplodingSettings
    monkeypatch.setitem(sys.modules, "src.config.settings", fake_settings)

    with pytest.raises(SystemExit) as exc_info:
        cli.get_dsn()

    assert exc_info.value.code == 2
    captured = capsys.readouterr().err
    assert "secret-value" not in captured
    assert "postgres:postgres" not in captured


@pytest.mark.parametrize(
    ("dsn", "expected"),
    [
        (
            "postgresql://user:secret@db.example:5432/gateway",
            "postgresql://user:******@db.example:5432/gateway",
        ),
        (
            "postgresql://user:p@ss:word@db.example:5432/gateway",
            "postgresql://user:******@db.example:5432/gateway",
        ),
        (
            "postgresql://localhost:5432/gateway",
            "postgresql://localhost:5432/gateway",
        ),
    ],
)
def test_cli_mask_dsn_hides_password_with_special_characters(dsn, expected):
    assert cli.mask_dsn(dsn) == expected
    assert "p@ss:word" not in cli.mask_dsn(dsn)
    assert "secret" not in cli.mask_dsn(dsn)
