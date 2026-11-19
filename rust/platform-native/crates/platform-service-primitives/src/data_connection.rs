//! Explicit file-secret PostgreSQL policy, role pools and transaction-local tenancy.
//! No environment reads, socket creation or schema mutation at construction.
mod config;
mod executor;
mod tenant;
pub use config::{
    ConnectionConfig, ConnectionMode, ConnectionPurpose, DataError, DatabaseRole, TlsFiles,
    read_material,
};
pub use executor::{TenantExecutor, tenant_executor};
pub use tenant::{
    TenantPool, TenantScope, TenantTransaction, bind_transaction, current_tenant_scope,
    validate_tenant_binding, with_tenant_scope,
};
#[cfg(test)]
mod real_tests;
#[cfg(test)]
mod tests;

pub mod append;

#[cfg(test)]
mod append_real_tests;

pub mod append_store;

/// Aggregate physical role/frontend budgets across all declared replica pools.
pub mod pooling;
