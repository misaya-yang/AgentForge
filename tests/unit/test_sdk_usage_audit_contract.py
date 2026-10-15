"""SQLite contract checks execute the staged journal's actual SELECT/INSERT SQL.

This verifies deterministic persistence/reopen/conflict behavior, not PostgreSQL
advisory locks, deployed role grants, or the Rust async implementation.
"""
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'rust/platform-native/crates/platform-gateway-integrations/src/migration_adapters/sdk_usage_audit.rs'
NATIVE = SOURCE.with_name('native_sdk.rs')
CODE = SOURCE.read_text()
SQL = {name: re.search(rf'const {name}: &str = "([^"]+)";', CODE).group(1) for name in ('SELECT', 'INSERT', 'LOCK')}

def bindings(values):
    # SQLite treats PostgreSQL $1 placeholders as named parameters.
    return {str(index): value for index, value in enumerate(values, 1)}

def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False)

def scope(**override):
    identity = dict(tenant_id='tenant', user_id='user', service_id='service', request_id='request', session_id='session', framework='openai_agents')
    identity.update(override)
    resource = hashlib.sha256(encoded(identity).encode()).hexdigest()
    return dict(identity, resource=resource, request_sha256='original-input-digest', model='configured-model')

def unknown(phase):
    return dict(local_execution_status=phase, provider_outcome='unknown', usage_state='unavailable', cost_state='unavailable')

class Journal:
    def __init__(self, path):
        self.path = path
        with self.connection() as db:
            db.execute('CREATE TABLE IF NOT EXISTS gateway.audit_logs (id INTEGER PRIMARY KEY, event_type TEXT,user_id TEXT,tenant_id TEXT,resource_type TEXT,resource_id TEXT,action TEXT,request_summary TEXT,response_summary TEXT,status TEXT)')
    @contextmanager
    def connection(self):
        db = sqlite3.connect(':memory:', timeout=10)
        db.execute('ATTACH DATABASE ? AS gateway', (str(self.path),))
        db.execute('PRAGMA busy_timeout=10000')
        try:
            with db:
                yield db
        finally:
            db.close()
    def rows(self, who, phase):
        with self.connection() as db:
            return db.execute(SQL['SELECT'], bindings((who['resource'], who['tenant_id'], who['user_id'], phase))).fetchall()
    def append(self, who, phase, observation, fail_before_commit=False, uncertain_after_commit=False):
        # BEGIN IMMEDIATE substitutes for PG transaction advisory serialization.
        with self.connection() as db:
            db.execute('BEGIN IMMEDIATE')
            args = (who['resource'], who['tenant_id'], who['user_id'], phase)
            rows = db.execute(SQL['SELECT'], bindings(args)).fetchall()
            expected = (encoded(who), encoded(observation))
            if rows:
                if len(rows) != 1 or rows[0] != expected:
                    raise ValueError('sdk_audit_identity_conflict')
            else:
                if phase != 'admitted':
                    admission = db.execute(SQL['SELECT'], bindings((*args[:-1], 'admitted'))).fetchall()
                    if len(admission) != 1 or admission[0][0] != expected[0]:
                        raise ValueError('sdk_audit_admission_missing')
                db.execute(SQL['INSERT'], bindings(('sdk_usage_observation', who['user_id'], who['tenant_id'], 'sdk_request', who['resource'], phase, *expected)))
            if fail_before_commit:
                raise RuntimeError('synthetic_write_failed')
            db.commit()
            if uncertain_after_commit:
                raise RuntimeError('sdk_audit_commit_unknown')

