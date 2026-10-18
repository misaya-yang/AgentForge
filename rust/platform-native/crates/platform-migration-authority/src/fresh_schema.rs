//! Explicit user-authorized fresh development engineering mode.
//! A new empty database receives all current definitions in one transaction.
//! Historical upgrade proofs, retirement evidence and N-1 runs are not inputs.
use crate::{AuthorityError, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
pub const DATABASE: &str = "gateway_dev_fresh";
pub const EPOCH: i32 = 35;
const SOURCE: &[u8] = include_bytes!("fresh_schema/source.json");
const TABLE: &str = "platform_fresh_schema_state";
#[path = "fresh_schema/acceptance.rs"]
mod acceptance;
#[path = "fresh_schema/admission.rs"]
mod admission;
#[path = "fresh_schema/catalog.rs"]
mod catalog;
#[path = "fresh_schema/execution.rs"]
mod execution;
pub use acceptance::acceptance;
#[path = "fresh_schema/business.rs"]
mod business;
#[path = "fresh_schema/extension_acl.rs"]
mod extension_acl;
#[path = "fresh_schema/forward.rs"]
mod forward;
#[path = "fresh_schema/install.rs"]
mod install;
#[path = "fresh_schema/operator.rs"]
mod operator;
#[path = "fresh_schema/source.rs"]
mod source;
pub use admission::{admit_if_fresh, validate_snapshot};
pub use install::{initialize, verify};
pub use operator::{LocalOperator, initialize_with_local_operator};
pub use source::{source_contract, source_inventory};
fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(format!("fresh development schema: {message}"))
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn source_sha256() -> String {
    sha(SOURCE)
}
pub fn admission_contract() -> Value {
    json!({"schema_version":"agentforge/fresh-schema-admission/v1",
        "mode":"fresh-development", "database":DATABASE, "schema_epoch":EPOCH,
        "source_contract_sha256":source_sha256(), "catalog_matches":true,
        "transactional_initialization":true, "historical_upgrade_acceptance":false})
}
#[cfg(test)]
#[path = "fresh_schema/tests.rs"]
mod tests;
