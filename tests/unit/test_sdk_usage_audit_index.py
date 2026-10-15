"""Static binding checks; measured PostgreSQL evidence is separate, not simulated."""
import hashlib
import json
import os
from pathlib import Path
import re
import unittest
ROOT=Path(__file__).resolve().parents[2]
DB=Path(os.environ.get('SDK_INDEX_DATABASE_ROOT',str(ROOT/'database')))
EPOCH=DB/'migrations/2026_08_post_kb_v1'
AUDIT=Path(os.environ.get('SDK_USAGE_AUDIT_SOURCE',str(ROOT/'rust/platform-native/crates/platform-gateway-integrations/src/migration_adapters/sdk_usage_audit.rs')))
class IndexContract(unittest.TestCase):
 def setUp(self):
  self.manifest=json.loads((EPOCH/'manifest.yml').read_text());self.change=next(c for c in self.manifest['changes'] if c['sequence']==16)
  self.sql=(EPOCH/'016_sdk_usage_audit_lookup.sql').read_text()
  self.prior=json.loads((EPOCH/'015_verification.json').read_text());self.record=json.loads((EPOCH/'016_verification.json').read_text())
 def test_contiguous_epoch_and_prior_changes_untouched(self):
  self.assertGreaterEqual(self.manifest['epoch'],16)
  self.assertEqual([c['sequence'] for c in self.manifest['changes']],list(range(1,self.manifest['epoch']+1)))
  self.assertEqual({str(c['sequence']):c['sha256'] for c in self.manifest['changes'][:15]},self.prior['changes'])
 def test_sql_checksum_is_exact(self):
  self.assertEqual(self.change['sha256'],hashlib.sha256(self.sql.encode()).hexdigest())
 def test_only_additive_nonunique_partial_index(self):
  body='\n'.join(line for line in self.sql.splitlines() if not line.strip().startswith('--'))
  self.assertIn('CREATE INDEX sdk_usage_audit_lookup',body)
  self.assertIn('ON gateway.audit_logs (tenant_id, user_id, resource_id, action, id)',body)
  for forbidden in ['CREATE UNIQUE','GRANT','REVOKE','DELETE','UPDATE','ALTER TABLE','DROP','CONCURRENTLY']:
   self.assertNotIn(forbidden,body)
 def test_transaction_and_budgets(self):
  self.assertEqual(self.change['owner'],'owner');self.assertEqual(self.change['transaction_mode'],'transactional')
  self.assertEqual(self.change['rollback_class'],'old-binary-compatible')
  self.assertEqual(self.change['lock_budget_seconds'],10);self.assertEqual(self.change['timeout_seconds'],300)
  self.assertIsNone(self.change['resume_handler']);self.assertIsNone(self.change['repair_handler'])
 def test_reference_binds_real_baseline_and_all_changes(self):
  self.assertEqual(self.record['baseline_manifest_sha256'],hashlib.sha256((DB/'baselines/2026_08_post_kb_v1/manifest.json').read_bytes()).hexdigest())
  self.assertEqual(self.record['sequence'],16)
  self.assertEqual(self.record['changes'],{str(c['sequence']):c['sha256'] for c in self.manifest['changes'][:16]})
 def test_acl_extensions_and_reference_data_unchanged(self):
  for name in ['acl','extensions','reference_data']:self.assertEqual(self.prior['fingerprints'][name],self.record['fingerprints'][name])
  self.assertNotEqual(self.prior['fingerprints']['structural'],self.record['fingerprints']['structural'])
 def test_actual_query_literals_match_partial_predicate(self):
  query=re.search(r'const SELECT: &str = "([^"]+)";',AUDIT.read_text()).group(1)
  self.assertIn("event_type='sdk_usage_observation'",query);self.assertIn("resource_type='sdk_request'",query)
  self.assertIn("event_type = 'sdk_usage_observation'",self.sql);self.assertIn("resource_type = 'sdk_request'",self.sql)
  self.assertIn('tenant_id=$2 AND user_id=$3 AND action=$4',query);self.assertIn('ORDER BY id LIMIT 2',query)
 def test_postcondition_checks_keys_and_nonunique_readiness(self):
  condition=' '.join(self.change['postconditions'])
  for expected in ['i.indisvalid','i.indisready','NOT i.indisunique','i.indnkeyatts=5','i.indnatts=5',"a.amname='btree'",'pg_get_expr']:
   self.assertIn(expected,condition)
  for n,key in enumerate(['tenant_id','user_id','resource_id','action','id'],1):self.assertIn(f"pg_get_indexdef(i.indexrelid,{n},true)='{key}'",condition)
if __name__=='__main__':unittest.main()
