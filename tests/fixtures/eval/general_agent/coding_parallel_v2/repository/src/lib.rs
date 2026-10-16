pub mod allocation;
pub mod idempotency;
use std::{collections::BTreeMap, sync::Arc};
use serde_json::Value;

#[derive(Debug)]
pub struct Settlement {
    pub fingerprint: String,
    pub tenant_id: String,
    pub external_id: String,
    pub allocations: BTreeMap<String, i64>,
}
#[derive(Default)]
pub struct InMemoryLedger { records: BTreeMap<String, Arc<Settlement>> }
impl InMemoryLedger {
    pub fn get(&self, fingerprint: &str) -> Option<Arc<Settlement>> { self.records.get(fingerprint).cloned() }
    pub fn put(&mut self, settlement: Arc<Settlement>) { self.records.insert(settlement.fingerprint.clone(), settlement); }
    pub fn count(&self) -> usize { self.records.len() }
}
#[derive(Default)]
pub struct SettlementService { pub ledger: InMemoryLedger }
impl SettlementService {
    pub fn settle(&mut self, request: &Value, beneficiaries: &[(&str, i64)]) -> Result<Arc<Settlement>, String> {
        let fingerprint = idempotency::request_fingerprint(request)?;
        if let Some(existing) = self.ledger.get(&fingerprint) { return Ok(existing); }
        let settlement = Arc::new(Settlement {
            fingerprint,
            tenant_id: request["tenant_id"].as_str().ok_or("missing tenant_id")?.into(),
            external_id: request["external_id"].as_str().ok_or("missing external_id")?.into(),
            allocations: allocation::allocate_cents(request["total_cents"].as_i64().ok_or("invalid total_cents")?, beneficiaries)?,
        });
        self.ledger.put(settlement.clone());
        Ok(settlement)
    }
}
