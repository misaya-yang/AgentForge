//! Frozen externally computed proof fixture; exercises canonical bytes across extraction.
use ai_platform_knowledge_domain::migration_content::capabilities::{CapabilityScope, verify_capability_proof};
use serde_json::json;
const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
const PATH: &str = "/internal/v2/capabilities/knowledge/ds/retrieve";
const HEADER: &str = "v1.eyJib2R5X3NoYTI1NiI6ImU5M2Q3YTZiN2FlN2U2N2ViOTg2NGE4NTFhNzBmNGJhZWMxNTdjMWRiNWEyOTM0ZTA2YTdlNTZjNzk2MGIyZjEiLCJleGVjdXRpb25faWQiOiJleGVjLWEiLCJleHBpcmVzX2F0IjoxMTAwLCJtZXRob2QiOiJQT1NUIiwibm9uY2UiOiJub25jZS1maXhlZCIsInBhdGgiOiIvaW50ZXJuYWwvdjIvY2FwYWJpbGl0aWVzL2tub3dsZWRnZS9kcy9yZXRyaWV2ZSIsInJ1bl9pZCI6InJ1bi1hIiwic2NoZW1hX3ZlcnNpb24iOiJhaS1wbGF0Zm9ybS1jYXBhYmlsaXR5LXByb29mL3YxIiwic2Vzc2lvbl9pZCI6InNlc3Npb24tYSIsInNpZ25hdHVyZSI6IjI1MGQxODJiMTFmY2M4OGMxMDU3OGMxNDFlZTgwYzkzYmMxMjhkNDYzYWM1YTk2MTBiZTNjNDE0YjEwOGJmZDkiLCJ0ZW5hbnRfaWQiOiJ0ZW5hbnQtYSIsInVzZXJfaWQiOiJ1c2VyLWEifQ";
fn scope() -> CapabilityScope {
    CapabilityScope { tenant_id: "tenant-a".into(), user_id: "user-a".into(), session_id: "session-a".into(), execution_id: "exec-a".into(), run_id: "run-a".into(), tool_call_id: String::new(), arguments_hash: String::new() }
}
#[test]
fn frozen_python_hmac_and_unicode_body_remain_wire_compatible() {
    let proof = verify_capability_proof(SECRET, HEADER, "POST", PATH, &json!({"query":"tenant α","top_k":5}), &scope(), 1000).unwrap();
    assert_eq!(proof["nonce"], "nonce-fixed");
}
#[test]
fn tenant_body_path_method_expiry_and_secret_changes_fail_closed() {
    let body = json!({"query":"tenant α","top_k":5});
    let mut other = scope(); other.tenant_id = "tenant-b".into();
    assert!(verify_capability_proof(SECRET, HEADER, "POST", PATH, &body, &other, 1000).is_err());
    assert!(verify_capability_proof(SECRET, HEADER, "POST", PATH, &json!({"query":"changed","top_k":5}), &scope(), 1000).is_err());
    assert!(verify_capability_proof(SECRET, HEADER, "GET", PATH, &body, &scope(), 1000).is_err());
    assert!(verify_capability_proof(SECRET, HEADER, "POST", "/different", &body, &scope(), 1000).is_err());
    assert!(verify_capability_proof(SECRET, HEADER, "POST", PATH, &body, &scope(), 1100).is_err());
    assert!(verify_capability_proof(b"different-secret-at-least-32-bytes", HEADER, "POST", PATH, &body, &scope(), 1000).is_err());
}
