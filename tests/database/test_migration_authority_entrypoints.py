from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_core.persistence import database as gateway_database
from knowledge_service.persistence import database as knowledge_database

from database import cli, migrate_per_service

ROOT = Path(__file__).resolve().parents[2]


async def test_legacy_python_cli_delegates_complete_plan(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[Any] = []

    async def fake_command(authority: Any) -> SimpleNamespace:
        calls.append(authority)
        return SimpleNamespace(exit_code=17)

    monkeypatch.setattr(cli, "get_dsn", lambda: "postgresql://authority.example/gateway")
    monkeypatch.setattr(cli, "command_migrate", fake_command)

    assert await cli.cmd_migrate() == 17
    assert len(calls) == 1
    assert calls[0].dsn == "postgresql://authority.example/gateway"


async def test_legacy_python_cli_rejects_selective_version_before_dsn(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        cli,
        "get_dsn",
        lambda: pytest.fail("selective migration must not inspect a DSN"),
    )

    with pytest.raises(cli.MigrationChainError, match="selective migration"):
        await cli.cmd_migrate("101")


def test_legacy_cli_names_translate_to_public_authority_cli(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls: list[list[str]] = []

    def fake_main(argv: list[str]) -> int:
        calls.append(argv)
        return 19

    monkeypatch.setattr(cli.authority_cli, "main", fake_main)

    assert cli.main(["init", "--baseline", "baseline-a"]) == 19
    assert cli.main(["status"]) == 19
    assert cli.main(["check"]) == 19
    assert calls == [
        ["init-fresh", "--baseline", "baseline-a"],
        ["status"],
        ["startup-check"],
    ]
    assert cli.main(["reset"]) == 2
    assert cli.main(["migrate", "101"]) == 2


async def test_per_service_name_delegates_full_plan_and_rejects_filter(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls: list[Any] = []

    async def fake_command(authority: Any) -> SimpleNamespace:
        calls.append(authority)
        return SimpleNamespace(exit_code=23)

    monkeypatch.setattr(
        migrate_per_service,
        "_dsn",
        lambda: "postgresql://authority.example/gateway",
    )
    monkeypatch.setattr(migrate_per_service, "command_migrate", fake_command)

    assert await migrate_per_service.main([]) == 23
    assert len(calls) == 1
    assert await migrate_per_service.main(["--service", "knowledge"]) == 2
    assert len(calls) == 1


@pytest.mark.parametrize(
    "relative_path",
    (
        "database/cli.py",
        "database/migrate_per_service.py",
        "scripts/new/migrate.sh",
    ),
)
def test_compatibility_entrypoints_contain_no_sql_execution_surface(
    relative_path: str,
) -> None:
    source = (ROOT / relative_path).read_text(encoding="utf-8")

    for forbidden in (
        "asyncpg.connect",
        "INSERT INTO public.schema_migrations",
        "CREATE TABLE IF NOT EXISTS public.schema_migrations",
        "DROP TABLE",
        "run_sql_file(",
    ):
        assert forbidden not in source


def test_runtime_manifests_run_authority_before_applications() -> None:
    compose = (ROOT / "docker-compose.yml").read_text(encoding="utf-8")
    migrate_dockerfile = (ROOT / "docker/migrate/Dockerfile").read_text(
        encoding="utf-8"
    )
    helm_job = (
        ROOT / "deploy/helm/ai-gateway/templates/migration-job.yaml"
    ).read_text(encoding="utf-8")
    helm_values = (ROOT / "deploy/helm/ai-gateway/values.yaml").read_text(
        encoding="utf-8"
    )

    assert 'entrypoint: ["python", "-m", "database.authority"]' in compose
    assert 'command: ["migrate"]' in compose
    assert "AI_GATEWAY_DATABASE_MIGRATOR_DSN" in compose
    assert 'GATEWAY_DATABASE__AUTO_INIT: "false"' in compose
    runtime = compose.split("  agent-runtime:", 1)[1]
    assert "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_RUNTIME_BASE_URL" in runtime
    assert compose.count("condition: service_completed_successfully") >= 6
    assert "profiles:" not in compose.split("  migrate:", 1)[1].split("  tempo:", 1)[0]
    volume_init = compose.split("  gateway-init:", 1)[1].split("  gateway:", 1)[0]
    assert "psql" not in volume_init
    assert "schema.sql" not in volume_init
    assert "database.authority" not in volume_init
    assert "COPY packages/ai-gateway-contracts/src/ai_gateway_contracts /app/ai_gateway_contracts" in migrate_dockerfile
    assert "COPY database /app/database" in migrate_dockerfile
    assert 'ENTRYPOINT ["python", "-m", "database.authority"]' in migrate_dockerfile
    assert "database.authority" in helm_job
    assert "src.services.database" not in helm_job
    assert "AUTO_INIT" not in helm_job
    assert ".Release.IsUpgrade" in helm_job
    assert '"helm.sh/hook": pre-upgrade' in helm_job
    assert "post-install" not in helm_job
    assert 'GATEWAY_DATABASE__AUTO_INIT: "false"' in helm_values

    gateway_template = (ROOT / "deploy/helm/ai-gateway/templates/gateway-deployment.yaml").read_text(encoding="utf-8")
    knowledge_template = (ROOT / "deploy/helm/ai-gateway/templates/knowledge-service-deployment.yaml").read_text(encoding="utf-8")
    assert 'command: ["python", "-m", "database.authority", "startup-check"]' in gateway_template
    assert "AI_GATEWAY_DATABASE_GATEWAY_DSN" in gateway_template
    assert '"startup-check", "--principal", "knowledge_api"' in knowledge_template
    assert "AI_GATEWAY_DATABASE_KNOWLEDGE_API_DSN" in knowledge_template
    assert "AI_GATEWAY_DATABASE_MIGRATOR_DSN" in helm_job


def test_distribution_includes_the_public_authority_cli() -> None:
    pyproject = (ROOT / "pyproject.toml").read_text(encoding="utf-8")
    dockerfile = (ROOT / "Dockerfile").read_text(encoding="utf-8")

    assert 'ai-gateway-db = "database.cli:main"' in pyproject
    assert '"database" = "database"' in pyproject
    assert '"/database"' in pyproject
    assert "COPY database/ ./database/" in dockerfile


@pytest.mark.parametrize(
    "storage_class",
    (gateway_database.DatabaseStorage, knowledge_database.DatabaseStorage),
)
async def test_application_execute_schema_surface_fails_closed(
    storage_class: type[Any],
) -> None:
    storage = storage_class.__new__(storage_class)
    storage._pool = object()

    with pytest.raises(RuntimeError, match="database.authority migrate"):
        await storage.execute_schema("database/schema.sql")


def test_setup_dev_never_pipes_schema_directly() -> None:
    source = (ROOT / "scripts/new/setup-dev.sh").read_text(encoding="utf-8")
    init_db = source.split("init_db() {", 1)[1].split("\n}", 1)[0]

    assert "database/schema.sql" not in init_db
    assert "psql" not in init_db
    assert 'migrate.sh" --auto' in init_db
