from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "harness" / "agent_runtime_rollback_rehearsal.sh"
MAKEFILE = ROOT / "Makefile"


def test_rollback_rehearsal_is_digest_pinned_and_volume_safe() -> None:
    source = SCRIPT.read_text(encoding="utf-8")

    assert 'verify_bundle_image agent-runtime "$rollback_runtime_image"' in source
    assert 'verify_bundle_image assistant-service "$rollback_assistant_image"' in source
    assert 'verify_bundle_image gateway "$rollback_gateway_image"' in source
    assert 'verify_bundle_image frontend "$rollback_frontend_image"' in source
    assert 'verify_bundle_image knowledge-service "$rollback_knowledge_image"' in source
    assert 'verify_bundle_image postgres "$rollback_postgres_image"' in source
    assert 'verify_bundle_image redis "$rollback_redis_image"' in source
    assert 'verify_bundle_image qdrant "$rollback_qdrant_image"' in source
    assert 'resolved rollback Compose does not match the frozen bundle' in source
    assert "assert_quiescent" in source
    assert "docker compose" in source
    assert " down " not in source
    assert "down -v" not in source
    assert "volume rm" not in source
    assert "schema downgrade" not in source.lower()


def test_rollback_rehearsal_uses_explicit_environment_and_restores_current() -> None:
    source = SCRIPT.read_text(encoding="utf-8")
    makefile = MAKEFILE.read_text(encoding="utf-8")

    assert "source \"$env_file\"" not in source
    assert "env -i HOME=" in source
    assert 'COMPOSE_PARALLEL_LIMIT="${COMPOSE_PARALLEL_LIMIT:-1}"' in source
    assert 'make -C "$repo_root" ENV_FILE="$env_file" hot-update ARGS="--all"' in source
    assert 'make -C "$repo_root" ENV_FILE="$env_file" migrate' in source
    assert "trap cleanup EXIT" in source
    assert 'ENV_FILE="$(ENV_FILE)" bash scripts/harness/agent_runtime_rollback_rehearsal.sh' in makefile


def test_rollback_fingerprint_covers_content_not_only_row_counts() -> None:
    source = SCRIPT.read_text(encoding="utf-8")

    assert "sessions_digest" in source
    assert "threads_digest" in source
    assert "items_digest" in source
    assert "capability_executions_digest" in source
    assert "session_set_sha256" in source
    assert "history_sha256" in source
    assert "migrations_digest" in source
    assert "schema_digest" in source


def test_restore_failure_is_not_silently_ignored() -> None:
    source = SCRIPT.read_text(encoding="utf-8")

    assert "restore_current || recovery_failed=1" in source
    assert "current release could not be restored" in source
    assert "exit 70" in source
