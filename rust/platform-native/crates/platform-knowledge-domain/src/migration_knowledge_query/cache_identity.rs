//! Exact cache authority/input identity. No policy result is cached by this module.
use super::types::UserContext;
use serde_json::json;
use sha2::{Digest, Sha256};
pub(super) fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
pub(super) fn input_hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
pub(super) fn acl_scope_hash(user: &UserContext) -> String {
    let mut roles = user.roles.clone();
    roles.sort();
    roles.dedup();
    input_hash(&json!({"tenant":user.tenant_id,"user":user.user_id,"roles":roles,"tier":user.user_tier,"type":user.user_type,"authenticated":user.is_authenticated}).to_string())
}
