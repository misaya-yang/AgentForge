"""Model capability profile migration contracts."""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MIGRATION = ROOT / "database" / "migrations" / "088_model_capability_profiles.sql"
QWEN_RESPONSES_MIGRATION = ROOT / "database" / "migrations" / "091_qwen_responses_default.sql"
SCHEMA = ROOT / "database" / "schema.sql"


def _table_block(schema: str, table_name: str) -> str:
    """Return the full ``CREATE TABLE ... <name> (...)`` body from schema.sql."""
    marker = f"CREATE TABLE IF NOT EXISTS {table_name} ("
    start = schema.find(marker)
    assert start != -1, f"{table_name} table missing from schema.sql"
    depth = 0
    for index in range(start, len(schema)):
        if schema[index] == "(":
            depth += 1
        elif schema[index] == ")":
            depth -= 1
            if depth == 0:
                return schema[start : index + 1]
    raise AssertionError(f"unbalanced CREATE TABLE for {table_name}")


def test_model_capability_profile_migration_is_additive_and_idempotent() -> None:
    migration = MIGRATION.read_text(encoding="utf-8")
    schema = SCHEMA.read_text(encoding="utf-8")
    upper = migration.upper()

    for column in (
        "catalog_capabilities",
        "capability_overrides",
        "capability_revision",
    ):
        assert f"ADD COLUMN IF NOT EXISTS {column}" in migration
        assert column in schema
    assert "jsonb_typeof(catalog_capabilities) = 'object'" in migration
    assert "jsonb_typeof(capability_overrides) = 'object'" in migration
    assert "capability_revision > 0" in migration
    assert "DROP TABLE" not in upper
    assert "TRUNCATE" not in upper
    assert "DELETE FROM" not in upper


def test_schema_sql_matches_migration_end_state() -> None:
    """schema.sql must place the capability columns where migration 088 puts them.

    A fresh database built from schema.sql must be equivalent to a database
    migrated to 088: the columns and CHECK constraints belong to ``llm_models``
    (the capability owner), not to ``model_pricing``.
    """
    schema = SCHEMA.read_text(encoding="utf-8")
    llm_models = _table_block(schema, "llm_models")
    model_pricing = _table_block(schema, "model_pricing")

    for column in (
        "catalog_capabilities JSONB NOT NULL DEFAULT '{}'::jsonb",
        "capability_overrides JSONB NOT NULL DEFAULT '{}'::jsonb",
        "capability_revision BIGINT NOT NULL DEFAULT 1",
    ):
        assert column in llm_models, f"llm_models is missing: {column}"

    for constraint in (
        "CONSTRAINT llm_models_catalog_capabilities_object",
        "CONSTRAINT llm_models_capability_overrides_object",
        "CONSTRAINT llm_models_capability_revision_positive",
    ):
        assert constraint in llm_models, f"llm_models is missing: {constraint}"
    assert "jsonb_typeof(catalog_capabilities) = 'object'" in llm_models
    assert "jsonb_typeof(capability_overrides) = 'object'" in llm_models
    assert "capability_revision > 0" in llm_models

    for column in ("catalog_capabilities", "capability_overrides", "capability_revision"):
        assert column not in model_pricing, (
            f"model_pricing must not carry capability column: {column}"
        )


def test_qwen_responses_default_migration_is_narrow_and_idempotent() -> None:
    migration = QWEN_RESPONSES_MIGRATION.read_text(encoding="utf-8")
    upper = migration.upper()

    assert "tenant_id = 'default'" in migration
    assert "provider_id = 'dashscope'" in migration
    assert "'\"responses_v1\"'::jsonb" in migration
    assert "IS DISTINCT FROM 'responses_v1'" in migration
    assert "DROP TABLE" not in upper
    assert "TRUNCATE" not in upper
    assert "DELETE FROM" not in upper
