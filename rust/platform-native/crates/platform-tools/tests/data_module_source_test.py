"""Source completeness checks; no DB/role/DDL/restore/capacity acceptance."""
from pathlib import Path
import re
import unittest
ROOT=Path(__file__).resolve().parents[5]
class CompleteData(unittest.TestCase):
    def test_all_known_knowledge_assistant_tenant_tables_are_in_first_stage(self):
        base=(ROOT/'database/baselines/2026_08_post_kb_v1/init.sql').read_text()
        expected={t for t,b in re.findall(r'CREATE TABLE ((?:knowledge|assistant)\.\w+) \(([\s\S]*?)\n\);',base) if re.search(r'^\s+tenant_id\s',b,re.M)}
        expected|={'knowledge.documents','knowledge.segments'}
        self.assertEqual(len(expected),50)
        proposal=(ROOT/'docs/design/proposals/data-platform/tenant-rls-complete.sql').read_text()
        export=(ROOT/'docs/design/proposals/data-platform/tenant-rls-catalog.sql').read_text()
        controller=(ROOT/'rust/platform-native/crates/platform-tools/src/data_security/rls.rs').read_text()
        self.assertTrue(all(repr(t) in proposal and repr(t) in export and '"'+t+'"' in controller for t in expected))
        self.assertNotRegex(proposal,r'GRANT\s+(?:ALL|BYPASSRLS)')
    def test_policy_observation_is_readonly_not_schema_admission(self):
        sql=(ROOT/'docs/design/proposals/data-platform/tenant-rls-catalog.sql').read_text()
        self.assertIn('BEGIN READ ONLY',sql);self.assertIn('ROLLBACK;',sql)
        self.assertNotRegex(sql,r'\b(?:ALTER|CREATE|DROP)\b')
    def test_real_acceptance_requires_isolated_explicit_fixture(self):
        tests=(ROOT/'rust/platform-native/crates/platform-service-primitives/src/data_connection/real_tests.rs').read_text()
        self.assertIn('real_transaction_pool_replica_connections_and_prepared_statements_stay_bounded',tests)
        self.assertIn('real_dataset_join_policy_rejects_cross_tenant_document_reparenting',tests)
        self.assertIn('isolated-copy-only',tests);self.assertIn('af_data_',tests)
        self.assertGreaterEqual(tests.count('#[ignore'),3)
    def test_controller_cli_and_all_family_maintenance_are_wired(self):
        cli=(ROOT/'rust/platform-native/crates/platform-tools/src/bin/platform-data-security.rs').read_text()
        self.assertIn('"pool-budget" =>',cli);self.assertIn('"rls-catalog" =>',cli)
        maintenance=(ROOT/'rust/platform-native/crates/platform-tools/src/data_security/append_partitions/mod.rs').read_text()
        self.assertIn('complete 22-family maintenance snapshot required',maintenance)
        self.assertIn('mixed authority observations refused',maintenance)
        self.assertIn('active family required for retention recovery',maintenance)
if __name__=='__main__':unittest.main(verbosity=2)
