use settlement_incident_fixture::SettlementService;
use serde_json::{from_str,Value};
use std::sync::Arc;
#[test]
fn reordered_retry_is_idempotent_and_balanced_end_to_end() {
    let mut service = SettlementService::default();
    let first:Value=from_str(r#"{"tenant_id":"tenant-a","external_id":"settlement-9001","total_cents":10,"metadata":{"window":{"end":"12:00","start":"11:00"},"region":"us"}}"#).unwrap();
    let replay:Value=from_str(r#"{"metadata":{"region":"us","window":{"start":"11:00","end":"12:00"}},"total_cents":10,"external_id":"settlement-9001","tenant_id":"tenant-a"}"#).unwrap();
    let beneficiaries=[("merchant-c",1),("merchant-a",1),("merchant-b",1)];
    let first=service.settle(&first,&beneficiaries).unwrap();
    let replay=service.settle(&replay,&beneficiaries).unwrap();
    assert!(Arc::ptr_eq(&first,&replay));
    assert_eq!(service.ledger.count(),1);
    assert_eq!(first.allocations,[("merchant-a".into(),4),("merchant-b".into(),3),("merchant-c".into(),3)].into());
    assert_eq!(first.allocations.values().sum::<i64>(),10);
}
