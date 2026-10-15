"""PostgreSQL proof for migration 101's restore-required boundary."""

from __future__ import annotations

import os
import shutil
import subprocess
import uuid
from pathlib import Path
from typing import Any

import asyncpg
import pytest
from dotenv import dotenv_values

ROOT = Path(__file__).resolve().parents[2]
MIGRATION_101 = ROOT / "database/migrations/101_kb_ingestion_lifecycle.sql"


def _postgres_config() -> dict[str, Any]:
    file_values = dotenv_values(os.environ.get("ENV_FILE") or ROOT / ".env")
    required = ("POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB", "POSTGRES_PORT")
    values = {key: os.environ.get(key) or file_values.get(key) for key in required}
    missing = [key for key, value in values.items() if not value]
    if missing:
        pytest.fail(f"local PostgreSQL test configuration missing keys: {', '.join(missing)}")
    return {
        "host": os.environ.get("POSTGRES_HOST") or file_values.get("POSTGRES_HOST") or "127.0.0.1",
        "port": int(str(values["POSTGRES_PORT"])),
        "user": str(values["POSTGRES_USER"]),
        "password": str(values["POSTGRES_PASSWORD"]),
        "database": str(values["POSTGRES_DB"]),
    }


def _postgres_client(name: str) -> str:
    if os.environ.get("POSTGRES_CLIENT_CONTAINER"):
        return name
    override = os.environ.get(f"POSTGRES_{name.upper()}_BIN")
    executable = override or shutil.which(name)
    if executable is None:
        pytest.fail(
            f"{name} is required by the migration-101 restore gate; "
            f"install the PostgreSQL client or set POSTGRES_{name.upper()}_BIN"
        )
    return executable


