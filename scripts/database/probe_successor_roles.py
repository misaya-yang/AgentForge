#!/usr/bin/env python3
"""Opt-in actual-login probes for an already measured isolated canonical020 DB.

This script never provisions roles, runs migrations/DDL, changes grants, starts a
service or falls back to another identity. DML stays in rolled-back transactions;
PostgreSQL sequence increments are nontransactional and may remain. Run only with
explicit authorization for this exact disposable DB and its synthetic fixtures.
Default invocation is offline source validation and never reads DSN variables.
"""
from __future__ import annotations

import argparse
import asyncio
from contextlib import asynccontextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import uuid

BASELINE = "2026_08_post_kb_v1"
ROLES = ("gateway", "knowledge_api", "knowledge_worker", "runtime", "capability_worker")
SOURCE = "rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_ingest/sql.rs"
#22 exact production-query cases, plus the composite-FK rejection below.
REQUEUE_CASES = (
    ("same_running_generation", True), ("wrong_document", False),
    ("wrong_dataset", False), ("wrong_execution", False),
    ("missing_execution_marker", False), ("replaced_generation", False),
    ("execution_cancelled", False), ("execution_failed", False),
    ("execution_completed", False), ("null_process_rule", False),
    ("special_document", False), ("special_manifest", False),
    ("dataset_deleted", False), ("dataset_archived", False),
    ("dataset_deletion_fence", False), ("document_disabled", False),
    ("document_archived", False), ("lifecycle_deactivating", False),
    ("lifecycle_restore_pending", True), ("lifecycle_restore_disabled", False),
    ("document_terminal", False), ("document_waiting", False),
)


def production_requeue(root: Path) -> str:
    source = (root / SOURCE).read_text()
    matches = re.findall(r'pub const REQUEUE_UNPUBLISHED_DOCUMENT_GENERATION_0: &str = r###"(.*?)"###;', source, re.S)
    if len(matches) != 1:
        raise ValueError("production requeue source identity is ambiguous")
    return matches[0]


def source_contract(root: Path) -> dict:
    sys.path[:0] = [str(root), str(root / "packages/ai-gateway-contracts/src")]
    from database.authority.manifest import load_epoch_manifest
    manifest = load_epoch_manifest(root / "database/migrations" / BASELINE / "manifest.yml")
    if manifest.epoch != 20:
        raise ValueError("live probe requires externally composed canonical020 source")
    sql = production_requeue(root)
    baseline_path = root / "database/baselines" / BASELINE / "manifest.json"
    baseline = json.loads(baseline_path.read_text())
    marker = {key: baseline[key] for key in ("baseline_id", "structural_sha256", "acl_sha256", "extensions_sha256", "reference_data_sha256", "source_git_sha")}
    marker["manifest_sha256"] = hashlib.sha256(baseline_path.read_bytes()).hexdigest()
    return {"baseline_id": BASELINE, "epoch": 20, "baseline": marker,
            "changes": [{"baseline_id": BASELINE, "sequence": c.sequence,
                         "name": c.name, "checksum_sha256": c.sha256} for c in manifest.changes],
            "production_requeue_sha256": hashlib.sha256(sql.encode()).hexdigest()}


def require_measurements(root: Path) -> None:
    from database.authority.epoch_verification import expected_epoch_fingerprints
    from database.authority.manifest import load_baseline_manifest, load_epoch_manifest
    from database.authority.runner import AuthorityPaths
    paths = AuthorityPaths(root / "database")
    baseline = load_baseline_manifest(paths.baseline_dir(BASELINE) / "manifest.json")
    changes = load_epoch_manifest(paths.epoch_dir(BASELINE) / "manifest.yml").changes
    for epoch in (18, 19, 20):
        expected_epoch_fingerprints(paths, baseline, {c.sequence: c.sha256 for c in changes if c.sequence <= epoch})


def check_destination(database: str, prefix: str, dataset: str, rule: str, other_dataset: str = "") -> None:
    if not re.fullmatch(r"schema_successor_verify_[a-z0-9_]{8,48}", database):
        raise ValueError("explicit disposable database name required")
    if not re.fullmatch(r"[a-z][a-z0-9_]{0,20}_", prefix):
        raise ValueError("invalid canonical role prefix")
    if not re.fullmatch(r"schema_successor_probe_[a-z0-9_]{8,48}", dataset) or not rule:
        raise ValueError("explicit synthetic Knowledge dataset and existing process rule required")
    if not re.fullmatch(r"schema_successor_probe_[a-z0-9_]{8,48}", other_dataset) or other_dataset == dataset:
        raise ValueError("distinct existing synthetic dataset required for composite-FK negative")


