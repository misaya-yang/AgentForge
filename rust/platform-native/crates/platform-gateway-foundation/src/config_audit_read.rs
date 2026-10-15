//! Bounded metadata-only reads of persisted configuration-change audit events.
//! Uses the existing cross-tenant administrator gate in tenant_policies_api.
//! tenant_id is the recorded ACTOR tenant, not a resource ownership assertion.
use crate::{
    service_config_api::ServiceConfigError,
    tenant_policies_store::{TenantPoliciesStatement, TenantPoliciesStatementKind},
};
use serde_json::{Value, json};

#[derive(Clone)]
pub struct ConfigAuditQuery {
    pub tenant_id: Option<String>,
    pub user_id: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub action: Option<String>,
    pub limit: Value,
    pub offset: Value,
}

pub fn statement(query: &ConfigAuditQuery) -> Result<(TenantPoliciesStatement, usize, i64), ServiceConfigError> {
    let bounded = |value: &Value, name: &str, min: i64, max: i64| {
        let parsed = value.as_i64().or_else(|| value.as_str().and_then(|v| v.parse::<i64>().ok()));
        parsed.filter(|v| (min..=max).contains(v)).ok_or_else(|| {
            ServiceConfigError::detail(422, format!("{name} must be an integer from {min} to {max}"))
        })
    };
    let limit = bounded(&query.limit, "limit", 1, crate::native_contract::AUDIT_LIMIT_MAX)?;
    let offset = bounded(&query.offset, "offset", 0, crate::native_contract::AUDIT_OFFSET_MAX)?;
    let mut conditions = vec!["event_type = 'config_changed'".to_owned()];
    let mut arguments = vec![];
    for (column, value, max) in [
        ("tenant_id", &query.tenant_id, crate::native_contract::AUDIT_FILTER_LIMITS[0].1),
        ("user_id", &query.user_id, crate::native_contract::AUDIT_FILTER_LIMITS[1].1),
        ("resource_type", &query.resource_type, crate::native_contract::AUDIT_FILTER_LIMITS[2].1),
        ("resource_id", &query.resource_id, crate::native_contract::AUDIT_FILTER_LIMITS[3].1),
        ("action", &query.action, crate::native_contract::AUDIT_FILTER_LIMITS[4].1),
    ] {
        if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
            if value.chars().count() > max {
                return Err(ServiceConfigError::detail(422, format!("{column} is too long")));
            }
            arguments.push(json!(value));
            conditions.push(format!("{column} = ${}", arguments.len()));
        }
    }
    let limit_parameter = arguments.len() + 1;
    // Fetch one extra row for has_more; no separate count scan or fabricated total.
    arguments.extend([json!(limit + 1), json!(offset)]);
    Ok((TenantPoliciesStatement {
        kind: TenantPoliciesStatementKind::ConfigAuditRows,
        // Explicit allowlist excludes snapshots, arbitrary summaries, IP/UA and error text.
        // Serialize BIGSERIAL as text to preserve IDs beyond JavaScript safe integer range.
        sql: format!(
            "SELECT id::text AS id, tenant_id, user_id, resource_type, resource_id, action, status, created_at FROM audit_logs WHERE {} ORDER BY created_at DESC, id DESC LIMIT ${limit_parameter} OFFSET ${}",
            conditions.join(" AND "), limit_parameter + 1
        ),
        arguments,
    }, limit as usize, offset))
}
