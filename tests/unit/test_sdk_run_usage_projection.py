"""Read-only source contracts; these do not execute Rust, SQL, or HTTP."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
GATEWAY = ROOT / 'rust/platform-native/crates/platform-gateway/src/migration_agent'
USAGE = (GATEWAY / 'sdk_approvals/usage.rs').read_text()
RUNS = (GATEWAY / 'sdk_approvals.rs').read_text()
CLIENT = (ROOT / 'rust/platform-native/crates/platform-client/src/assistant_runs.rs').read_text()
WEB = (ROOT / 'web/src/api/assistant.ts').read_text()

class SdkRunUsageSourceContract(unittest.TestCase):
    def test_exact_indexed_bounded_lookup(self):
        sql = re.search(r'const SELECT: &str = "([^"]+)";', USAGE).group(1)
        self.assertIn("event_type='sdk_usage_observation' AND resource_type='sdk_request'", sql)
        self.assertIn('resource_id=$1 AND tenant_id=$2 AND user_id=$3 AND action=$4 ORDER BY id LIMIT 2', sql)
        self.assertNotIn('->', sql)
        self.assertIn('sdk_usage_audit_lookup', USAGE)
        self.assertLess(USAGE.index('fetch_one(&mut *tx).await'), USAGE.index('fetch_all(&mut *tx).await'))
    def test_optional_lookup_has_client_and_server_deadlines(self):
        self.assertIn('tokio::time::timeout(std::time::Duration::from_secs(1), observation)', USAGE)
        self.assertIn('bounded(observe(pool, identity, run)).await', USAGE)
        self.assertIn("set_config('statement_timeout','250ms',true)", USAGE)
        self.assertIn("set_config('lock_timeout','100ms',true)", USAGE)
        self.assertNotIn('fetch_all(pool)', USAGE)
        self.assertIn('tx.rollback().await', USAGE)
    def test_cancel_does_not_wait_for_optional_audit_enrichment(self):
        cancel = RUNS.split('pub async fn cancel_run', 1)[1]
        self.assertIn('run_status_inner(pool,actor,run,false)', cancel)
        self.assertNotIn('usage::read', cancel)
        self.assertIn('if include_usage {row["usage"]=usage::read', RUNS)
    def test_reader_has_no_writes_or_new_authority(self):
        for forbidden in ('INSERT ', 'UPDATE ', 'DELETE ', 'CREATE ', 'assistant_sdk_execution_lineage', '017'):
            self.assertNotIn(forbidden, USAGE)
    def test_scope_reuses_all_identity_fields(self):
        for key in ('tenant_id','user_id','service_id','request_id','session_id','framework'):
            self.assertIn('"'+key+'":', RUNS)
        self.assertIn('canonical_hash(identity)', USAGE)
        self.assertIn('summary["assistant_run_id"] == run', USAGE)
        self.assertIn('summary != admission', USAGE)
    def test_conflicts_and_absence_are_unavailable(self):
        self.assertIn('rows.len() > 1', USAGE)
        self.assertIn('unavailable("identity_conflict")', USAGE)
        self.assertIn('unavailable("unresolved_admission")', USAGE)
        self.assertIn('"accounting_status":"not_asserted"', USAGE)
        self.assertNotRegex(USAGE.split("#[cfg(test)]")[0], r'"(?:input_tokens|provider_cost_usd)"\s*:\s*0')
    def test_only_sdk_projection_changes_no_runtime_identity(self):
        self.assertIn("'harness_thread_id',NULL,'harness_turn_id',NULL", RUNS)
        self.assertIn('row["usage"]=usage::read', RUNS)
        self.assertIn('.remove("request_id")', RUNS)
        self.assertIn('.remove("service_id")', RUNS)
    def test_cli_uses_sanitized_run_projection(self):
        self.assertIn('run_view(id, value)', CLIENT)
        self.assertIn('result["usage"] = filtered', CLIENT)
        self.assertIn('metadata["accounting_status"] = json!("not_asserted")', CLIENT)
        self.assertIn('metadata["provider_usage"] = usage_tokens(raw)', CLIENT)
    def test_reported_and_phase_consistency_are_validated(self):
        self.assertIn('input.checked_add(output)', USAGE)
        self.assertIn('value.as_i64() == Some(total)', USAGE)
        self.assertIn('observation["usage_state"] == "reported" && complete_tokens(observation)', USAGE)
        self.assertIn('observation["local_execution_status"].as_str() != expected', USAGE)
    def test_cli_metadata_is_string_enum_only(self):
        self.assertIn('and_then(Value::as_str).filter(|value| allowed.contains(value))', CLIENT)
        self.assertIn('fn run_usage_rejects_nested_or_unknown_metadata_states()', CLIENT)
    def test_web_accepts_sdk_terminal_reason_and_usage(self):
        self.assertIn('"sdk_authority_expired"', WEB)
        self.assertIn('usage?: Record<string, unknown> | null', WEB)
    def test_rust_behavior_tests_are_staged(self):
        self.assertIn('fn partial_cache_and_cost_are_observations_not_charges()', USAGE)
        self.assertIn('fn missing_conflicting_and_wrong_run_observations_never_invent_tokens()', USAGE)
        self.assertIn('fn run_usage_preserves_partial_observations_and_excludes_runtime_identity()', CLIENT)

if __name__ == '__main__':
    unittest.main()