@asynccontextmanager
async def rolled_back(conn):
    transaction = conn.transaction(readonly=False)
    await transaction.start()
    try:
        await conn.execute("SET LOCAL search_path=pg_temp,pg_catalog")
        yield
    finally:
        await transaction.rollback()


async def denied(conn, sql: str, *args) -> None:
    #A savepoint contains the expected error without poisoning the outer probe.
    transaction = conn.transaction(readonly=False)
    await transaction.start()
    try:
        try:
            await conn.execute(sql, *args)
        except Exception as exc:
            if getattr(exc, "sqlstate", None) != "42501":
                raise AssertionError("expected SQLSTATE42501") from None
        else:
            raise AssertionError("unexpected permission")
    finally:
        await transaction.rollback()


async def matrix(conn, role: str, prefix: str, identity: dict) -> dict:
    assert await conn.fetchval("SELECT current_user=session_user")
    assert await conn.fetchval("SELECT session_user") == prefix + role
    assert not await conn.fetchval("SELECT pg_has_role(current_user,$1,'MEMBER')", prefix + "owner")
    for schema in ("gateway", "knowledge"):
        assert not await conn.fetchval("SELECT has_schema_privilege(current_user,$1,'CREATE')", schema)
    if role in ("gateway", "knowledge_api", "knowledge_worker"):
        value = await conn.fetchval("SELECT gateway.platform_schema_admission_snapshot_v1()")
        value = json.loads(value) if isinstance(value, str) else value
        assert value["schema_version"] == "ai-platform/schema-admission-snapshot/v1"
        assert value["epoch"] == 20 and value["changes"] == identity["changes"]
        assert value["baselines"] == [identity["baseline"]]
        assert value["has_attempts"] is False and value["has_incomplete_attempts"] is False
        await denied(conn, "SELECT * FROM public.platform_schema_changes LIMIT 0")
    else:
        await denied(conn, "SELECT gateway.platform_schema_admission_snapshot_v1()")
    for privilege in ("SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER"):
        allowed = role == "gateway" and privilege in ("SELECT", "INSERT", "UPDATE")
        assert await conn.fetchval("SELECT has_table_privilege(current_user,'gateway.task_deliveries',$1)", privilege) == allowed
    if role != "gateway":
        await denied(conn, "SELECT * FROM gateway.task_deliveries LIMIT 0")
    if role in ("knowledge_api", "knowledge_worker"):
        await denied(conn, "UPDATE knowledge.kb_document_progress_events SET payload=payload WHERE false")
        await denied(conn, "DELETE FROM knowledge.kb_document_progress_events WHERE false")
        await denied(conn, "SELECT knowledge.record_kb_document_progress_event()")
        assert not await conn.fetchval("SELECT has_sequence_privilege(current_user,'knowledge.kb_document_progress_events_event_sequence_seq','USAGE,SELECT,UPDATE')")
    return {"role": role, "actual_login": True, "acl": "passed"}


async def add_document(conn, dataset, rule, doc, execution=None, *, execution_status="running", execution_manifest=None):
    metadata = {} if execution is None else {"_document_pipeline_execution_id": execution}
    await conn.execute("INSERT INTO knowledge.documents(document_id,dataset_id,title,status,process_rule_id,metadata) VALUES($1,$2,'schema successor synthetic probe','indexing',$3,$4::pg_catalog.jsonb)", doc, dataset, rule, json.dumps(metadata))
    if execution is not None:
        await conn.execute("INSERT INTO knowledge.document_pipeline_executions(execution_id,document_id,dataset_id,action,process_rule_id,status,manifest) VALUES($1,$2,$3,'recover',$4,$5,$6::pg_catalog.jsonb)", execution, doc, dataset, rule, execution_status, json.dumps(execution_manifest or {}))


