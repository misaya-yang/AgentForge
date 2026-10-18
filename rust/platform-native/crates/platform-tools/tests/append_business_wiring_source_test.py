"""L0 business-wire source contracts. Does not establish SQL/native acceptance."""
from pathlib import Path
import json
import re
import unittest

ROOT = Path(__file__).resolve().parents[5]
CRATES = ROOT / 'rust/platform-native/crates'
PROPOSAL = ROOT / 'docs/design/proposals/data-platform/append-partitions'
ADAPTER = CRATES / 'platform-service-primitives/src/data_connection/append_store'
LITERALS = re.compile(r'r(?P<h>#+)"(?P<raw>[\s\S]*?)"(?P=h)|"(?P<normal>(?:\\.|[^"\\])*)"')


def strings(text):
    for match in LITERALS.finditer(text):
        if match['raw'] is not None:
            yield match['raw']
        else:
            try:
                yield json.loads('"' + match['normal'] + '"')
            except ValueError:
                pass


class BusinessWiring(unittest.TestCase):
    def test_all_twelve_real_conflict_tails_are_authority_policies(self):
        cases = json.loads((ADAPTER / 'native_statements.json').read_text())
        self.assertEqual(len(cases), 12)
        self.assertEqual(len({(case['family'], case['statement_tail']) for case in cases}), 12)
        installed = (PROPOSAL / 'native-writer-policies.sql').read_text()
        for case in cases:
            tails = [value[value.upper().find('ON CONFLICT'):].strip()
                     for value in strings((CRATES / case['source']).read_text())
                     if 'ON CONFLICT' in value.upper()]
            self.assertIn(case['statement_tail'], tails, case['source'])
            self.assertIn("'" + case['statement_tail'].replace("'", "''") + "'", installed)
            if case['update_sql']:
                self.assertIn("'" + case['update_sql'].replace("'", "''") + "'", installed)
            if case['where_sql']:
                self.assertIn("'" + case['where_sql'].replace("'", "''") + "'", installed)

    def test_owned_inline_upserts_execute_through_adapter_in_the_same_transaction(self):
        for name in ['platform-gateway-foundation/src/usage_recorder.rs',
                     'platform-gateway-foundation/src/usage_stage_receipts.rs',
                     'platform-gateway-foundation/src/provider_batch_settlement.rs',
                     'platform-gateway/src/task_batch_accounting.rs']:
            source = (CRATES / name).read_text()
            for match in LITERALS.finditer(source):
                literal = match['raw'] if match['raw'] is not None else match['normal']
                if 'INSERT INTO' not in literal or 'ON CONFLICT' not in literal:
                    continue
                if not any(table in literal for table in ['billing_events', 'usage_records', 'accounting_checkpoints']):
                    continue
                before = source[max(0, match.start()-220):match.start()]
                self.assertIn('append_store::', before, name)
        ingest = (CRATES / 'platform-gateway-eval/src/migration_trace/ingest.rs').read_text()
        for family in ['gateway.agent_traces', 'gateway.agent_trace_spans', 'gateway.agent_trace_events']:
            self.assertRegex(ingest, r'append_store::fetch_optional\([^;]*"'+re.escape(family)+'"')
        reconcile = (CRATES / 'platform-gateway-eval/src/migration_trace/reconcile.rs').read_text()
        for query in ['INSERT_1', 'INSERT_3', 'INSERT_4']:
            self.assertRegex(reconcile, r'append_store::(?:execute|fetch_optional)\([^;]*queries::'+query)

    def test_missing_authority_is_legacy_but_errors_never_trigger_fallback(self):
        source = (ADAPTER / 'mod.rs').read_text()
        self.assertIn('to_regprocedure', source)
        self.assertIn('if !installed', source)
        self.assertIn('return Ok(false)', source)
        self.assertIn('if !enabled(connection, family).await?', source)
        self.assertNotIn('unwrap_or(false)', source)
        self.assertNotIn('if let Err', source)
        self.assertNotIn('Err(_) =>', source)
        sql = (PROPOSAL / 'expand.sql').read_text()
        self.assertIn("RETURN f.phase<>'registered'", sql)
        self.assertIn('pg_advisory_xact_lock_shared', sql)
        self.assertIn('pg_advisory_xact_lock(hashtextextended', sql)

    def test_retired_checkpoint_cannot_become_a_new_charge_or_optimizer_filtered_error(self):
        sql = (PROPOSAL / 'write-compatibility.sql').read_text()
        # Must raise inside the writer, before outer SELECT can filter NULL rows.
        writer = sql.split('CREATE FUNCTION partition_ops.write_json', 1)[1]
        retired = writer.split('IF found_row IS NULL THEN', 1)[1].split('END IF;', 2)[1]
        self.assertIn("IF p_mode='statement' THEN RAISE EXCEPTION 'retired append identity", retired)
        stage = (CRATES / 'platform-gateway-foundation/src/usage_stage_receipts.rs').read_text()
        self.assertIn('ON CONFLICT(tenant_id,request_id,service_id,request_type,stage) DO UPDATE SET observation=EXCLUDED.observation,updated_at=CURRENT_TIMESTAMP', stage)
        self.assertIn('append_store::bind_tenant', stage)

    def test_real_negative_fixture_is_rollback_only_and_tests_transaction_abort(self):
        source = (CRATES / 'platform-service-primitives/src/data_connection/append_real_tests.rs').read_text()
        self.assertIn('actual_receipt_query_preserves_returning_and_rejects_unapproved_policy', source)
        self.assertIn('RETURNING event_id AS unadmitted_change', source)
        self.assertIn('SELECT 1', source)
        self.assertIn('af_partition_acceptance_', source)
        self.assertNotIn('tx.commit()', source)

    def test_tenant_batch_grouping_keeps_one_atomic_write_transaction(self):
        source = (CRATES / 'platform-gateway-foundation/src/usage_recorder.rs').read_text()
        body = source.split('fn upsert_raw', 2)[-1].split('fn post_write', 1)[0]
        self.assertIn('BTreeMap::<&str, Vec<UsageRecord>>', body)
        self.assertEqual(body.count('self.pool.begin()'), 1)
        self.assertEqual(body.count('tx.commit()'), 1)
        self.assertIn('records.len()', body)
        adapter = (ADAPTER / 'mod.rs').read_text()
        self.assertIn('current_tenant_scope().is_ok_and', adapter)
        self.assertIn("set_config('app.tenant_id',$1,true)", adapter)

    def test_nullable_time_reads_and_legacy_delete_paths_are_wired(self):
        for file in ['billing_store.rs', 'billing_scheduler.rs', 'usage_admin_api.rs',
                     'price_book_service.rs', 'request_meter.rs', 'tenant_policies_store.rs']:
            self.assertIn('append_store::', (CRATES / 'platform-gateway-foundation/src' / file).read_text(), file)
        source = (CRATES / 'platform-gateway-eval/src/migration_trace/detail.rs').read_text()
        cleanup = source.split('pub async fn purge_expired_traces', 1)[1]
        self.assertLess(cleanup.index('append_store::enabled'), cleanup.index('DELETE FROM'))
        detail = source.split('pub async fn purge_expired_traces', 1)[0]
        self.assertNotIn('append_store::enabled', detail)
        sql = (PROPOSAL / 'legacy-retention-adapters.sql').read_text()
        for fn in ['gateway.cleanup_old_usage_records', 'knowledge.prune_kb_document_progress_events']:
            body = sql.split('CREATE OR REPLACE FUNCTION '+fn, 1)[1].split('$$;', 1)[0]
            self.assertLess(body.index('partition_ops.family_enabled'), body.index('DELETE FROM'))
        # Every clock remains its original column; only two are nullable.
        self.assertIn('CASE WHEN attnotnull THEN quote_ident(f.clock_column)', (PROPOSAL / 'expand.sql').read_text())

    def test_metadata_hides_cross_tenant_keys_and_grants_no_directory_dml(self):
        sql = (PROPOSAL / 'write-compatibility.sql').read_text()
        self.assertIn('bound trace tenant required', sql)
        self.assertIn("jsonb_build_object('scope_mismatch',true)", sql)
        self.assertIn("statement_tail=p_tail", sql)
        grants = (PROPOSAL / 'legacy-retention-adapters.sql').read_text()
        self.assertIn('REVOKE ALL ON ALL FUNCTIONS', grants)
        self.assertNotRegex(grants, r'GRANT (?:SELECT|INSERT|UPDATE|DELETE|ALL) ON')
        self.assertIn('has_table_privilege(role_name', grants)

    def test_insert_global_claim_follows_original_constraints_before_fk_triggers(self):
        source = (PROPOSAL / 'expand.sql').read_text()
        after = source.split("IF TG_WHEN='AFTER' THEN", 1)[1].split("RETURN NULL;", 1)[0]
        before = source.split("ELSIF TG_OP='INSERT' THEN", 1)[1].split("ELSIF TG_OP='UPDATE' THEN", 1)[0]
        self.assertIn('partition_ops.claim_row', after)
        self.assertNotIn('partition_ops.claim_row', before)
        self.assertIn("state='active' FOR KEY SHARE", before)
        self.assertIn('CREATE TRIGGER "AAAA_af_global_after" AFTER', source)
        self.assertIn('upsert BEFORE INSERT projection requires explicit successor', source)
        real = (CRATES / 'platform-service-primitives/src/data_connection/append_real_tests.rs').read_text()
        self.assertIn('Option::<serde_json::Value>::None', real)
        self.assertIn('23502', real)

    def test_no_second_pool_checkout_while_holding_billing_summary_connection(self):
        source = (CRATES / 'platform-gateway-foundation/src/billing_store.rs').read_text()
        body = source.split('pub async fn try_usage_summary', 1)[1].split('pub async fn get_usage_summary', 1)[0]
        self.assertLess(body.index('drop(connection)'), body.index('annotate_summary'))


if __name__ == '__main__':
    unittest.main(verbosity=2)
