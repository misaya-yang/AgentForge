"""L0 source contract checks; deliberately no database, Cargo or SQL execution."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[5]
SQL = ROOT / "docs/design/proposals/data-platform/append-partitions"
BASE = (ROOT / "database/baselines/2026_08_post_kb_v1/init.sql").read_text()
NATIVE = ROOT / "rust/platform-native/crates"


def registrations():
    text = (SQL / "expand.sql").read_text()
    return re.findall(r"\('([a-z0-9_]+\.[a-z0-9_]+)','([a-z_]+)','(month|day)',", text)


class SourceContracts(unittest.TestCase):
    def test_all_twenty_two_families_match_both_native_interfaces(self):
        families = {row[0] for row in registrations()}
        self.assertEqual(len(families), 22)
        catalog = (NATIVE / "platform-tools/src/data_security/append_partitions/catalog.rs").read_text()
        api = (NATIVE / "platform-service-primitives/src/data_connection/append.rs").read_text().split("pub const FAMILIES", 1)[1].split("];", 1)[0]
        self.assertEqual(set(re.findall(r'"([a-z0-9_]+\.[a-z0-9_]+)"', catalog)), families)
        self.assertEqual(set(re.findall(r'"([a-z0-9_]+\.[a-z0-9_]+)"', api)), families)
        plan = (ROOT / "docs/plans/platform-upgrade-2026-10/07-data-platform.md").read_text()
        expected = ["usage_records", "request_traces", "audit_logs", "billing_events", "agent_trace_spans", "agent_trace_events", "tool_audit_log", "login_audit", "service_health_records", "assistant_runtime_items", "assistant_audit_events", "assistant_capability_events", "kb_document_progress_events"]
        for name in expected:
            self.assertIn(name, plan)
            self.assertTrue(any(f.endswith("." + name) for f in families), name)

    def test_every_clock_exists_in_actual_frozen_source(self):
        successors = "\n".join(p.read_text() for p in (ROOT / "database/migrations/2026_08_post_kb_v1").glob("0[2][6-9]*.sql"))
        for family, clock, _ in registrations():
            match = re.search(r"CREATE TABLE " + re.escape(family) + r"\s*\(([\s\S]*?)\n\);", BASE + "\n" + successors)
            self.assertIsNotNone(match, family)
            self.assertRegex(match.group(1), rf"\b{clock}\s+(timestamp with time zone|timestamptz)\b", family)

    def test_registry_preserves_expression_partial_and_null_uniqueness(self):
        text = (SQL / "expand.sql").read_text()
        for token in ["idx_usage_records_request_identity", "COALESCE(request_id", "request_id IS NOT NULL"]:
            self.assertIn(token, BASE)
        for token in ["pg_get_indexdef(i.indexrelid)", "pg_get_expr(idx.indpred,source)", "idx.indnullsnotdistinct", "pg_depend", "CREATE UNIQUE INDEX"]:
            self.assertIn(token, text)
        self.assertNotRegex(text, r"DROP (?:INDEX|CONSTRAINT).*request_identity")
        self.assertNotIn("PRIMARY KEY(id,created_at)", text)

    def test_all_fk_actions_and_composite_scope_are_preserved_by_definition(self):
        text = (SQL / "migrate.sql").read_text()
        self.assertIn("pg_get_constraintdef(c.oid)", text)
        self.assertIn("k.original_definition", text)
        self.assertIn("source_columns", text)
        self.assertIn("target_columns", text)
        self.assertIn("' NOT VALID'", text)
        for table in ["gateway.agent_trace_events", "gateway.agent_trace_scores", "gateway.eval_examples"]:
            self.assertRegex(BASE, re.escape(table) + r"\n\s+ADD CONSTRAINT [^;]*REFERENCES gateway.agent_trace_spans\(span_id\) ON DELETE SET NULL")
        self.assertIn("REFERENCES assistant.assistant_runtime_model_leases(lease_id, snapshot_id, run_id, tenant_id, user_id, session_id)", BASE)

    def test_no_legacy_attach_builds_missing_indexes(self):
        text = (SQL / "migrate.sql").read_text()
        self.assertIn("CREATE INDEX CONCURRENTLY", text)
        self.assertIn("valid matching CONCURRENTLY built legacy indexes required", text)
        self.assertIn("indisvalid AND x.indisready", text)
        self.assertIn("VALIDATE CONSTRAINT af_legacy_bound", text)
        self.assertIn("LIMIT %s FOR SHARE", text)
        self.assertIn("p_limit>10000", text)
        self.assertNotRegex(text, r"\bOFFSET\s+\d")

    def test_invoker_writer_has_no_privilege_or_rls_bypass(self):
        text = (SQL / "write-compatibility.sql").read_text()
        body = text.split("CREATE FUNCTION partition_ops.write_json", 1)[1]
        self.assertNotIn("SECURITY DEFINER", body.split("AS $$", 1)[0])
        self.assertIn("original table INSERT privilege required", text)
        self.assertIn("bound transaction tenant required", text)
        self.assertIn("retained_identity", body)
        self.assertIn("pricing_conflict", body)
        self.assertIn("SELECT to_jsonb(candidate)", body)
        self.assertIn("NULLIF($2->'metadata','null'::jsonb)", body)

    def test_retention_has_a_write_reference_and_archive_barrier(self):
        text = (SQL / "retention.sql").read_text()
        guard = (SQL / "expand.sql").read_text()
        self.assertIn("BEFORE INSERT OR UPDATE OR DELETE", guard)
        self.assertIn("partition is sealed for retention", guard)
        self.assertIn("FOR UPDATE", text)
        self.assertIn("t.tableoid<>$2", text)
        self.assertIn("retention_evidence->>'rows'", text)
        self.assertIn("archive receipt replacement refused", text)
        self.assertIn("FINALIZE", text)
        self.assertIn("CONCURRENTLY", text)
        self.assertNotIn("DELETE FROM", text)
        self.assertNotIn("DROP TABLE %s CASCADE", text)
        for gate in ["pitr_restore_drill_sha256", "role_rls_acceptance_sha256", "retention_approval_sha256"]:
            self.assertIn(gate, text)

    def test_active_and_unknown_model_work_never_has_true_retention_predicate(self):
        text = (SQL / "expand.sql").read_text()
        self.assertIn("t.status IN ('completed','failed') AND t.completed_at IS NOT NULL", text)
        self.assertIn("t.reserved_cost_microusd=0", text)
        self.assertIn("t.calls_completed=t.calls_reserved", text)
        self.assertIn("t.state IN ('completed','rejected')", text)
        self.assertIn("t.result IS NOT NULL", text)
        self.assertIn("a.archived_at IS NOT NULL OR a.deleted_at IS NOT NULL", text)
        self.assertIn("d.status IN ('completed','error','failed','disabled')", text)

    def test_acl_rls_triggers_and_sequence_ownership_are_explicit(self):
        migrate = (SQL / "migrate.sql").read_text()
        for token in ["aclexplode(c.relacl)", "aclexplode(a.attacl)", "WITH GRANT OPTION", "ENABLE ROW LEVEL SECURITY", "FORCE ROW LEVEL SECURITY", "pg_get_triggerdef", "OWNED BY", "REVOKE ALL"]:
            self.assertIn(token, migrate)
        self.assertIn("REVOKE ALL", (SQL / "expand.sql").read_text())

    def test_native_cli_cannot_open_a_database_or_execute_unreviewed_ddl(self):
        cli = (NATIVE / "platform-tools/src/bin/platform-append-partitions.rs").read_text()
        self.assertNotIn("PgPool", cli)
        self.assertNotIn('"--dsn"', cli)
        self.assertNotIn('"execute" =>', cli)
        self.assertIn('"verify-archive"', cli)
        self.assertIn('"--now"', cli)

    def test_runtime_tests_require_restored_copy_and_never_commit(self):
        test = (NATIVE / "platform-service-primitives/src/data_connection/append_real_tests.rs").read_text()
        self.assertIn("#[ignore", test)
        self.assertIn("isolated-copy-only", test)
        self.assertIn("af_partition_acceptance_", test)
        self.assertIn("tx.rollback()", test)
        self.assertNotIn("tx.commit()", test)


if __name__ == "__main__":
    unittest.main(verbosity=2)
