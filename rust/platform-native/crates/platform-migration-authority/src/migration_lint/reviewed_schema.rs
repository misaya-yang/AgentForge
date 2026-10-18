//! Source-owned schema facts, never sequence-only exemptions.029's four validated
//! CHECK widenings preserve the old token/request branch; no object/data is removed.
use super::{ChangeSpec, TransactionMode};
use sha2::{Digest, Sha256};
const SQL_029: &str = "8652f2b46e760a6c37947f5e56540219037a0b90eb456a73d438fc7f9ce96745";
const SPEC_029: &str = "7efe50a50a4456ffff4ac897a3499a949d61a34de6d504212c9b880bd76ea1bf";
pub(super) fn exact029(sql: &str, change: &ChangeSpec) -> bool {
    change.sequence == 29
        && change.sha256 == SQL_029
        && change.transaction_mode == TransactionMode::Transactional
        && format!("{:x}", Sha256::digest(sql.as_bytes())) == SQL_029
        && serde_json::to_vec(change)
            .ok()
            .is_some_and(|v| format!("{:x}", Sha256::digest(v)) == SPEC_029)
}
pub(super) fn grant_body(known029: bool, statement: usize) -> bool {
    known029 && matches!(statement, 12 | 28 | 37)
}
pub(super) fn validated_widening(known029: bool, statement: usize) -> bool {
    known029 && matches!(statement, 22 | 25 | 40 | 43)
}
pub(super) fn new_empty_table_alter(known029: bool, statement: usize) -> bool {
    known029 && matches!(statement, 29 | 30)
}
///032 validates the exact two031 checks before its one SET NOT NULL. Its full
/// immutable recipe/spec/hash and owner/precondition/transaction contract match.
pub(super) fn staged_not_null(sql: &str, change: &ChangeSpec) -> bool {
    matches!(change.sequence, 32 | 34)
        && ai_platform_migration_authority::source_tools::service_successors::recognizes_change(
            change,
        )
        && format!("{:x}", Sha256::digest(sql.as_bytes())) == change.sha256
}

/// Only source035's complete immutable installer may define the reviewed owner
/// maintenance routines and perform its exact ACL-only DO blocks. Their dynamic
/// DDL remains dormant function source; no operation is invoked during migration.
/// Timeout, authorization, transaction and N-1 checks still run independently.
pub(super) fn partition_installer(sql: &str, change: &ChangeSpec) -> bool {
    change.sequence == 35
        && ai_platform_migration_authority::source_tools::service_successors::recognizes_change(
            change,
        )
        && format!("{:x}", Sha256::digest(sql.as_bytes())) == change.sha256
}