async def knowledge_cases(worker, api, dataset, other_dataset, rule, sql):
    for conn in (worker, api):
        assert await conn.fetchval("SELECT EXISTS(SELECT 1 FROM knowledge.datasets WHERE dataset_id=$1 AND NOT is_deleted AND NOT is_archived)", dataset)
        assert await conn.fetchval("SELECT EXISTS(SELECT 1 FROM knowledge.dataset_process_rules WHERE dataset_id=$1 AND id=$2)", dataset, rule)
    assert await worker.fetchval("SELECT EXISTS(SELECT 1 FROM knowledge.datasets WHERE dataset_id=$1)", other_dataset)
    passed = []
    for case, expected in REQUEUE_CASES:
        async with rolled_back(worker):
            doc, execution = "schema_probe_" + uuid.uuid4().hex, "schema_probe_" + uuid.uuid4().hex
            await add_document(worker, dataset, rule, doc, execution,
                               execution_status=case.removeprefix("execution_") if case.startswith("execution_") else "running",
                               execution_manifest={"special_publication": True} if case == "special_manifest" else {})
            params = [doc, dataset, execution]
            if case in ("wrong_document", "wrong_dataset", "wrong_execution"):
                params[("wrong_document", "wrong_dataset", "wrong_execution").index(case)] += "_wrong"
            elif case in ("missing_execution_marker", "replaced_generation", "special_document"):
                metadata = {} if case == "missing_execution_marker" else {"_document_pipeline_execution_id": execution + ("_new" if case == "replaced_generation" else ""), **({"_special_publication_generation_id": "special"} if case == "special_document" else {})}
                await worker.execute("UPDATE knowledge.documents SET metadata=$2::pg_catalog.jsonb WHERE document_id=$1", doc, json.dumps(metadata))
            elif case == "null_process_rule":
                await worker.execute("UPDATE knowledge.documents SET process_rule_id=NULL WHERE document_id=$1", doc)
            elif case in ("dataset_deleted", "dataset_archived"):
                column = "is_deleted" if case == "dataset_deleted" else "is_archived"
                await worker.execute(f"UPDATE knowledge.datasets SET {column}=true WHERE dataset_id=$1", dataset)
            elif case == "dataset_deletion_fence":
                await worker.execute("UPDATE knowledge.datasets SET index_config='{" + '"retrieval":{"_index_deletion_fence":{}}' + "}'::pg_catalog.jsonb WHERE dataset_id=$1", dataset)
            elif case in ("document_disabled", "document_archived"):
                clause = "enabled=false" if case == "document_disabled" else "archived=true"
                await worker.execute(f"UPDATE knowledge.documents SET {clause} WHERE document_id=$1", doc)
            elif case.startswith("lifecycle_"):
                marker = {"status": "deactivating" if case == "lifecycle_deactivating" else "pending", "desired_enabled": case != "lifecycle_restore_disabled", "desired_archived": False}
                await worker.execute("UPDATE knowledge.documents SET enabled=false,archived=true,metadata=metadata||$2::pg_catalog.jsonb WHERE document_id=$1", doc, json.dumps({"_document_lifecycle_reindex": marker}))
            elif case in ("document_terminal", "document_waiting"):
                await worker.execute("UPDATE knowledge.documents SET status=$2 WHERE document_id=$1", doc, "completed" if case == "document_terminal" else "waiting")
            assert (await worker.fetchval(sql, *params) is not None) == expected, case
            if expected:
                assert await worker.fetchval("SELECT status='waiting' AND progress=0 AND error IS NULL FROM knowledge.documents WHERE document_id=$1", doc)
            passed.append(case)
    async with rolled_back(worker):
        doc = "schema_probe_" + uuid.uuid4().hex
        await add_document(worker, dataset, rule, doc)
        transaction = worker.transaction(); await transaction.start()
        try:
            try:
                await worker.execute("INSERT INTO knowledge.document_pipeline_executions(execution_id,document_id,dataset_id,action,process_rule_id,status) VALUES($1,$2,$3,'recover',$4,'running')", "schema_probe_"+uuid.uuid4().hex, doc, other_dataset, None)
            except Exception as exc:
                assert getattr(exc, "sqlstate", None) == "23503"
                assert getattr(exc, "constraint_name", None) == "fk_pipeline_execution_document_dataset"
            else:
                raise AssertionError("composite FK accepted wrong dataset")
        finally:
            await transaction.rollback()
        assert await worker.execute("DELETE FROM knowledge.documents WHERE document_id=$1", doc) == "DELETE 1"
    passed.append("composite_fk_rejected")
    #API has existing event SELECT; worker deliberately does not gain it.
    async with rolled_back(api):
        doc = "schema_probe_" + uuid.uuid4().hex
        await add_document(api, dataset, rule, doc)
        count = await api.fetchval("SELECT count(*) FROM knowledge.kb_document_progress_events WHERE dataset_id=$1 AND document_id=$2", dataset, doc)
        assert count == 1
        await api.execute("UPDATE knowledge.documents SET title=title WHERE document_id=$1", doc)
        assert await api.fetchval("SELECT count(*) FROM knowledge.kb_document_progress_events WHERE dataset_id=$1 AND document_id=$2", dataset, doc) == count
        await api.execute("UPDATE knowledge.documents SET status='completed',progress=100 WHERE document_id=$1", doc)
        await api.execute("DELETE FROM knowledge.documents WHERE document_id=$1", doc)
        events = await api.fetch("SELECT event_type,payload FROM knowledge.kb_document_progress_events WHERE dataset_id=$1 AND document_id=$2 ORDER BY event_sequence", dataset, doc)
        assert [e["event_type"] for e in events] == ["progress", "terminal", "deleted"]
        payload = events[-1]["payload"]; payload = json.loads(payload) if isinstance(payload,str) else payload
        assert payload["document_id"] == doc and payload["terminal"] is True and payload["progress"]["state"] == "deleted"
    return {"actual_worker_requeue_cases": passed, "actual_api_progress_payload": "passed", "all_rows_rolled_back": True, "sequence_increments_may_remain": True}


