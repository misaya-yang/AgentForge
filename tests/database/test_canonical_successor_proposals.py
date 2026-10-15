"""Offline successor composition/safety tests; no SQL or Rust execution."""
from __future__ import annotations

import asyncio
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts/database" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


checker = load("check_successor_proposals")
probe = load("probe_successor_roles")


@pytest.fixture
def source(tmp_path):
    root = tmp_path / "source"
    for folder in ("database", "docs/design", "packages/ai-gateway-contracts/src"):
        shutil.copytree(ROOT / folder, root / folder, ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    return root


def plan_at(root):
    path = root / checker.PLAN
    return path, json.loads(path.read_text())


def test_canonical17_and_current_windows_remain_unchanged():
    plan, value = checker.validate(ROOT)
    assert plan["predecessor_epoch"] == 17 and value["epoch"] == 20
    before = json.loads((ROOT / "database/migrations/2026_08_post_kb_v1/manifest.yml").read_text())
    assert value["changes"][:17] == before["changes"]
    assert [c["sequence"] for c in value["changes"]] == list(range(1, 21))
    contract = (ROOT / "packages/ai-gateway-contracts/src/ai_gateway_contracts/database_revision.py").read_text()
    assert "KNOWLEDGE_MAX_SUPPORTED_EPOCH_SEQUENCE = 17" in contract
    assert "MAX_SUPPORTED_EPOCH_SEQUENCE = 10" in contract
    assert not (ROOT / "rust/platform-native/crates/platform-gateway/src/migration_host/schema_admission.rs").exists()


@pytest.mark.parametrize("field,value", [("file", "../../escape.sql"), ("file", "/tmp/escape.sql"), ("file", "019_wrong.sql"), ("name", "../wrong")])
def test_malicious_destinations_rejected_before_output(source, tmp_path, field, value):
    path, plan = plan_at(source); plan["successors"][0]["change"][field] = value
    path.write_text(json.dumps(plan))
    output = tmp_path / "preview"
    with pytest.raises(checker.ProposalError, match="destination"):
        checker.materialize(source, output)
    assert not output.exists()


@pytest.mark.parametrize("baseline", ["../escape", "/tmp/escape", "unreviewed"])
def test_baseline_escape_rejected_before_output(source, tmp_path, baseline):
    path, plan = plan_at(source); plan["baseline_id"] = baseline; path.write_text(json.dumps(plan))
    with pytest.raises(checker.ProposalError, match="baseline"):
        checker.materialize(source, tmp_path / "preview")
    assert not (tmp_path / "preview").exists()


def test_drift_fails_closed(source):
    path = source / "database/migrations/2026_08_post_kb_v1/017_sdk_execution_lineage.sql"
    path.write_text(path.read_text() + "\n-- drift\n")
    with pytest.raises(checker.ProposalError, match="preimage"):
        checker.validate(source)


def test_proposal_hash_drift_rejected(source):
    path = source / "docs/design/application-schema-admission.sql.proposal"
    path.write_text(path.read_text() + "\n-- drift\n")
    with pytest.raises(checker.ProposalError, match="source hash"):
        checker.validate(source)


def test_symlink_and_existing_or_internal_outputs_rejected(source, tmp_path):
    existing = tmp_path / "exists"; existing.mkdir()
    symlink = tmp_path / "alias"; symlink.symlink_to(existing, target_is_directory=True)
    for output in (existing, source / "preview", source.parent, symlink / "child"):
        with pytest.raises(checker.ProposalError):
            checker.materialize(source, output)
    assert not (source / "preview").exists() and not (existing / "child").exists()


def test_source_symlink_escape_rejected(source, tmp_path):
    path = source / "docs/design/application-schema-admission.sql.proposal"
    moved = tmp_path / "outside.sql"; path.rename(moved); path.symlink_to(moved)
    with pytest.raises(checker.ProposalError, match="escaped"):
        checker.validate(source)


def test_preview_is_exact_and_no_fingerprint_is_fabricated(source, tmp_path):
    output = tmp_path / "preview"
    receipt = checker.materialize(source, output)
    assert receipt["epoch"] == 20 and receipt["database_actions"] is False
    from database.authority.manifest import load_epoch_manifest
    from database.authority.epoch_verification import expected_epoch_fingerprints
    from database.authority.manifest import load_baseline_manifest
    from database.authority.runner import AuthorityError, AuthorityPaths
    paths = AuthorityPaths(output / "database")
    baseline = load_baseline_manifest(paths.baseline_dir(checker.BASELINE) / "manifest.json")
    manifest = load_epoch_manifest(paths.epoch_dir(checker.BASELINE) / "manifest.yml")
    for epoch in (18, 19, 20):
        assert not (paths.epoch_dir(checker.BASELINE) / f"{epoch:03d}_verification.json").exists()
        with pytest.raises(AuthorityError, match="fingerprints are unavailable"):
            expected_epoch_fingerprints(paths, baseline, {c.sequence:c.sha256 for c in manifest.changes if c.sequence <= epoch})
    assert len(manifest.changes) == 20
    assert json.loads((source / "database/migrations/2026_08_post_kb_v1/manifest.yml").read_text())["epoch"] == 17


def test_security_proposals_keep_temp_last_and_exact_authority():
    plan,_ = checker.validate(ROOT)
    for entry in plan["successors"][:2]:
        sql = (ROOT / entry["proposal"]).read_text()
        assert "pg_catalog, pg_temp" in sql and "SECURITY DEFINER" in sql
        assert any("search_path=pg_catalog, pg_temp" in p for p in entry["change"]["postconditions"])
    reader = (ROOT / plan["successors"][0]["proposal"]).read_text()
    assert "ARRAY['gateway','knowledge_api','knowledge_worker']" in reader
    body = reader.split("AS $function$",1)[1].split("$function$",1)[0]
    for forbidden in ("EXECUTE", "INSERT ", "UPDATE ", "DELETE ", "CREATE ", "GRANT "):
        assert forbidden not in body
    repair = (ROOT / plan["successors"][1]["proposal"]).read_text()
    assert repair.count("INSERT INTO knowledge.kb_document_progress_events") == 2
    assert "TG_RELID <> 'knowledge.documents'::regclass" in repair
    statements = "\n".join(s for s in repair.splitlines() if not s.lstrip().startswith("--"))
    assert "GRANT " not in statements
    ledger = (ROOT / plan["successors"][2]["proposal"]).read_text()
    assert "GRANT SELECT, INSERT, UPDATE ON gateway.task_deliveries" in ledger
    assert "state = 'pending'" in ledger


def test_actual_role_probe_default_is_offline_and_denials_are_rw_rollback():
    transaction = SimpleNamespace(start=AsyncMock(), rollback=AsyncMock())
    class Denied(Exception):
        sqlstate = "42501"
    conn = SimpleNamespace(transaction=lambda **kwargs: (calls.append(kwargs) or transaction), execute=AsyncMock(side_effect=Denied()))
    calls = []
    asyncio.run(probe.denied(conn,"UPDATE no_table SET a=a WHERE false"))
    assert calls == [{"readonly":False}]
    transaction.start.assert_awaited_once(); transaction.rollback.assert_awaited_once()
    assert len(probe.REQUEUE_CASES) == 22
    assert sum(expected for _,expected in probe.REQUEUE_CASES) == 2
    text = (ROOT / "scripts/database/probe_successor_roles.py").read_text()
    assert "UPDATE knowledge.document_pipeline_executions" not in text
    assert 'if sys.flags.optimize:' in text
    assert 'current_user=session_user' in text
    assert 'CREATE DOMAIN pg_temp.' in text
    assert 'run_approved_temporary_shadow_probes' in text
    assert 'AI_GATEWAY_DATABASE_ADMIN_DSN' not in text and 'SET ROLE' not in text


def test_optimized_python_rejects_before_any_database_input():
    result = subprocess.run([sys.executable,"-O",str(ROOT / "scripts/database/probe_successor_roles.py"),"--run-approved-isolated-probes"], capture_output=True,text=True)
    assert result.returncode == 1 and '"error_class": "ValueError"' in result.stdout


@pytest.mark.parametrize("database,dataset", [("canonical17", "schema_successor_probe_12345678"), ("schema_successor_verify_12345678", "production")])
def test_actual_role_probe_rejects_nonsynthetic_target(database,dataset):
    with pytest.raises(ValueError):
        probe.check_destination(database,"ai_gateway_",dataset,"synthetic_rule")


def test_composite_fk_probe_requires_existing_distinct_synthetic_dataset():
    with pytest.raises(ValueError, match="distinct"):
        probe.check_destination("schema_successor_verify_12345678", "ai_gateway_", "schema_successor_probe_12345678", "synthetic_rule")
    probe.check_destination("schema_successor_verify_12345678", "ai_gateway_", "schema_successor_probe_12345678", "synthetic_rule", "schema_successor_probe_87654321")
    source = (ROOT / "scripts/database/probe_successor_roles.py").read_text()
    assert 'getattr(exc, "constraint_name", None) == "fk_pipeline_execution_document_dataset"' in source
    assert 'doc, other_dataset, None)' in source
