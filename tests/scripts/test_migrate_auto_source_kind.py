from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest


@pytest.mark.parametrize(
    ("source_kind", "expected_commands"),
    [
        ("empty", ["source-kind", "migrate"]),
        ("adopted", ["source-kind", "migrate"]),
        ("ledgerless-platform", ["source-kind", "prepare-cutover-ownership", "migrate"]),
        (
            "tracked-legacy",
            ["source-kind", "migrate --no-adoption", "prepare-cutover-ownership", "migrate"],
        ),
    ],
)
def test_auto_migration_routes_each_source_without_guessing(
    tmp_path: Path,
    source_kind: str,
    expected_commands: list[str],
) -> None:
    script_dir = tmp_path / "scripts" / "new"
    script_dir.mkdir(parents=True)
    (script_dir / "migrate.sh").write_text(Path("scripts/new/migrate.sh").read_text())
    (script_dir / "common.sh").write_text(
        "#!/bin/bash\n"
        "set -euo pipefail\n"
        'PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"\n'
        "load_env() { :; }\n"
        "require_env_file() { :; }\n"
        'log_step() { :; }\n'
        'log_error() { echo "$1" >&2; }\n'
    )
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    command_log = tmp_path / "commands.log"
    fake_uv = fake_bin / "uv"
    fake_uv.write_text(
        "#!/bin/bash\n"
        'command_text="$*"\n'
        'case "$command_text" in\n'
        '  *" provision-roles") exit 0 ;;\n'
        '  *" source-kind") echo "$SOURCE_KIND"; echo source-kind >> "$COMMAND_LOG" ;;\n'
        '  *" prepare-cutover-ownership"*) echo prepare-cutover-ownership >> "$COMMAND_LOG" ;;\n'
        '  *" migrate --no-adoption") echo "migrate --no-adoption" >> "$COMMAND_LOG" ;;\n'
        '  *" migrate") echo migrate >> "$COMMAND_LOG" ;;\n'
        "esac\n"
    )
    fake_uv.chmod(0o755)

    result = subprocess.run(
        ["bash", str(script_dir / "migrate.sh"), "--auto"],
        cwd=tmp_path,
        env={
            **os.environ,
            "PATH": f"{fake_bin}:{os.environ['PATH']}",
            "SOURCE_KIND": source_kind,
            "COMMAND_LOG": str(command_log),
            "AI_GATEWAY_DATABASE_ADMIN_DSN": "postgresql://admin.invalid/db",
            "AI_GATEWAY_DATABASE_MIGRATOR_DSN": "postgresql://migrator.invalid/db",
        },
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert command_log.read_text().splitlines() == expected_commands
