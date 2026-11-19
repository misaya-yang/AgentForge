//! Opt-in versioned routing, over existing authorized catalog snapshots.
//! No dispatch, retries, credential lookup, quota or billing authority lives here.
// SPDX-License-Identifier: Apache-2.0
mod evaluate;
mod model;
pub use evaluate::{evaluate, permits_next_attempt};
pub use model::*;
#[cfg(test)]
mod tests;
