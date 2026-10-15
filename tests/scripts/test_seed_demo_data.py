from __future__ import annotations

import subprocess
from pathlib import Path

SCRIPT_PATH = Path("scripts/new/seed-demo-data.sh")
SQL_PATH = Path("examples/demo-data/open-source-demo.sql")


def test_demo_seed_dry_run_does_not_require_env_file() -> None:
    result = subprocess.run(
        ["bash", str(SCRIPT_PATH), "--dry-run"],
        text=True,
        capture_output=True,
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Dry run complete" in output
    assert "No database connection will be opened" in output
    assert "/knowledge/demo-kb-ai-gateway" in output
    assert "/share/demo-share" in output
    assert "/quiz/demo-quiz" in output


def test_demo_seed_sql_contains_idempotent_public_route_records() -> None:
    sql = SQL_PATH.read_text()

    assert "ON CONFLICT" in sql
    assert "demo-kb-ai-gateway" in sql
    assert "demo-share" in sql
    assert "demo-quiz" in sql
    assert "demo-assistant-session" in sql