class UsageAuditContract(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / 'audit.sqlite'
        self.journal = Journal(self.path)
        self.scope = scope()
    def tearDown(self):
        self.temp.cleanup()
    def test_exact_duplicates_and_restart_preserve_one_admission(self):
        self.journal.append(self.scope, 'admitted', unknown('admitted'))
        reopened = Journal(self.path)
        reopened.append(self.scope, 'admitted', unknown('admitted'))
        self.assertEqual(len(reopened.rows(self.scope, 'admitted')), 1)
    def test_concurrent_same_scope_phase_serializes(self):
        with ThreadPoolExecutor(max_workers=8) as executor:
            list(executor.map(lambda _: self.journal.append(self.scope, 'admitted', unknown('admitted')), range(16)))
        self.assertEqual(len(self.journal.rows(self.scope, 'admitted')), 1)
    def test_changed_request_hash_fails_closed(self):
        self.journal.append(self.scope, 'admitted', unknown('admitted'))
        changed = dict(self.scope, request_sha256='changed')
        with self.assertRaisesRegex(ValueError, 'identity_conflict'):
            self.journal.append(changed, 'admitted', unknown('admitted'))
    def test_scope_isolation_even_for_forced_resource_collision(self):
        for tenant in ['tenant', 'other-tenant']:
            other = dict(scope(tenant_id=tenant), resource=self.scope['resource'])
            self.journal.append(other, 'admitted', unknown('admitted'))
            self.assertEqual(len(self.journal.rows(other, 'admitted')), 1)
    def test_terminal_requires_matching_admission(self):
        with self.assertRaisesRegex(ValueError, 'admission_missing'):
            self.journal.append(self.scope, 'completed', {'usage_state':'unavailable'})
    def test_cancel_then_restart_retains_unknown_without_zero(self):
        self.journal.append(self.scope, 'admitted', unknown('admitted'))
        self.journal.append(self.scope, 'cancelled', unknown('cancelled'))
        row = Journal(self.path).rows(self.scope, 'cancelled')[0]
        usage = json.loads(row[1])
        self.assertEqual(usage['provider_outcome'], 'unknown')
        self.assertNotIn('input_tokens', usage)
        self.assertNotIn('provider_cost_usd', usage)
    def test_crash_before_terminal_retains_unresolved_admission(self):
        self.journal.append(self.scope, 'admitted', unknown('admitted'))
        reopened = Journal(self.path)
        self.assertEqual(len(reopened.rows(self.scope, 'admitted')), 1)
        self.assertEqual(reopened.rows(self.scope, 'completed'), [])
    def test_cached_terminal_is_durable_without_a_charge(self):
        self.journal.append(self.scope, 'admitted', unknown('admitted'))
        usage = dict(input_tokens=9, output_tokens=1, provider_cost_usd=0.125, platform_pricing_state='reconciliation_required', provider_usage={'cache_read_input_tokens':3})
        self.journal.append(self.scope, 'completed', usage)
        self.assertEqual(json.loads(Journal(self.path).rows(self.scope,'completed')[0][1]),usage)
        self.assertNotIn('user_quotas', SQL['INSERT'])
        self.assertNotIn('billing_events', SQL['INSERT'])
        self.assertNotIn('usage_records', SQL['INSERT'])
    def test_terminal_failure_keeps_admission(self):
        self.journal.append(self.scope, 'admitted', unknown('admitted'))
        with self.assertRaises(RuntimeError):
            self.journal.append(self.scope, 'completed', {'usage_state':'unavailable'}, fail_before_commit=True)
        self.assertEqual(len(Journal(self.path).rows(self.scope, 'admitted')), 1)
        self.assertEqual(Journal(self.path).rows(self.scope, 'completed'), [])
    def test_uncertain_commit_is_durable_and_explicit_duplicate_stays_single(self):
        self.journal.append(self.scope, 'admitted', unknown('admitted'))
        with self.assertRaisesRegex(RuntimeError, 'commit_unknown'):
            self.journal.append(self.scope, 'completed', {'usage_state':'unavailable'}, uncertain_after_commit=True)
        reopened = Journal(self.path)
        self.assertEqual(len(reopened.rows(self.scope, 'completed')), 1)
        reopened.append(self.scope, 'completed', {'usage_state':'unavailable'})
        self.assertEqual(len(reopened.rows(self.scope, 'completed')), 1)
    def test_phase_collision_does_not_overwrite_terminal(self):
        self.journal.append(self.scope, 'admitted', unknown('admitted'))
        self.journal.append(self.scope, 'completed', {'input_tokens':7})
        with self.assertRaisesRegex(ValueError,'identity_conflict'):
            self.journal.append(self.scope, 'completed', {'input_tokens':8})
        self.assertEqual(json.loads(self.journal.rows(self.scope, 'completed')[0][1]), {'input_tokens':7})
    def test_failed_admission_cannot_dispatch_and_terminal_does_not_retry(self):
        native = NATIVE.read_text()
        for part in [native.split('fn invoke')[1].split('fn stream')[0], native.split('fn stream')[1].split('fn health_check')[0]]:
            self.assertLess(part.index('UsageAudit::admit'), part.index('post_json("v1/runs"'))
        self.assertIn('sdk_audit_index_required', CODE)
        self.assertLess(CODE.index('query_scalar::<_,bool>(INDEX_READY)'),CODE.index('sqlx::query(SELECT)'))
        self.assertIn('SET TRANSACTION ISOLATION LEVEL READ COMMITTED', CODE)
        self.assertLess(CODE.index('SET TRANSACTION ISOLATION LEVEL READ COMMITTED'), CODE.index('sqlx::query(LOCK)'))
        self.assertIn('tx.commit().await.map_err',CODE)
        self.assertNotIn('retry',CODE.replace('not retried',''))
        self.assertIn('tokio::spawn(async move {append',CODE)

if __name__ == '__main__':
    unittest.main()
