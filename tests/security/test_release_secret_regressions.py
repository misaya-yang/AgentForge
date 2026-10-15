from __future__ import annotations

from pathlib import Path

from src.core.middleware.streaming import StreamingAdmissionConfig, StreamingRateLimitConfig

ROOT = Path(__file__).resolve().parents[2]


def test_account_migration_does_not_seed_known_admin_password() -> None:
    migration = (ROOT / "database/migrations/005_account_permission_system.sql").read_text()

    assert "Password: 123456.dc" not in migration
    assert "123456.dc" not in migration
    assert "$2b$12$ORXIEYVft.OQ5v55S6WiFukZGEk.1QkB/fElA.0IMM5shEpBoyhWC" not in migration
    assert "password_hash = EXCLUDED.password_hash" not in migration
    assert "force_password_change = EXCLUDED.force_password_change" not in migration


def test_helm_secret_template_requires_explicit_secret_values() -> None:
    template = (ROOT / "deploy/helm/ai-gateway/templates/secret.yaml").read_text()

    assert 'default "change-me"' not in template
    assert 'default "change-me-in-production"' not in template
    assert "required " in template


def test_database_cli_has_no_hardcoded_postgres_password() -> None:
    cli = (ROOT / "database/cli.py").read_text()

    assert "postgres:postgres" not in cli
    assert "gateway123" not in cli
    assert 'sys.exit(2)' in cli


def test_bootstrap_password_is_not_hardcoded_in_source() -> None:
    password_py = (ROOT / "src/core/auth/password.py").read_text()
    container = (ROOT / "src/container.py").read_text()

    assert "ChangeMe-Admin-2026!" not in password_py
    assert 'os.environ.get("DEFAULT_USER_PASSWORD", "")' in password_py
    assert 'os.environ.get("DEFAULT_USER_PASSWORD", "")' in container


def test_monitoring_compose_does_not_embed_credentials() -> None:
    compose = (ROOT / "docker/monitoring/docker-compose.monitoring.yml").read_text()

    assert "gateway123" not in compose
    assert "GF_SECURITY_ADMIN_PASSWORD=admin" not in compose
    assert "POSTGRES_PASSWORD: gateway123" not in compose
    assert "${POSTGRES_PASSWORD:?POSTGRES_PASSWORD is required}" in compose
    assert "${GRAFANA_ADMIN_PASSWORD:?GRAFANA_ADMIN_PASSWORD is required}" in compose
    assert '"127.0.0.1:' in compose


def test_local_published_ports_bind_loopback() -> None:
    compose = (ROOT / "docker-compose.yml").read_text()
    setup_dev = (ROOT / "scripts/new/setup-dev.sh").read_text()
    override = (ROOT / "docker-compose.override.yml.example").read_text()

    assert '"127.0.0.1:${GATEWAY_PORT:-8080}:8080"' in compose
    assert '"127.0.0.1:${FRONTEND_PORT:-8081}:80"' in compose
    assert '-p "127.0.0.1:${PG_PORT}:5432"' in setup_dev
    assert '-p "127.0.0.1:${REDIS_DEV_PORT}:6379"' in setup_dev
    assert "redis-cli -a $REDIS_PASS" not in setup_dev
    assert '"127.0.0.1:5432:5432"' in override


def test_prometheus_metrics_are_not_publicly_exposed() -> None:
    main_py = (ROOT / "src/main.py").read_text()
    ingress = (ROOT / "deploy/helm/ai-gateway/templates/ingress.yaml").read_text()
    whitelist = main_py.split("whitelist_paths=[", 1)[1].split("]", 1)[0]
    metrics_route = main_py.split('@app.get("/metrics"', 1)[1].split("metrics_collector", 1)[0]

    assert '"/metrics"' not in whitelist
    assert "path: /metrics" not in ingress
    assert "/metrics" not in StreamingRateLimitConfig().whitelist_paths
    assert "/metrics" not in StreamingAdmissionConfig().whitelist_paths
    assert "Depends(get_auth_context)" in metrics_route
    assert "Capability.GATEWAY_METRICS_READ" in metrics_route
