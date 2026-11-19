use settlement_incident_fixture::idempotency::request_fingerprint;
use serde_json::{json,from_str,Value};
#[test]
fn fingerprint_is_stable_across_nested_object_order() {
    let first:Value=from_str(r#"{"tenant_id":"tenant-a","external_id":"payout-1042","metadata":{"invoice":{"currency":"USD","country":"US"},"source":"reconciliation"}}"#).unwrap();
    let replay:Value=from_str(r#"{"external_id":"payout-1042","metadata":{"source":"reconciliation","invoice":{"country":"US","currency":"USD"}},"tenant_id":"tenant-a"}"#).unwrap();
    assert_eq!(request_fingerprint(&first).unwrap(),request_fingerprint(&replay).unwrap());
}
#[test]
fn fingerprint_remains_tenant_scoped() {
    assert_ne!(request_fingerprint(&json!({"external_id":"same-external-id","metadata":{"batch":7},"tenant_id":"tenant-a"})).unwrap(),request_fingerprint(&json!({"external_id":"same-external-id","metadata":{"batch":7},"tenant_id":"tenant-b"})).unwrap());
}
