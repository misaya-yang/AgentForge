"""Contract tests for the gateway KB boundary gate (PRD T8.4)."""

from __future__ import annotations

import io
from contextlib import redirect_stdout
from pathlib import Path

import pytest

from scripts.harness import gateway_kb_boundary_gate as gate


def _make_fixture(tmp_path: Path, gateway_code: str, ks_code: str = "") -> Path:
    root = tmp_path
    gw = root / "src" / "api"
    gw.mkdir(parents=True, exist_ok=True)
    (gw / "routes.py").write_text(gateway_code, encoding="utf-8")
    if ks_code:
        ks = root / "apps" / "knowledge-service" / "src" / "knowledge_service" / "api" / "schemas"
        ks.mkdir(parents=True, exist_ok=True)
        (ks / "knowledge.py").write_text(ks_code, encoding="utf-8")
    return root


def _run(monkeypatch, root: Path) -> tuple[int, str]:
    monkeypatch.setattr(gate, "ROOT", root)
    monkeypatch.setattr(gate, "SCAN_ROOTS", (root / "src",))
    monkeypatch.setattr(gate, "KS_API_ROOT", root / "apps" / "knowledge-service" / "src" / "knowledge_service" / "api")
    monkeypatch.setattr(gate, "_KS_API_MODELS", None)
    buf = io.StringIO()
    with redirect_stdout(buf):
        code = gate.main()
    return code, buf.getvalue()


def test_real_gateway_tree_passes_the_gate() -> None:
    assert gate.main() == 0


def test_kb_table_sql_in_gateway_is_flagged(monkeypatch, tmp_path: Path) -> None:
    code, out = _run(
        monkeypatch,
        _make_fixture(
            tmp_path,
            "async def list_datasets(db):\n"
            "    return await db.fetch(\"SELECT * FROM datasets WHERE tenant_id = $1\")\n",
        ),
    )
    assert code == 1
    assert "rule 1" in out


def test_startup_migration_helper_sql_is_not_exempt(monkeypatch, tmp_path: Path) -> None:
    code, out = _run(
        monkeypatch,
        _make_fixture(
            tmp_path,
            "async def _openai_embedding_needs_migration(conn):\n"
            "    return await conn.fetch(\"SELECT 1 FROM datasets WHERE embedding_model LIKE 'openai%'\")\n",
        ),
    )
    assert code == 1, out


def test_authoring_validator_sql_is_not_exempt(monkeypatch, tmp_path: Path) -> None:
    code, out = _run(
        monkeypatch,
        _make_fixture(
            tmp_path,
            "async def authorized_dataset_ids(conn, ids):\n"
            "    return await conn.fetch('SELECT dataset_id FROM dataset_permissions')\n",
        ),
    )
    assert code == 1, out


@pytest.mark.parametrize(
    "sql",
    [
        "SELECT to_regclass('knowledge.datasets')",
        "SELECT 1 FROM information_schema.columns WHERE table_name = 'segments'",
        "ALTER TABLE document_summaries ADD COLUMN stale BOOLEAN",
        "CREATE INDEX idx_segment_images ON segment_images(segment_id)",
        'SELECT * FROM "knowledge"."datasets"',
    ],
)
def test_kb_metadata_and_ddl_blind_spots_are_caught(
    monkeypatch, tmp_path: Path, sql: str
) -> None:
    code, out = _run(
        monkeypatch,
        _make_fixture(tmp_path, f"def probe(db):\n    return '''{sql}'''\n"),
    )
    assert code == 1, out


def test_gateway_model_mirroring_ks_contract_is_flagged(monkeypatch, tmp_path: Path) -> None:
    # RagasJudgeSelector matches no KB name heuristic — only the KS mirror
    # rule can catch it, which is what makes the mirror check load-bearing.
    root = _make_fixture(
        tmp_path,
        "from pydantic import BaseModel\n\n\nclass RagasJudgeSelector(BaseModel):\n    model: str\n",
        "from pydantic import BaseModel\n\n\nclass RagasJudgeSelector(BaseModel):\n    model: str\n",
    )
    code, out = _run(monkeypatch, root)
    assert code == 1
    assert "mirrors a knowledge-service API" in out


def test_kb_request_schema_name_heuristic_fires_without_ks_mirror(monkeypatch, tmp_path: Path) -> None:
    root = _make_fixture(
        tmp_path,
        "from pydantic import BaseModel\n\n\nclass DatasetCreateSchema(BaseModel):\n    name: str\n",
    )
    code, out = _run(monkeypatch, root)
    assert code == 1
    assert "DatasetCreateSchema" in out


def test_unrelated_gateway_models_pass(monkeypatch, tmp_path: Path) -> None:
    root = _make_fixture(
        tmp_path,
        "from pydantic import BaseModel\n\n\nclass PresignedUploadRequest(BaseModel):\n"
        "    filename: str\n\n\nclass ConfluenceReadRequest(BaseModel):\n    page_id: str\n",
    )
    code, out = _run(monkeypatch, root)
    assert code == 0, out


@pytest.mark.parametrize(
    "sql",
    [
        "UPDATE documents SET status = 'x'",
        "DELETE FROM segments WHERE document_id = $1",
        "INSERT INTO confluence_pages (id) VALUES ($1)",
        "SELECT 1 FROM dataset_permissions",
        "JOIN image_segments USING (segment_id)",
        "INSERT INTO segment_images (segment_id) VALUES ($1)",
        "DELETE FROM document_summaries WHERE document_id = $1",
    ],
)
def test_each_kb_table_verb_is_caught(monkeypatch, tmp_path: Path, sql: str) -> None:
    code, out = _run(
        monkeypatch,
        _make_fixture(tmp_path, f"def probe(db):\n    return '''{sql}'''\n"),
    )
    assert code == 1, out