def _run_postgres_client(
    executable: str,
    config: dict[str, Any],
    database: str,
    *arguments: str,
) -> None:
    env = {**os.environ, "PGPASSWORD": str(config["password"])}
    container = os.environ.get("POSTGRES_CLIENT_CONTAINER")
    if container:
        client_arguments = list(arguments)
        input_bytes: bytes | None = None
        dump_path: Path | None = None
        if executable == "pg_dump":
            file_index = client_arguments.index("--file")
            dump_path = Path(client_arguments[file_index + 1])
            del client_arguments[file_index : file_index + 2]
        elif executable == "pg_restore":
            dump_path = Path(client_arguments.pop())
            input_bytes = dump_path.read_bytes()
        command = [
            "docker",
            "exec",
            "--interactive",
            "--env",
            "PGPASSWORD",
            container,
            executable,
            "--username",
            str(config["user"]),
            "--dbname",
            database,
            *client_arguments,
        ]
        result = subprocess.run(
            command,
            env=env,
            input=input_bytes,
            capture_output=True,
            check=False,
        )
        if result.returncode == 0 and executable == "pg_dump":
            assert dump_path is not None
            dump_path.write_bytes(result.stdout)
    else:
        command = [
            executable,
            "--host",
            str(config["host"]),
            "--port",
            str(config["port"]),
            "--username",
            str(config["user"]),
            "--dbname",
            database,
            *arguments,
        ]
        result = subprocess.run(
            command,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
    assert result.returncode == 0, (
        f"{Path(executable).name} failed with exit code {result.returncode}; "
        "credentials and client output are intentionally redacted"
    )


_N_MINUS_ONE_SCHEMA = """
CREATE TABLE datasets (
    dataset_id VARCHAR(255) PRIMARY KEY,
    tenant_id VARCHAR(255) NOT NULL DEFAULT ''
);
CREATE TABLE dataset_process_rules (
    id VARCHAR(255) PRIMARY KEY,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    mode VARCHAR(50) NOT NULL DEFAULT 'automatic',
    rules JSONB NOT NULL DEFAULT '{}'
);
CREATE TABLE documents (
    document_id VARCHAR(255) PRIMARY KEY,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    title VARCHAR(255) NOT NULL,
    status VARCHAR(50) NOT NULL DEFAULT 'uploaded',
    error TEXT,
    metadata JSONB NOT NULL DEFAULT '{}',
    process_rule_id VARCHAR(255),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);
CREATE TABLE segments (
    segment_id VARCHAR(255) PRIMARY KEY,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id) ON DELETE CASCADE,
    position INTEGER NOT NULL DEFAULT 0,
    text TEXT NOT NULL,
    content_type VARCHAR(50) NOT NULL DEFAULT 'text',
    status VARCHAR(50) DEFAULT 'completed',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    content_hash VARCHAR(64),
    index_node_id VARCHAR(255),
    index_node_hash VARCHAR(255),
    UNIQUE(document_id, position)
);
INSERT INTO datasets (dataset_id, tenant_id) VALUES ('dataset-a', 'tenant-a');
INSERT INTO dataset_process_rules (id, dataset_id) VALUES ('rule-a', 'dataset-a');
INSERT INTO documents (
    document_id, dataset_id, title, status, process_rule_id
) VALUES ('document-a', 'dataset-a', 'Document A', 'completed', 'rule-a');
INSERT INTO segments (
    segment_id, dataset_id, document_id, position, text, content_type
) VALUES ('segment-a', 'dataset-a', 'document-a', 0, 'original', 'text');
"""

_N_MINUS_ONE_UPSERT = """
INSERT INTO segments (
    segment_id, dataset_id, document_id, position, text, content_type
) VALUES ($1, 'dataset-a', 'document-a', 0, $2, 'text')
ON CONFLICT (document_id, position)
DO UPDATE SET text = EXCLUDED.text
"""


@pytest.mark.asyncio
async def test_101_requires_dump_restore_before_returning_to_n_minus_one(
    tmp_path: Path,
) -> None:
    pg_dump = _postgres_client("pg_dump")
    pg_restore = _postgres_client("pg_restore")
    config = _postgres_config()
    source_database = f"kb_101_source_{uuid.uuid4().hex}"
    restored_database = f"kb_101_restore_{uuid.uuid4().hex}"
    dump_path = tmp_path / "pre-101.dump"

    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{source_database}"')
    await admin.execute(f'CREATE DATABASE "{restored_database}"')
    source: asyncpg.Connection | None = None
    restored: asyncpg.Connection | None = None
    try:
        source = await asyncpg.connect(**{**config, "database": source_database})
        await source.execute(_N_MINUS_ONE_SCHEMA)
        await source.execute(_N_MINUS_ONE_UPSERT, "old-writer-before", "before")
        assert (
            await source.fetchval("SELECT text FROM segments WHERE document_id = 'document-a'")
            == "before"
        )

        _run_postgres_client(
            pg_dump,
            config,
            source_database,
            "--format=custom",
            "--no-owner",
            "--no-privileges",
            "--file",
            str(dump_path),
        )
        assert dump_path.stat().st_size > 0

        await source.execute(MIGRATION_101.read_text(encoding="utf-8"))
        with pytest.raises(asyncpg.InvalidColumnReferenceError):
            await source.execute(
                _N_MINUS_ONE_UPSERT,
                "old-writer-after",
                "must-not-write",
            )
        await source.execute(
            """
            INSERT INTO segments (
                segment_id, dataset_id, document_id, position, text, content_type
            ) VALUES (
                'new-writer-after', 'dataset-a', 'document-a', 0, 'after', 'text'
            )
            ON CONFLICT (document_id, content_type, position)
            DO UPDATE SET text = EXCLUDED.text
            """
        )
        assert (
            await source.fetchval("SELECT text FROM segments WHERE document_id = 'document-a'")
            == "after"
        )
        await source.close()
        source = None

        _run_postgres_client(
            pg_restore,
            config,
            restored_database,
            "--no-owner",
            "--no-privileges",
            "--exit-on-error",
            str(dump_path),
        )
        restored = await asyncpg.connect(**{**config, "database": restored_database})
        assert (
            await restored.fetchval(
                """
            SELECT count(*) FROM pg_constraint
            WHERE conrelid = 'public.segments'::regclass
              AND conname = 'segments_document_id_position_key'
            """
            )
            == 1
        )
        assert (
            await restored.fetchval("SELECT to_regclass('knowledge.document_pipeline_executions')")
            is None
        )
        await restored.execute(
            _N_MINUS_ONE_UPSERT,
            "old-writer-restored",
            "restored",
        )
        assert (
            await restored.fetchval("SELECT text FROM segments WHERE document_id = 'document-a'")
            == "restored"
        )
    finally:
        if source is not None:
            await source.close()
        if restored is not None:
            await restored.close()
        for database in (source_database, restored_database):
            await admin.execute(f'DROP DATABASE "{database}"')
        await admin.close()
