//! Resolve the immutable producer's child module after mounting it privately.
#[path = "fingerprint/values.rs"]
mod frozen_values;
pub use frozen_values::{canonical_digest, canonical_value, python_json};
