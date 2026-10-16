//! Native evaluation fixture validation, recording and regression gates.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};
pub mod bootstrap;
pub mod canonical;
pub mod cli;
pub mod golden;
pub mod golden_cli;
pub mod golden_gate;
pub mod golden_stateful;
pub mod golden_validation;
pub mod judges;
pub mod live;
pub mod metrics;
pub mod producer;
pub mod parity_benchmark;
pub mod cra;
pub mod assistant_capability;
pub mod resource_stats;
pub mod resource_profile;
pub mod parity_support;
pub mod rag;
pub mod record;
pub mod runtime_regression;
pub mod studio;
pub mod strict_json;
pub mod failure_recovery;
pub mod finance;
pub mod ttft_stats;
pub mod ttft;
pub mod scenario;
pub mod scenario_contract;
pub mod scenario_events;
pub mod scenario_evidence;
pub mod scenario_runner;
pub mod seals;
pub mod coding_receipt;
pub mod coding_sandbox;
pub mod seed;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub fn snapshot(path: impl AsRef<Path>) -> Result<(Vec<Value>, String)> {
    let bytes = std::fs::read(path)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let text = std::str::from_utf8(&bytes)?;
    let mut rows = vec![];
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: Value = serde_json::from_str(line)
            .map_err(|e| format!("Invalid JSONL at line {}: {e}", index + 1))?;
        if !row.is_object() {
            return Err(format!("JSONL row {} must be an object", index + 1).into());
        }
        rows.push(row);
    }
    Ok((rows, hash))
}
pub fn observations(rows: &[Value]) -> Result<BTreeMap<String, Value>> {
    let mut map = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let id = row["case_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("Observation at line {} requires case_id", index + 1))?;
        let replay = &row["replay"];
        if !replay.as_object().is_some_and(|v| !v.is_empty()) {
            return Err(format!("Observation '{id}' requires replay").into());
        }
        if map.insert(id.into(), replay.clone()).is_some() {
            return Err(format!("Duplicate observation case_id '{id}'").into());
        }
    }
    Ok(map)
}
pub fn write_report(path: impl AsRef<Path>, value: &Value) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}
pub(crate) fn nonempty(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.trim().is_empty())
}
pub(crate) fn string_list(v: &Value, allow_empty: bool) -> bool {
    v.as_array()
        .is_some_and(|a| (allow_empty || !a.is_empty()) && a.iter().all(nonempty))
}
pub(crate) fn validation(count: usize, errors: Vec<Value>) -> Value {
    json!({"valid":errors.is_empty(),"case_count":count,"errors":errors})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_duplicate_observations() {
        let rows = vec![json!({"case_id":"x","replay":{"status":"succeeded"}}); 2];
        assert!(observations(&rows).is_err());
    }
}

pub mod runtime_contract;

pub mod judge_receipts;
