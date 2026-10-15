"""Source-only admission checks; PostgreSQL/HTTP acceptance is a separate gate."""
from pathlib import Path
import unittest
import re

ROOT = Path(__file__).resolve().parents[2]
GATEWAY = ROOT / "rust/platform-native/crates/platform-gateway/src/migration_agent"
RUNTIME = ROOT / "rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/src"


class NativeApprovalReceiptReadbackContract(unittest.TestCase):
    def test_both_public_routes_retain_auth_before_receipt_readback(self):
        source = (GATEWAY / "routes.rs").read_text()
        for marker in ['("POST",["approvals",approval_id,"decision"])', '("POST",["approvals",id])']:
            route = source.split(marker, 1)[1].split('\n', 1)[0]
            self.assertLess(route.index("sources.require_conversation"), route.index("read_approval_decision"))
            self.assertIn('if !receipt_only{let preview=approval::owner_approval_preview', route)
            self.assertIn('APPROVAL_ACTION_UNVERIFIED', route)
        self.assertIn('read_approval_decision(&scope,&thread.runtime_thread_id,approval_id', source)
        self.assertIn('read_approval_decision(&scope,text(&row,"harness_thread_id"),id', source)
        v1 = source.split('("POST",["approvals",id])', 1)[1].split('\n', 1)[0]
        reason_check = v1.index('bounded(&payload,"reason",0,usize::MAX,false)?')
        self.assertLess(v1.index('sdk_approvals::decide'), reason_check)
        self.assertLess(reason_check, v1.index('read_approval_decision'))

    def test_missing_receipt_cannot_fall_through_to_mutation(self):
        source = (RUNTIME / "postgres_store/approval_decision.rs").read_text()
        read = source.index('let saved = sqlx::query(')
        resolve = re.search(r'if let Some\(event\) =\s*resolve_decision_receipt\(', source).start()
        self.assertLess(source.index('validate_receipt_thread(receipt_only_thread, root_id)?'), read)
        self.assertLess(source.index('validate_required('), read)
        self.assertLess(read, resolve)
        self.assertLess(resolve, source.index('UPDATE assistant_tool_approvals'))
        self.assertIn('None if receipt_only => Err(conflict("approval decision receipt missing"))', source)
        self.assertIn('!same_decision(&saved.event, proposed)', source)
        self.assertIn('saved_id != event_id', source)

    def test_receipt_mode_cannot_reach_native_resolution_or_wakeup(self):
        source = (RUNTIME / "approval_control.rs").read_text()
        dynamic = source.index('if receipt_only_thread.is_some()')
        self.assertLess(dynamic, source.index('resolve_server_request_async('))
        block = source[dynamic:source.index('// Legacy native requests still require')]
        self.assertIn('receipt_only_thread,', block)
        self.assertIn('if saved.changed {', block)
        self.assertIn('return Ok(ApprovalProjection', block)
        http = (RUNTIME / 'http_service/approvals.rs').read_text()
        self.assertIn('if body.receipt_only.is_none() {\n        let _ = state.events.send', http)
        self.assertLess(http.index('authorize(&headers'), http.index('.decide('))

    def test_normal_wire_body_is_unchanged_and_readback_has_no_retry_fallback(self):
        source = (GATEWAY / 'control.rs').read_text()
        block = source.split('async fn submit_approval_decision(', 1)[1].split('pub async fn complete_run', 1)[0]
        self.assertIn('if let Some(thread) = receipt_only_thread', block)
        self.assertIn('payload["receipt_only"] = json!({"thread_id":super::uuid(thread)?})', block)
        self.assertEqual(block.count('.send()'), 1)
        self.assertNotIn('decide_approval(', block)


if __name__ == '__main__':
    unittest.main()
