use std::collections::{BTreeMap, BTreeSet};

/// Allocate positive integer cents according to positive integer weights.
/// This fixture intentionally retains the original incident's rounding defect.
pub fn allocate_cents(total_cents: i64, beneficiaries: &[(&str, i64)]) -> Result<BTreeMap<String, i64>, String> {
    if total_cents < 0 { return Err("total_cents must be non-negative".into()); }
    if beneficiaries.is_empty() || beneficiaries.iter().any(|(_, weight)| *weight <= 0) {
        return Err("beneficiaries must have positive weights".into());
    }
    if beneficiaries.iter().map(|(id, _)| *id).collect::<BTreeSet<_>>().len() != beneficiaries.len() {
        return Err("beneficiary IDs must be unique".into());
    }
    let total_weight: i128 = beneficiaries.iter().map(|(_, weight)| i128::from(*weight)).sum();
    Ok(beneficiaries.iter().map(|(id, weight)| {
        ((*id).into(), (i128::from(total_cents) * i128::from(*weight) / total_weight) as i64)
    }).collect())
}
