//! Native companion migration of `apps/local-node/src/local_node`.
//!
//! This is a library composition surface, never an ambient listener, Agent loop,
//! implicit credential store, or replacement for the existing Local Nodes control plane.
pub mod approvals;
pub mod broker;
pub mod cli;
pub mod computer;
pub mod credentials;
pub mod database;
pub mod doctor;
pub mod errors;
pub mod execution_repository;
pub mod file_dispatch;
pub mod files;
pub mod grants;
pub mod identity;
pub mod ledger;
pub mod macos_native;
pub mod models;
pub mod outbox;
pub mod processes;
pub mod protocol;
pub mod service;
pub mod transport;
pub mod watcher;
pub mod workspace;
pub use errors::{LocalNodeError, Result};
