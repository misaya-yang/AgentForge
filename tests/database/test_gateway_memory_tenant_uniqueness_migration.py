from __future__ import annotations

from pathlib import Path


def test_gateway_memory_uniqueness_upgrade_is_scoped_and_idempotent() -> None:
    root = Path(__file__).resolve().parents[2]
    sql = (
        root / "database/migrations/099_gateway_memory_tenant_uniqueness.sql"
    ).read_text()

    assert "gateway.session_memory" in sql
    assert "UNIQUE (tenant_id, session_id, key)" in sql
    assert "gateway.user_memory" in sql
    assert "UNIQUE (tenant_id, user_id, key)" in sql
    assert "DROP TABLE" not in sql.upper()
    assert "TRUNCATE" not in sql.upper()
