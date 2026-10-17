//! The native implementation of the existing immutable database authority.
//! No alternative schema ledger or migration lock is introduced.
pub mod bootstrap;
pub mod commands;
pub mod credential_bootstrap;
pub mod credentials;
pub mod distribution;
pub mod ledger;
pub mod manifest;
pub mod runner;
pub mod sql;

pub type Result<T> = std::result::Result<T, AuthorityError>;
#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Blocked(String),
    // Never include driver Display text: authentication errors can contain DSNs.
    #[error("database authority operation failed")]
    Database(#[from] sqlx::Error),
    #[error("database authority artifact could not be read")]
    Io(#[from] std::io::Error),
}
pub const DEFAULT_BASELINE_ID: &str = "2026_08_post_kb_v1";
pub const DEFAULT_ROLE_PREFIX: &str = "ai_gateway_";
pub const LOCK_NAMESPACE: i32 = 1_095_781_959;
pub const LOCK_ID: i32 = 1;

pub mod fingerprint;
pub mod ledger_acl;

pub mod legacy;

pub mod recovery;

pub mod numeric;

pub mod source_tools;

pub mod application_guard;

mod producer_provenance;
