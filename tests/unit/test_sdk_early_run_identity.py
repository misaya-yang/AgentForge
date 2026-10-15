"""Structural source guards only; Rust behavior tests require the crate test gate."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
ADAPTERS = ROOT / 'rust/platform-native/crates/platform-gateway-integrations/src/migration_adapters'
NATIVE = (ADAPTERS / 'native_sdk.rs').read_text().split('#[cfg(test)]')[0]
RUST_TESTS = (ADAPTERS / 'native_sdk_tests.rs').read_text()
METADATA = NATIVE.split('fn metadata(', 1)[1].split('fn sdk_usage(', 1)[0]
READ = NATIVE.split('async fn read(', 1)[1].split('impl DispatchProducer', 1)[0]


class SdkEarlyRunIdentitySourceContract(unittest.TestCase):
    def test_preoutput_chunk_is_immediate_nonterminal_and_one_shot(self):
        identity = READ.split('if self.index==0 && self.capability.is_some() {', 1)[1].split('\n        loop {', 1)[0]
        self.assertIn('json!(""),StreamEventType::TextDelta,false', identity)
        self.assertIn('metadata(self.framework,self.knowledge,self.capability.as_ref())', identity)
        self.assertIn('self.index+=1;return Ok(Some(part));', identity)
        for forbidden in ('await', 'self.lines', 'self.audit', '.complete(', '"usage"', 'self.text'):
            self.assertNotIn(forbidden, identity)
        self.assertLess(READ.index('if self.done'), READ.index('if self.index==0'))
        self.assertLess(READ.index('return Ok(Some(part))'), READ.index('self.lines.next().await'))

    def test_worker_headers_and_durable_admission_remain_before_publication(self):
        stream = NATIVE.split('fn stream(', 1)[1].split('fn health_check', 1)[0]
        order = ['self.prepare(&request,true).await?', 'UsageAudit::admit',
                 'self.http.post_json("v1/runs"', 'Lines::new(response)', 'Box::new(NativeSdkStream']
        positions = [stream.index(item) for item in order]
        self.assertEqual(positions, sorted(positions))
        self.assertIn('index:0, done:false', stream)
        self.assertNotIn('uuid::', stream)

    def test_metadata_identity_comes_only_from_authoritative_lease(self):
        self.assertIn('if let Some(lease)=capability', METADATA)
        self.assertIn('meta.insert("assistant_run_id".into(),lease.bridge["run_id"].clone())', METADATA)
        self.assertIn('"sdk_framework":framework', METADATA)
        for forbidden in ('request_id', 'session_id', 'uuid::', 'sdk_lifecycle', '"usage"', '"grant"'):
            self.assertNotIn(forbidden, METADATA)

    def test_lifecycle_retains_run_and_tool_guards_and_gains_top_level_identity(self):
        lifecycle = READ.split('if matches!(event,"approval_required"', 1)[1].split('let text =', 1)[0]
        self.assertIn('value["run_id"]!=capability.bridge["run_id"]', lifecycle)
        self.assertIn('tools.is_empty()', lifecycle)
        self.assertIn('!value["tool_call_id"].is_string()', lifecycle)
        self.assertIn('metadata(self.framework,self.knowledge,self.capability.as_ref())', lifecycle)
        self.assertIn('meta.insert("sdk_lifecycle".into(),value)', lifecycle)

    def test_terminal_and_usage_ownership_stays_single(self):
        self.assertEqual(READ.count('self.audit.complete(&mut usage).await'), 1)
        self.assertEqual(READ.count('capability.complete();'), 1)
        self.assertEqual(READ.count('meta.insert("usage"'), 1)
        self.assertIn('self.done=true; self.lines.close()', READ)
        self.assertIn('if let Some(usage)=terminal_usage', READ)
        self.assertIn('audit.remove("assistant_run_id")', NATIVE)

    def test_behavioral_regressions_are_socket_free_and_cover_required_cases(self):
        for name in (
            'identity_precedes_first_body_poll_for_every_framework_and_profile',
            'identity_preserves_content_order_and_single_terminal_usage',
            'every_governed_lifecycle_chunk_has_the_same_trusted_top_level_identity',
            'lifecycle_still_rejects_unbound_run_and_zero_tool_authority',
            'missing_lease_never_fabricates_an_identity_chunk',
            'completion_without_deltas_still_follows_identity_exactly_once',
        ):
            self.assertIn('async fn ' + name + '()', RUST_TESTS)
        self.assertIn('stream.next().now_or_never()', RUST_TESTS)
        self.assertIn('polls.load(Ordering::SeqCst), 0', RUST_TESTS)
        self.assertIn('for_stream_test().await', RUST_TESTS)
        self.assertNotIn('TcpListener', RUST_TESTS)
        self.assertIn('#[path = "native_sdk_tests.rs"]', (ADAPTERS / 'native_sdk.rs').read_text())


if __name__ == '__main__':
    unittest.main()