async def durable_cases(conn):
    async with rolled_back(conn):
        task = "schema_probe_" + uuid.uuid4().hex
        await conn.execute("INSERT INTO gateway.tasks(task_id,request_id,service_id,user_id,tenant_id) VALUES($1,$1,$1,$1,$1)", task)
        await conn.execute("INSERT INTO gateway.task_deliveries(task_id,request_id,service_id,user_id,tenant_id,payload,priority,state) VALUES($1,$1,$1,$1,$1,'{}'::pg_catalog.jsonb,0,'pending')", task)
        claim = uuid.uuid4()
        assert await conn.fetchval("UPDATE gateway.task_deliveries SET state='claimed_unknown',claim_id=$2,claimed_at=clock_timestamp() WHERE task_id=$1 AND state='pending' RETURNING task_id", task, claim) == task
        assert await conn.fetchval("UPDATE gateway.task_deliveries SET state='completed',finished_at=clock_timestamp() WHERE task_id=$1 AND claim_id=$2 AND state='claimed_unknown' RETURNING task_id", task, uuid.uuid4()) is None
        assert await conn.fetchval("UPDATE gateway.task_deliveries SET state='completed',finished_at=clock_timestamp() WHERE task_id=$1 AND claim_id=$2 AND state='claimed_unknown' RETURNING task_id", task, claim) == task
        assert await conn.fetchval("UPDATE gateway.task_deliveries SET state='failed',finished_at=clock_timestamp() WHERE task_id=$1 AND claim_id=$2 AND state='claimed_unknown' RETURNING task_id", task, claim) is None
        await denied(conn, "DELETE FROM gateway.task_deliveries WHERE false")
    return {"gateway_ledger_insert_claim_fence_terminal": "passed", "all_rows_rolled_back": True, "native_ledger_code_execution": False}


async def temporary_shadow_cases(conn, dataset, rule):
    """Separately approved transaction-local DDL; no persistent object/grant change."""
    if not await conn.fetchval("SELECT has_database_privilege(current_user,current_database(),'TEMP')"):
        return {"status": "temp_creation_denied_by_existing_role", "shadow_execution": False}
    async with rolled_back(conn):
        # Explicitly establish the session temp namespace, then create rejecting
        # shadow domains. A pg_temp-first definer would resolve the wrong type.
        await conn.execute("CREATE TEMP TABLE kb_document_progress_events (blocked pg_catalog.bool)")
        for name in ("text", "jsonb", "regclass"):
            await conn.execute(f"CREATE DOMAIN pg_temp.{name} AS pg_catalog.{name} CHECK(false)")
        value = await conn.fetchval("SELECT gateway.platform_schema_admission_snapshot_v1()")
        value = json.loads(value) if isinstance(value, str) else value
        assert value["epoch"] == 20
        doc = "schema_probe_" + uuid.uuid4().hex
        await add_document(conn, dataset, rule, doc)
        await conn.execute("UPDATE knowledge.documents SET status='completed',progress=100 WHERE document_id=$1", doc)
        await conn.execute("DELETE FROM knowledge.documents WHERE document_id=$1", doc)
        assert await conn.fetchval("SELECT count(*) FROM pg_temp.kb_document_progress_events") == 0
    return {"status": "passed_hostile_temporary_types_and_relation", "ddl_rolled_back": True}


