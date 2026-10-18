"""L0 source wiring checks only. No Rust/native/network/accepted-build run."""
from pathlib import Path
import json
import re
import unittest
ROOT = Path(__file__).resolve().parents[5]
TOOLS = ROOT / 'rust/platform-native/crates/platform-tools'
MODULE = TOOLS / 'src/performance_baseline'

class BaselineSource(unittest.TestCase):
    def test_standalone_dispatch_and_export_are_real(self):
        self.assertIn('pub mod performance_baseline;', (TOOLS/'src/lib.rs').read_text())
        self.assertIn('performance_baseline::command(&args)', (TOOLS/'src/bin/platform-performance-baseline.rs').read_text())
        entry = (TOOLS/'src/performance_baseline.rs').read_text()
        self.assertIn('"status":"BLOCKED"', entry)
        self.assertIn('"network_executed":false', entry)
        self.assertIn('command == "plan"', entry)
        self.assertIn('controller::run', entry)

    def test_no_runtime_signer_or_acceptance_factory(self):
        code = '\n'.join(p.read_text() for p in MODULE.glob('*.rs') if p.name != 'tests.rs')
        self.assertNotIn('SigningKey', code)
        self.assertNotIn('generate_key', code)
        self.assertNotIn('"accepted":true', code)
        self.assertIn('verify_strict', code)
        self.assertIn('externally_pinned_acceptance_key_required', code)
        self.assertIn('accepted_gate_evidence_changed', code)

    def test_controlled_transports_do_not_bypass_existing_controls(self):
        code = (MODULE/'controller.rs').read_text()
        for value in ['.no_proxy()', 'Policy::none()', 'reqwest::retry::never()', '/api/v1/auth/me', 'live_actor_tenant_mismatch']:
            self.assertIn(value, code)
        self.assertNotIn('danger_accept_invalid', code)
        self.assertIn('Identity::from_pem', code)
        self.assertIn('Certificate::from_pem', code)
        sql = re.findall(r'sqlx::query(?:_as)?\("([^"]+)"', code)
        self.assertTrue(sql)
        self.assertTrue(all(value.startswith(('SELECT ', 'SET TRANSACTION READ ONLY')) for value in sql))
        self.assertIn('bind_transaction', code)
        self.assertIn('same_request_usage_does_not_match_provider_or_request_meter', code)

    def test_profile_requires_gateway_knowledge_ingestion_and_correctness(self):
        p = json.loads((MODULE/'profile.example.json').read_text())
        self.assertEqual({c['kind'] for c in p['cases']}, {'gateway_mock','retrieval','ingestion'})
        for k in ['authentication','limiter','tenant_acl','billing','csrf_when_applicable']:
            self.assertIs(p['environment']['controls'][k], True)
        stages = next(c for c in p['cases'] if c['kind']=='retrieval')['stages']
        self.assertEqual(set(stages), {'embedding','cache_lookup','lexical','vector','fusion','rerank','serialization'})
        self.assertEqual(next(c for c in p['cases'] if c['kind']=='retrieval')['retrieval']['results_pointer'], '/results')
        self.assertIn('correctness_control_must_remain_enabled', (MODULE/'config.rs').read_text())

    def test_actual_build_image_pid_configuration_and_socket_are_bound(self):
        code = (MODULE/'identity.rs').read_text()
        for value in ['verify_sources', 'source_inventory_changed', 'live_process_binary_changed',
                      'process_privilege_changed','live_environment_profile_changed',
                      'runtime_material_changed','live_container_image_or_owner_changed',
                      'live_resource_or_security_profile_changed', 'service_process_not_in_accepted_container',
                      'actual_owned_service_listener_missing','service.instance.id']:
            self.assertIn(value, code + (MODULE/'trace.rs').read_text())
        self.assertIn('"container", "inspect"', code)
        self.assertNotRegex(code, r'\.args\(\["(?:up|down|exec|pull|restart)"')
        self.assertIn('DOCKER_HOST', code)
        self.assertIn('ProcessIdentity', code)

    def test_gateway_timings_use_same_request_boundaries_not_percentile_subtraction(self):
        code = (MODULE/'trace.rs').read_text()
        for name in ['receive','admit','upstream_dispatch','upstream_first_byte','upstream_complete','response_complete']:
            self.assertIn('"'+name+'"', code)
        self.assertIn('ms(0,2)+ms(4,5)', code.replace(' ',''))
        self.assertIn('missing_ambiguous_or_unordered_gateway_boundaries', code)
        self.assertIn('foreign_or_unmatched_trace', code)
        self.assertIn('trace_instance_not_verified_live_process', code)
        self.assertIn('decode_id', code)

    def test_real_provider_and_synthetic_populations_cannot_be_silently_merged(self):
        code = (MODULE/'controller.rs').read_text()
        for name in ['blocked_real_provider_authorization_missing','real_provider_case_observed_synthetic_upstream',
                     'owned_upstream_not_uniquely_correlated','equivalent_upstream_request_body']:
            self.assertIn(name, code)
        self.assertIn('gen_ai.provider.name', (MODULE/'config.rs').read_text())
        self.assertIn('real_provider_identity', (MODULE/'trace.rs').read_text())
        self.assertIn('provider_scope', code)

    def test_partial_observations_and_ingestion_intents_are_preserved(self):
        code = (MODULE/'controller.rs').read_text()
        for value in ['http_observed','body_state','incomplete','http_body_or_timeout',
                      'candidate_response_before_correctness_validation', 'run_deadline_effects_may_continue_no_replay',
                      'private-state.json','not_started_warmup_failed','resource_safety_stop']:
            self.assertIn(value, code)
        self.assertIn('terminal_offset_ms', code)
        self.assertIn('ingestion_completed_documents_per_second', code)
        self.assertIn('completed as f64/s', code)
        self.assertIn('r.body["dataset_id"] == i.dataset_id', code)

    def test_private_artifacts_and_bounded_mock_are_explicit(self):
        io = (MODULE/'evidence.rs').read_text()
        for value in ['O_NOFOLLOW', '0o077', '0o600','0o700','fresh_external_output_required','external_output_required']:
            self.assertIn(value, io)
        mock = (MODULE/'mock.rs').read_text()
        for value in ['local_address(address)', 'peers.contains', 'max_requests','mock_auth_rejected','mock_missing_current_run_correlation']:
            self.assertIn(value, mock)

    def test_make_and_native_negative_tests_are_wired_without_claiming_execution(self):
        make = (ROOT/'Makefile').read_text()
        for target in ['performance-baseline-plan:', 'performance-baseline-run:', 'performance-baseline-l0:']:
            self.assertIn(target, make)
        source = (MODULE/'tests.rs').read_text()
        for test in ['signatures_require_external_key_exact_bytes_and_domain',
                     'signed_false_stale_or_different_profile_does_not_admit',
                     'trace_must_match_live_instance_and_same_request',
                     'ingestion_batch_rate_uses_measured_wall_not_average_inverse_latencies',
                     'partial_transport_keeps_received_200_and_body_failure']:
            self.assertIn(test, source)
        self.assertIn('CARGO_BIN_EXE_platform-performance-baseline', (TOOLS/'tests/performance_baseline_cli.rs').read_text())
        doc = (ROOT/'docs/performance/native-baseline-controller.md').read_text()
        self.assertIn('No Cargo, Docker, network, real-provider or live baseline run', doc)

if __name__ == '__main__':
    unittest.main(verbosity=2)
