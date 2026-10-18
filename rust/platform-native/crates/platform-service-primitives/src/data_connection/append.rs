//! Atomic append compatibility entrypoint for partition successors. No direct
//! registry privileges, no retry, no independent transaction or quota commit.
use super::{TenantScope, TenantTransaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppendMode {
    Insert,
    Ignore,
    SameOrError,
    UsageMerge,
}
impl AppendMode {
    fn sql(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Ignore => "ignore",
            Self::SameOrError => "same_or_error",
            Self::UsageMerge => "usage_merge",
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppendOutcome {
    pub inserted: bool,
    pub outcome: String,
    pub row: Option<Value>,
}
impl AppendOutcome {
    /// Retired keys and duplicate writes never authorize another billable
    /// accounting/admission stage. The caller must use its existing durable
    /// checkpoint/receipt contract in this same transaction.
    pub fn newly_inserted(&self) -> bool {
        self.inserted && self.outcome == "inserted"
    }
    pub fn retained_identity(&self) -> bool {
        !self.inserted && self.outcome == "retained_identity"
    }
}
#[derive(Debug)]
pub enum AppendError {
    Scope,
    Submission,
    Contract,
}
impl std::fmt::Display for AppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Scope => "append scope or request invalid",
            Self::Submission => {
                "append failed; roll back transaction and do not automatically replay"
            }
            Self::Contract => "append outcome contract invalid",
        })
    }
}
impl std::error::Error for AppendError {}
// Keep family identifiers explicit; table names never come from request bodies.
pub const FAMILIES: &[&str] = &[
    "gateway.usage_records",
    "gateway.request_traces",
    "gateway.audit_logs",
    "gateway.billing_events",
    "gateway.agent_trace_spans",
    "gateway.agent_trace_events",
    "gateway.tool_audit_log",
    "gateway.login_audit",
    "gateway.service_health_records",
    "assistant.assistant_runtime_items",
    "assistant.assistant_audit_events",
    "assistant.assistant_capability_events",
    "knowledge.kb_document_progress_events",
    "assistant.assistant_runtime_model_calls",
    "assistant.assistant_runtime_model_leases",
    "gateway.knowledge_model_calls",
    "gateway.task_reconciliation_audit",
    "gateway.price_book_v2_activation_audit",
    "gateway.price_book_v2_import_audit",
    "gateway.price_book_v2_usage_receipts",
    "gateway.price_book_v2_accounting_checkpoints",
    "gateway.agent_traces",
];
pub fn validate_request(
    scope: &TenantScope,
    family: &str,
    row: &Value,
    mode: AppendMode,
    index: Option<&str>,
) -> Result<(), AppendError> {
    if !FAMILIES.contains(&family)
        || !row.is_object()
        || row
            .get("tenant_id")
            .and_then(Value::as_str)
            .is_some_and(|t| t != scope.as_str())
        || serde_json::to_vec(row)
            .map_err(|_| AppendError::Scope)?
            .len()
            > 16 * 1024 * 1024
    {
        return Err(AppendError::Scope);
    }
    if matches!(mode, AppendMode::SameOrError | AppendMode::UsageMerge)
        && index.is_none_or(|v| {
            v.is_empty()
                || v.len() > 63
                || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
    {
        return Err(AppendError::Scope);
    }
    if mode == AppendMode::UsageMerge
        && (family != "gateway.usage_records"
            || index != Some("idx_usage_records_request_identity")
            || row.get("tenant_id").and_then(Value::as_str) != Some(scope.as_str())
            || row
                .get("request_id")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty))
    {
        return Err(AppendError::Scope);
    }
    Ok(())
}
pub async fn execute(
    transaction: &mut TenantTransaction,
    family: &str,
    row: &Value,
    mode: AppendMode,
    index: Option<&str>,
) -> Result<AppendOutcome, AppendError> {
    validate_request(transaction.scope(), family, row, mode, index)?;
    transaction
        .verify_context()
        .await
        .map_err(|_| AppendError::Scope)?;
    let value: Value = sqlx::query_scalar("SELECT partition_ops.write_json($1,$2,$3,$4)")
        .bind(family)
        .bind(row)
        .bind(mode.sql())
        .bind(index)
        .fetch_one(transaction.connection())
        .await
        .map_err(|_| AppendError::Submission)?;
    let outcome: AppendOutcome =
        serde_json::from_value(value).map_err(|_| AppendError::Contract)?;
    if !matches!(
        outcome.outcome.as_str(),
        "inserted" | "duplicate" | "existing" | "merged" | "retained_identity" | "pricing_conflict"
    ) || outcome.inserted != (outcome.outcome == "inserted")
    {
        return Err(AppendError::Contract);
    }
    Ok(outcome)
}
/// Exact existing partial request identity: NULL and empty request IDs do not
/// participate; nullable service/type use COALESCE('', ...), tenant is retained.
pub fn usage_identity<'a>(
    tenant: &'a str,
    request: Option<&'a str>,
    service: Option<&'a str>,
    kind: Option<&'a str>,
) -> Option<[&'a str; 4]> {
    Some([
        tenant,
        request.filter(|r| !r.is_empty())?,
        service.unwrap_or(""),
        kind.unwrap_or(""),
    ])
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn global_usage_identity_has_no_time_and_preserves_partial_null_semantics() {
        assert!(usage_identity("a", None, None, None).is_none());
        assert!(usage_identity("a", Some(""), None, None).is_none());
        assert_eq!(
            usage_identity("a", Some("r"), None, None),
            usage_identity("a", Some("r"), Some(""), Some(""))
        );
        assert_ne!(
            usage_identity("a", Some("r"), None, None),
            usage_identity("b", Some("r"), None, None)
        );
    }
    #[test]
    fn tenant_injection_and_wrong_global_conflict_target_are_refused() {
        let scope = TenantScope::new("tenant-a").unwrap();
        let row = json!({"tenant_id":"tenant-a","request_id":"r"});
        assert!(
            validate_request(
                &scope,
                "gateway.usage_records",
                &row,
                AppendMode::UsageMerge,
                Some("idx_usage_records_request_identity")
            )
            .is_ok()
        );
        assert!(
            validate_request(
                &scope,
                "gateway.usage_records",
                &json!({"tenant_id":"tenant-b"}),
                AppendMode::Insert,
                None
            )
            .is_err()
        );
        assert!(
            validate_request(
                &scope,
                "gateway.usage_records;DROP SCHEMA gateway",
                &row,
                AppendMode::Insert,
                None
            )
            .is_err()
        );
        assert!(
            validate_request(
                &scope,
                "gateway.usage_records",
                &row,
                AppendMode::UsageMerge,
                Some("usage_records_pkey")
            )
            .is_err()
        );
    }
    #[test]
    fn tombstones_never_authorize_repeat_accounting() {
        let result = AppendOutcome {
            inserted: false,
            outcome: "retained_identity".into(),
            row: None,
        };
        assert!(result.retained_identity());
        assert!(!result.newly_inserted());
    }
}