async def run(args, identity):
    if sys.flags.optimize:
        raise ValueError("optimized Python is forbidden for acceptance probes")
    import asyncpg
    connections = {}
    try:
        for role in ROLES:
            conn = await asyncpg.connect(os.environ[f"AI_GATEWAY_DATABASE_{role.upper()}_DSN"], server_settings={"default_transaction_read_only":"on", "statement_timeout":"5000", "lock_timeout":"1000"})
            connections[role] = conn
            assert await conn.fetchval("SELECT current_database()") == args.database
            assert await conn.fetchval("SELECT inet_server_addr()::text") in (None, "127.0.0.1", "127.0.0.1/32", "::1", "::1/128")
        matrices = [await matrix(conn, role, args.role_prefix, identity) for role, conn in connections.items()]
        knowledge = await knowledge_cases(connections["knowledge_worker"], connections["knowledge_api"], args.dataset, args.other_dataset, args.process_rule, production_requeue(args.root))
        durable = await durable_cases(connections["gateway"])
        shadow = {"status": "not_run_requires_separate_ddl_approval"}
        if args.run_approved_temporary_shadow_probes:
            # A fresh session prevents precompiled reader/trigger plans from
            # masking first-call type resolution under hostile temporary names.
            fresh = await asyncpg.connect(os.environ["AI_GATEWAY_DATABASE_KNOWLEDGE_API_DSN"], server_settings={"default_transaction_read_only":"on", "statement_timeout":"5000", "lock_timeout":"1000"})
            try:
                assert await fresh.fetchval("SELECT current_database()") == args.database
                assert await fresh.fetchval("SELECT current_user=session_user")
                assert await fresh.fetchval("SELECT session_user") == args.role_prefix + "knowledge_api"
                assert await fresh.fetchval("SELECT inet_server_addr()::text") in (None, "127.0.0.1", "127.0.0.1/32", "::1", "::1/128")
                shadow = await temporary_shadow_cases(fresh, args.dataset, args.process_rule)
            finally:
                await fresh.close()
        return {"status":"passed_bounded_actual_role_sql", "matrix":matrices, "knowledge":knowledge, "durable":durable, "temporary_shadow":shadow, "source":identity,
                "not_verified":["native process startup", "native PgTaskDeliveryLedger execution", "Redis/session revocation", "concurrent connections/claim race", "process crash", "provider/HTTP", "foreign trigger attachment", "seven-day retention"]}
    finally:
        for conn in connections.values():
            await conn.close()


def main():
    if sys.flags.optimize:
        raise ValueError("optimized Python is forbidden for acceptance probes")
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root",type=Path,default=Path(__file__).resolve().parents[2])
    parser.add_argument("--run-approved-isolated-probes",action="store_true")
    parser.add_argument("--run-approved-temporary-shadow-probes",action="store_true")
    parser.add_argument("--database",default="")
    parser.add_argument("--role-prefix",default="ai_gateway_")
    parser.add_argument("--dataset",default="")
    parser.add_argument("--process-rule",default="")
    parser.add_argument("--other-dataset",default="")
    args=parser.parse_args(); args.root=args.root.resolve()
    identity=source_contract(args.root)
    if args.run_approved_temporary_shadow_probes and not args.run_approved_isolated_probes:
        raise ValueError("shadow probes require the explicit isolated probe mode")
    if not args.run_approved_isolated_probes:
        print(json.dumps({"status":"offline_source_only", "production_requeue_sha256":identity["production_requeue_sha256"],"database_actions":False}));return 0
    check_destination(args.database,args.role_prefix,args.dataset,args.process_rule,args.other_dataset)
    require_measurements(args.root)
    print(json.dumps(asyncio.run(run(args,identity)),sort_keys=True));return 0


if __name__=="__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        #No driver text, arguments, DSNs, row values or passwords in diagnostics.
        print(json.dumps({"status":"failed", "error_class":type(exc).__name__, "sqlstate":getattr(exc,"sqlstate",None)}))
        raise SystemExit(1) from None
