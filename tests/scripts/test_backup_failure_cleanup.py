from __future__ import annotations

import os
import stat
import subprocess
from pathlib import Path


def test_backup_removes_partial_file_when_pg_dump_pipeline_fails(tmp_path: Path) -> None:
    script_dir = tmp_path / "scripts" / "new"
    script_dir.mkdir(parents=True)
    backup_script = script_dir / "backup.sh"
    backup_script.write_text(Path("scripts/new/backup.sh").read_text())
    (script_dir / "common.sh").write_text(
        "#!/bin/bash\n"
        "set -euo pipefail\n"
        'SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[1]:-${BASH_SOURCE[0]}}")" && pwd)"\n'
        'PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"\n'
        'DEFAULT_ENV_FILE="$PROJECT_ROOT/.env"\n'
        'ENV_FILE="${ENV_FILE:-$DEFAULT_ENV_FILE}"\n'
        'log_info() { echo "[INFO] $1"; }\n'
        'log_success() { echo "[OK] $1"; }\n'
        'log_warn() { echo "[WARN] $1"; }\n'
        'log_error() { echo "[ERROR] $1"; }\n'
        'log_step() { echo "=> $1"; }\n'
        'require_env_file() { [ -f "$ENV_FILE" ]; }\n'
        "load_env() { :; }\n"
        "pg_container() { echo fake-postgres; }\n"
        "pg_user() { echo postgres; }\n"
        "pg_database() { echo gateway; }\n"
    )

    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    fake_docker = fake_bin / "docker"
    fake_docker.write_text("#!/bin/sh\nprintf 'partial SQL dump\\n'\nexit 42\n")
    fake_docker.chmod(fake_docker.stat().st_mode | stat.S_IXUSR)
    env_file = tmp_path / "test.env"
    env_file.write_text("")
    external_backups = tmp_path.parent / f"{tmp_path.name}-external-backups"

    result = subprocess.run(
        ["bash", str(backup_script), "--env", str(env_file)],
        text=True,
        capture_output=True,
        env={
            **os.environ,
            "AI_PLATFORM_BACKUP_DIR": str(external_backups),
            "PATH": f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
        },
        check=False,
    )

    output = result.stdout + result.stderr
    assert not list(external_backups.glob("*.sql.gz"))
    assert not (tmp_path / "backups").exists()
    assert result.returncode == 1, output
    assert "backup aborted" in output


def test_backup_writes_private_file_outside_checkout(tmp_path: Path) -> None:
    script_dir = tmp_path / "scripts" / "new"
    script_dir.mkdir(parents=True)
    backup_script = script_dir / "backup.sh"
    backup_script.write_text(Path("scripts/new/backup.sh").read_text())
    (script_dir / "common.sh").write_text(
        "#!/bin/bash\n"
        "set -euo pipefail\n"
        'SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[1]:-${BASH_SOURCE[0]}}")" && pwd)"\n'
        'PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"\n'
        'DEFAULT_ENV_FILE="$PROJECT_ROOT/.env"\n'
        'ENV_FILE="${ENV_FILE:-$DEFAULT_ENV_FILE}"\n'
        'log_info() { echo "[INFO] $1"; }\n'
        'log_success() { echo "[OK] $1"; }\n'
        'log_warn() { echo "[WARN] $1"; }\n'
        'log_error() { echo "[ERROR] $1"; }\n'
        'log_step() { echo "=> $1"; }\n'
        'require_env_file() { [ -f "$ENV_FILE" ]; }\n'
        "load_env() { :; }\n"
        "pg_container() { echo fake-postgres; }\n"
        "pg_user() { echo postgres; }\n"
        "pg_database() { echo gateway; }\n"
    )

    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    fake_docker = fake_bin / "docker"
    fake_docker.write_text("#!/bin/sh\nprintf 'complete SQL dump\\n'\n")
    fake_docker.chmod(fake_docker.stat().st_mode | stat.S_IXUSR)
    env_file = tmp_path / "test.env"
    env_file.write_text("")
    external_backups = tmp_path.parent / f"{tmp_path.name}-private-backups"

    result = subprocess.run(
        ["bash", str(backup_script), "--env", str(env_file)],
        text=True,
        capture_output=True,
        env={
            **os.environ,
            "AI_PLATFORM_BACKUP_DIR": str(external_backups),
            "PATH": f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
        },
        check=False,
    )

    output = result.stdout + result.stderr
    backups = list(external_backups.glob("gateway_*.sql.gz"))
    assert result.returncode == 0, output
    assert len(backups) == 1
    assert stat.S_IMODE(backups[0].stat().st_mode) == 0o600
    assert not (tmp_path / "backups").exists()
