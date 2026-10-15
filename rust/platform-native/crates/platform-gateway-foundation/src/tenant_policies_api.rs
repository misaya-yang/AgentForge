//! Canonical tenant-policy routes plus a bounded configuration-audit read route.
//!
//! Explicit platform administrator roles may administer across tenants. Ordinary
//! admin roles/tiers are limited to their authenticated tenant at the query and
//! mutation boundaries, including audit reads. Database presence is
//! required, not enabled. Optional synchronous invalidation follows committed
//! writes and can fail without undoing them; no invented cache engine/audit write.
//!
//! Reuses shared errors, request context, cancellation and MIME classification.
//! Syntax parsing precedes identity; body/query validation precedes endpoint admin
//! and DB checks. Integer/finite JSON, invalid encoding/JSON diagnostic positions,
//! decoded slash paths and trusted-origin trailing-slash redirects retain existing
//! host boundaries. Tenant payload isalnum uses Python UCD 15.0 rather than Rust's
//! broader Alphabetic property. Path/query tenant IDs are deliberately unvalidated.
use crate::{
    config_audit_writer::ConfigAuditRequestContext,
    config_audit_read::{self, ConfigAuditQuery},
    rate_config_api::integer,
    service_config_api::{ServiceConfigCancellation, ServiceConfigError, ServiceConfigFuture},
    tenant_policies_store::*,
};
use ai_platform_auth::admission::AdmissionService;
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{FromRequest, MatchedPath, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

pub const TENANT_POLICIES_ROUTES: &[(&str, &str, &str)] = &[
    (
        "GET",
        "/admin/tenant-policies/tool-policies",
        "list_tool_policies",
    ),
    (
        "GET",
        "/admin/tenant-policies/tool-policies/{tenant_id}",
        "get_tool_policy",
    ),
    (
        "PUT",
        "/admin/tenant-policies/tool-policies",
        "upsert_tool_policy",
    ),
    (
        "DELETE",
        "/admin/tenant-policies/tool-policies/{tenant_id}",
        "delete_tool_policy",
    ),
    (
        "GET",
        "/admin/tenant-policies/mcp-configs",
        "list_mcp_configs",
    ),
    (
        "GET",
        "/admin/tenant-policies/mcp-configs/{tenant_id}",
        "get_mcp_config",
    ),
    (
        "PUT",
        "/admin/tenant-policies/mcp-configs",
        "upsert_mcp_config",
    ),
    (
        "DELETE",
        "/admin/tenant-policies/mcp-configs/{tenant_id}",
        "delete_mcp_config",
    ),
    ("GET", "/admin/tenant-policies/audit-log", "query_audit_log"),
    ("GET", "/admin/tenant-policies/config-audit-log", "query_config_audit_log"),
];
#[derive(Clone, Default)]
pub struct TenantPoliciesIdentity {
    pub user_id: String,
    pub tenant_id: String,
    pub roles: Vec<String>,
    pub tier: String,
}
pub trait TenantPoliciesIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, TenantPoliciesIdentity>;
    fn context(&self, headers: &HeaderMap) -> ConfigAuditRequestContext {
        ConfigAuditRequestContext {
            headers: headers.clone(),
            ..Default::default()
        }
    }
}
impl TenantPoliciesIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, TenantPoliciesIdentity> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let user = request
                .resolve()
                .await
                .map_err(|e| ServiceConfigError::detail(e.status(), e.to_string()))?;
            Ok(TenantPoliciesIdentity {
                user_id: user.user_id().into(),
                tenant_id: user.tenant_id().into(),
                roles: user.roles().to_vec(),
                tier: user.tier().into(),
            })
        })
    }
}
pub trait TenantPolicyInvalidation: Send + Sync {
    fn invalidate(&self, tenant_id: &str) -> Result<(), ServiceConfigError>;
}
pub struct TenantPoliciesDependencies {
    pub identity: Arc<dyn TenantPoliciesIdentityResolver>,
    pub database: Option<Arc<dyn TenantPoliciesDatabase>>,
    pub tool_invalidation: Option<Arc<dyn TenantPolicyInvalidation>>,
    pub mcp_invalidation: Option<Arc<dyn TenantPolicyInvalidation>>,
}
pub struct TenantPoliciesApi {
    dependencies: TenantPoliciesDependencies,
}
#[derive(Clone)]
pub struct TenantAuditQuery {
    pub tenant_id: Option<String>,
    pub user_id: Option<String>,
    pub tool_name: Option<String>,
    pub limit: Value,
    pub offset: Value,
}
impl Default for TenantAuditQuery {
    fn default() -> Self {
        Self {
            tenant_id: None,
            user_id: None,
            tool_name: None,
            limit: json!(50),
            offset: json!(0),
        }
    }
}
#[derive(Clone)]
pub enum TenantPoliciesOperation {
    ToolList,
    ToolGet { tenant_id: String },
    ToolUpsert,
    ToolDelete { tenant_id: String },
    McpList,
    McpGet { tenant_id: String },
    McpUpsert,
    McpDelete { tenant_id: String },
    Audit(TenantAuditQuery),
    ConfigAudit(ConfigAuditQuery),
}
impl TenantPoliciesOperation {
    fn path(&self) -> String {
        let tail = match self {
            Self::ToolList | Self::ToolUpsert => "tool-policies".into(),
            Self::McpList | Self::McpUpsert => "mcp-configs".into(),
            Self::ToolGet { tenant_id } | Self::ToolDelete { tenant_id } => {
                format!("tool-policies/{tenant_id}")
            }
            Self::McpGet { tenant_id } | Self::McpDelete { tenant_id } => {
                format!("mcp-configs/{tenant_id}")
            }
            Self::Audit(_) => "audit-log".into(),
            Self::ConfigAudit(_) => "config-audit-log".into(),
        };
        format!("/api/v1/admin/tenant-policies/{tail}")
    }
    fn body(&self) -> bool {
        matches!(self, Self::ToolUpsert | Self::McpUpsert)
    }
}
fn store_error(e: TenantPoliciesStoreError) -> ServiceConfigError {
    if e == TenantPoliciesStoreError::Cancelled {
        ServiceConfigError::Cancelled
    } else {
        ServiceConfigError::Internal
    }
}
impl TenantPoliciesApi {
    pub fn new(dependencies: TenantPoliciesDependencies) -> Self {
        Self { dependencies }
    }
    pub async fn handle(
        &self,
        operation: TenantPoliciesOperation,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<Value, ServiceConfigError> {
        let identity = self
            .dependencies
            .identity
            .resolve(headers, &operation.path())
            .await?;
        self.execute(operation, &identity, body).await
    }
    pub async fn execute(
        &self,
        mut operation: TenantPoliciesOperation,
        identity: &TenantPoliciesIdentity,
        body: &Value,
    ) -> Result<Value, ServiceConfigError> {
        let payload = if operation.body() {
            Some(validate_payload(
                body,
                matches!(operation, TenantPoliciesOperation::McpUpsert),
            )?)
        } else {
            None
        };
        let platform_admin = identity.roles.iter().any(|role| matches!(
            role.trim().to_lowercase().as_str(), "platform_admin" | "superadmin" | "super_admin"
        ));
        let require_tenant = |tenant: &str| {
            if !platform_admin && (identity.tenant_id.is_empty() || tenant != identity.tenant_id) {
                Err(ServiceConfigError::detail(403, "Cross-tenant policy administration denied"))
            } else { Ok(()) }
        };
        if !platform_admin {
            use TenantPoliciesOperation as ScopeOp;
            match &mut operation {
                ScopeOp::ToolGet { tenant_id } | ScopeOp::ToolDelete { tenant_id }
                | ScopeOp::McpGet { tenant_id } | ScopeOp::McpDelete { tenant_id } => require_tenant(tenant_id)?,
                ScopeOp::ToolUpsert | ScopeOp::McpUpsert => require_tenant(
                    payload.as_ref().and_then(|value| value["tenant_id"].as_str()).unwrap_or("")
                )?,
                ScopeOp::Audit(query) => {
                    require_tenant(query.tenant_id.as_deref().filter(|v| !v.is_empty()).unwrap_or(&identity.tenant_id))?;
                    query.tenant_id = Some(identity.tenant_id.clone());
                }
                ScopeOp::ConfigAudit(query) => {
                    require_tenant(query.tenant_id.as_deref().filter(|v| !v.is_empty()).unwrap_or(&identity.tenant_id))?;
                    query.tenant_id = Some(identity.tenant_id.clone());
                }
                ScopeOp::ToolList | ScopeOp::McpList => require_tenant(&identity.tenant_id)?,
            }
        }
        let audit = if let TenantPoliciesOperation::Audit(query) = &operation {
            Some(audit_statements(query)?)
        } else {
            None
        };
        if !platform_admin && !identity.roles.iter().any(|r| r == "admin") && identity.tier.to_lowercase() != "admin" {
            return Err(ServiceConfigError::detail(403, "Admin access required"));
        }
        let db = self
            .dependencies
            .database
            .as_ref()
            .ok_or_else(|| ServiceConfigError::detail(503, "Database not available"))?;
        let statement = |kind, sql: &str, arguments| TenantPoliciesStatement {
            kind,
            sql: sql.into(),
            arguments,
        };
        use TenantPoliciesOperation as Op;
        use TenantPoliciesStatementKind as Kind;
        let tool = matches!(
            &operation,
            Op::ToolList | Op::ToolGet { .. } | Op::ToolUpsert | Op::ToolDelete { .. }
        );
        match operation {
            Op::ToolList | Op::McpList => {
                let rows = db
                    .run(statement(
                        if tool { Kind::ToolList } else { Kind::McpList },
                        if platform_admin {
                            if tool { TOOL_LIST_SQL } else { MCP_LIST_SQL }
                        } else if tool {
                            "SELECT * FROM tenant_tool_policies WHERE tenant_id = $1 ORDER BY tenant_id"
                        } else {
                            "SELECT * FROM tenant_mcp_configs WHERE tenant_id = $1 ORDER BY tenant_id"
                        },
                        if platform_admin { vec![] } else { vec![json!(identity.tenant_id)] },
                    ))
                    .await
                    .map_err(store_error)?;
                Ok(if tool {
                    json!({"policies":rows})
                } else {
                    json!({"configs":rows})
                })
            }
            Op::ToolGet { tenant_id } | Op::McpGet { tenant_id } => {
                let mut rows = db
                    .run(statement(
                        if tool { Kind::ToolGet } else { Kind::McpGet },
                        if tool { TOOL_GET_SQL } else { MCP_GET_SQL },
                        vec![json!(tenant_id)],
                    ))
                    .await
                    .map_err(store_error)?;
                if rows.is_empty() {
                    Err(ServiceConfigError::detail(
                        404,
                        if tool {
                            format!("No policy for tenant {tenant_id}")
                        } else {
                            format!("No MCP config for tenant {tenant_id}")
                        },
                    ))
                } else {
                    Ok(rows.remove(0))
                }
            }
            Op::ToolUpsert | Op::McpUpsert => {
                let value = payload.expect("validated payload");
                let tenant = value["tenant_id"].as_str().expect("tenant string");
                let args = if tool {
                    vec![
                        value["tenant_id"].clone(),
                        value["allowed_tools"].clone(),
                        value["blocked_tools"].clone(),
                        value["allowed_categories"].clone(),
                        value["max_calls_per_minute"].clone(),
                    ]
                } else {
                    let overrides = value["server_overrides"]
                        .as_object()
                        .filter(|v| !v.is_empty())
                        .map(|_| tenant_policy_json_dumps(&value["server_overrides"]));
                    vec![
                        value["tenant_id"].clone(),
                        value["allowed_servers"].clone(),
                        json!(overrides),
                        value["max_connections"].clone(),
                    ]
                };
                db.run(statement(
                    if tool {
                        Kind::ToolUpsert
                    } else {
                        Kind::McpUpsert
                    },
                    if tool {
                        TOOL_UPSERT_SQL
                    } else {
                        MCP_UPSERT_SQL
                    },
                    args,
                ))
                .await
                .map_err(store_error)?;
                self.invalidate(tool, tenant)?;
                Ok(json!({"status":"ok","tenant_id":tenant}))
            }
            Op::ToolDelete { tenant_id } | Op::McpDelete { tenant_id } => {
                db.run(statement(
                    if tool {
                        Kind::ToolDelete
                    } else {
                        Kind::McpDelete
                    },
                    if tool {
                        TOOL_DELETE_SQL
                    } else {
                        MCP_DELETE_SQL
                    },
                    vec![json!(tenant_id)],
                ))
                .await
                .map_err(store_error)?;
                self.invalidate(tool, &tenant_id)?;
                Ok(json!({"status":"deleted","tenant_id":tenant_id}))
            }
            Op::ConfigAudit(query) => {
                if !db.config_audit_available() {
                    return Err(ServiceConfigError::detail(503, "Configuration audit database not available"));
                }
                let (statement, limit, offset) = config_audit_read::statement(&query)?;
                let mut items = db.run(statement).await.map_err(store_error)?;
                let has_more = items.len() > limit;
                items.truncate(limit);
                Ok(crate::native_contract::responses::audit::value(items, has_more, limit, offset, crate::native_contract::AUDIT_OFFSET_MAX))
            }
            Op::Audit(_) => {
                let (rows, count) = audit.expect("validated audit query");
                let items = db.run(rows).await.map_err(store_error)?;
                let count = db.run(count).await.map_err(store_error)?;
                let total = match count.first() {
                    None => json!(0),
                    Some(v) if v.as_object().is_some_and(|v| v.is_empty()) => json!(0),
                    Some(v) => v.get("cnt").cloned().ok_or(ServiceConfigError::Internal)?,
                };
                Ok(json!({"total":total,"items":items}))
            }
        }
    }
    fn invalidate(&self, tool: bool, tenant: &str) -> Result<(), ServiceConfigError> {
        let cache = if tool {
            &self.dependencies.tool_invalidation
        } else {
            &self.dependencies.mcp_invalidation
        };
        if let Some(cache) = cache {
            cache.invalidate(tenant)?;
        }
        Ok(())
    }
}
fn error(kind: &str, loc: Vec<Value>, input: &Value) -> Value {
    let message = match kind {
        "missing" => "Field required",
        "string_type" => "Input should be a valid string",
        "list_type" => "Input should be a valid list",
        "dict_type" => "Input should be a valid dictionary",
        "model_attributes_type" => {
            "Input should be a valid dictionary or object to extract fields from"
        }
        "int_type" => "Input should be a valid integer",
        "int_parsing" => "Input should be a valid integer, unable to parse string as an integer",
        "int_from_float" => "Input should be a valid integer, got a number with a fractional part",
        "int_parsing_size" => "Unable to parse input string as an integer, exceeded maximum size",
        _ => "Invalid value",
    };
    json!({"type":kind,"loc":loc,"msg":message,"input":input})
}
fn bounded_integer(
    value: &Value,
    loc: Vec<Value>,
    min: i64,
    max: Option<i64>,
    errors: &mut Vec<Value>,
) -> Option<i64> {
    match integer(value) {
        Err(kind) => {
            errors.push(error(kind, loc, value));
            None
        }
        Ok(v) => {
            let bound = if v < min {
                Some(("greater_than_equal", "greater", "ge", min))
            } else {
                max.filter(|m| v > *m)
                    .map(|m| ("less_than_equal", "less", "le", m))
            };
            if let Some((kind, word, key, bound)) = bound {
                let mut e = error(kind, loc, value);
                e["msg"] = json!(format!("Input should be {word} than or equal to {bound}"));
                e["ctx"] = json!({key:bound});
                errors.push(e);
                None
            } else {
                Some(v)
            }
        }
    }
}
/// Request model validation, including field order and explicit null distinctions.
pub fn validate_payload(body: &Value, mcp: bool) -> Result<Value, ServiceConfigError> {
    let class = if mcp {
        "MCPConfigPayload"
    } else {
        "ToolPolicyPayload"
    };
    let Some(fields) = body.as_object() else {
        return Err(ServiceConfigError::validation(vec![
            json!({"type":"model_type","loc":["body"],"msg":format!("Input should be a valid dictionary or instance of {class}"),"input":body,"ctx":{"class_name":class}}),
        ]));
    };
    let mut out = if mcp {
        json!({"tenant_id":"","allowed_servers":[],"server_overrides":null,"max_connections":5})
    } else {
        json!({"tenant_id":"","allowed_tools":null,"blocked_tools":null,"allowed_categories":null,"max_calls_per_minute":20})
    };
    let mut errors = vec![];
    let order: &[&str] = if mcp {
        &[
            "tenant_id",
            "allowed_servers",
            "server_overrides",
            "max_connections",
        ]
    } else {
        &[
            "tenant_id",
            "allowed_tools",
            "blocked_tools",
            "allowed_categories",
            "max_calls_per_minute",
        ]
    };
    for field in order {
        let loc = vec![json!("body"), json!(field)];
        let Some(v) = fields.get(*field) else {
            if matches!(*field, "tenant_id" | "allowed_servers") {
                errors.push(error("missing", loc, body));
            }
            continue;
        };
        if *field == "tenant_id" {
            if let Some(s) = v.as_str() {
                let length = s.chars().count();
                if !(1..=64).contains(&length) {
                    let short = length < 1;
                    errors.push(json!({"type":if short{"string_too_short"}else{"string_too_long"},"loc":loc,"msg":if short{"String should have at least 1 character"}else{"String should have at most 64 characters"},"input":v,"ctx":if short{json!({"min_length":1})}else{json!({"max_length":64})}}));
                } else if !valid_tenant_id(s) {
                    errors.push(json!({"type":"value_error","loc":loc,"msg":"Value error, tenant_id must be alphanumeric (hyphens/underscores allowed)","input":v,"ctx":{"error":{}}}));
                } else {
                    out[*field] = v.clone();
                }
            } else {
                errors.push(error("string_type", loc, v));
            }
        } else if field.starts_with("max_") {
            if let Some(n) =
                bounded_integer(v, loc, 1, Some(if mcp { 100 } else { 10000 }), &mut errors)
            {
                out[*field] = json!(n);
            }
        } else if *field == "server_overrides" {
            if v.is_null() || v.is_object() {
                out[*field] = v.clone();
            } else {
                errors.push(error("dict_type", loc, v));
            }
        } else if v.is_null() && *field != "allowed_servers" {
            out[*field] = Value::Null;
        } else if let Some(array) = v.as_array() {
            for (i, item) in array.iter().enumerate() {
                if !item.is_string() {
                    let mut at = loc.clone();
                    at.push(json!(i));
                    errors.push(error("string_type", at, item));
                }
            }
            out[*field] = v.clone();
        } else {
            errors.push(error("list_type", loc, v));
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(ServiceConfigError::validation(errors))
    }
}
pub fn audit_statements(
    query: &TenantAuditQuery,
) -> Result<(TenantPoliciesStatement, TenantPoliciesStatement), ServiceConfigError> {
    let mut errors = vec![];
    let limit = bounded_integer(
        &query.limit,
        vec![json!("query"), json!("limit")],
        1,
        Some(1000),
        &mut errors,
    );
    let offset = bounded_integer(
        &query.offset,
        vec![json!("query"), json!("offset")],
        0,
        None,
        &mut errors,
    );
    if !errors.is_empty() {
        return Err(ServiceConfigError::validation(errors));
    }
    let mut conditions = vec![];
    let mut args = vec![];
    for (column, value) in [("tenant_id", &query.tenant_id), ("user_id", &query.user_id)] {
        if let Some(value) = value.as_ref().filter(|s| !s.is_empty()) {
            args.push(json!(value));
            conditions.push(format!("{column} = ${}", args.len()));
        }
    }
    if let Some(value) = query.tool_name.as_ref().filter(|s| !s.is_empty()) {
        let escaped = value
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        args.push(json!(format!("%{escaped}%")));
        conditions.push(format!("tool_name LIKE ${} ESCAPE '\\'", args.len()));
    }
    let clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };
    let i = args.len() + 1;
    let count = TenantPoliciesStatement {
        kind: TenantPoliciesStatementKind::AuditCount,
        sql: format!("SELECT COUNT(*) AS cnt FROM tool_audit_log {clause}"),
        arguments: args.clone(),
    };
    args.extend([json!(limit.unwrap()), json!(offset.unwrap())]);
    Ok((
        TenantPoliciesStatement {
            kind: TenantPoliciesStatementKind::AuditRows,
            sql: format!(
                "SELECT * FROM tool_audit_log {clause} ORDER BY created_at DESC LIMIT ${i} OFFSET ${}",
                i + 1
            ),
            arguments: args,
        },
        count,
    ))
}
/// Python str.isalnum, after only '-' and '_' are removed. Unicode version pinned
/// by the unchanged-source oracle; this rejects combining Other_Alphabetic marks.
pub fn valid_tenant_id(value: &str) -> bool {
    let mut any = false;
    for c in value.chars().filter(|c| !matches!(c, '-' | '_')) {
        any = true;
        let n = c as u32;
        if PYTHON_ALNUM_RANGES
            .binary_search_by(|&(a, b)| {
                if n < a {
                    std::cmp::Ordering::Greater
                } else if n > b {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_err()
        {
            return false;
        }
    }
    any
}

#[derive(Clone)]
struct HttpState {
    api: Arc<TenantPoliciesApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
}
pub fn router(
    api: Arc<TenantPoliciesApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
) -> Router {
    Router::new()
        .route("/api/v1/admin/tenant-policies/tool-policies", any(dispatch))
        .route(
            "/api/v1/admin/tenant-policies/tool-policies/{tenant_id}",
            any(dispatch),
        )
        .route("/api/v1/admin/tenant-policies/mcp-configs", any(dispatch))
        .route(
            "/api/v1/admin/tenant-policies/mcp-configs/{tenant_id}",
            any(dispatch),
        )
        .route("/api/v1/admin/tenant-policies/audit-log", any(dispatch))
        .route(crate::native_contract::CONFIG_AUDIT, any(dispatch))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn response(
    state: &HttpState,
    result: Result<Value, ServiceConfigError>,
    context: &ConfigAuditRequestContext,
) -> Response {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(ServiceConfigError::Cancelled) => state.cancellation.cancelled().await,
        Err(e) => {
            let (status, body) = e.response(context).expect("noncancelled");
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    }
}
async fn dispatch(
    State(state): State<HttpState>,
    matched: MatchedPath,
    params: Result<Path<HashMap<String, String>>, axum::extract::rejection::PathRejection>,
    request: Request,
) -> Response {
    let params = match params {
        Ok(Path(v)) => v,
        Err(e) => return e.into_response(),
    };
    if params.values().any(|v| v.contains('/')) {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    }
    let path = matched.as_str();
    let method = request.method().as_str();
    let detail = path.ends_with("/{tenant_id}");
    let config_audit = path.ends_with("/config-audit-log");
    let audit = path.ends_with("/audit-log") || config_audit;
    let allowed = method == "GET"
        || if audit {
            false
        } else if detail {
            method == "DELETE"
        } else {
            method == "PUT"
        };
    if !allowed {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", "GET")],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response();
    }
    let tool = path.contains("/tool-policies");
    let tenant_id = params.get("tenant_id").cloned().unwrap_or_default();
    let operation = if audit {
        let pairs = match Query::<Vec<(String, String)>>::try_from_uri(request.uri()) {
            Ok(Query(v)) => v,
            Err(e) => return e.into_response(),
        };
        let get = |key: &str| {
            pairs
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        if config_audit {
            TenantPoliciesOperation::ConfigAudit(ConfigAuditQuery {
                tenant_id: get("tenant_id"),
                user_id: get("user_id"),
                resource_type: get("resource_type"),
                resource_id: get("resource_id"),
                action: get("action"),
                limit: get("limit").map_or(json!(25), |v| json!(v)),
                offset: get("offset").map_or(json!(0), |v| json!(v)),
            })
        } else {
        TenantPoliciesOperation::Audit(TenantAuditQuery {
            tenant_id: get("tenant_id"),
            user_id: get("user_id"),
            tool_name: get("tool_name"),
            limit: get("limit").map_or(json!(50), |v| json!(v)),
            offset: get("offset").map_or(json!(0), |v| json!(v)),
        })
        }
    } else {
        match (method, detail, tool) {
            ("GET", false, true) => TenantPoliciesOperation::ToolList,
            ("GET", false, false) => TenantPoliciesOperation::McpList,
            ("GET", true, true) => TenantPoliciesOperation::ToolGet { tenant_id },
            ("GET", true, false) => TenantPoliciesOperation::McpGet { tenant_id },
            ("DELETE", _, true) => TenantPoliciesOperation::ToolDelete { tenant_id },
            ("DELETE", _, false) => TenantPoliciesOperation::McpDelete { tenant_id },
            (_, _, true) => TenantPoliciesOperation::ToolUpsert,
            _ => TenantPoliciesOperation::McpUpsert,
        }
    };
    let headers = request.headers().clone();
    let mut context = request
        .extensions()
        .get::<ConfigAuditRequestContext>()
        .cloned()
        .or_else(|| {
            request
                .extensions()
                .get::<Extension<ConfigAuditRequestContext>>()
                .map(|Extension(v)| v.clone())
        })
        .unwrap_or_else(|| state.api.dependencies.identity.context(&headers));
    context.headers = headers.clone();
    let body = if operation.body() {
        let bytes = match Bytes::from_request(request, &state).await {
            Ok(v) => v,
            Err(e) => return e.into_response(),
        };
        if bytes.is_empty() {
            Value::Null
        } else if crate::http_content_type::is_json_content_type(&headers) {
            match serde_json::from_slice(&bytes){Ok(v)=>v,Err(_)=>return response(&state,Err(ServiceConfigError::validation(vec![json!({"type":"json_invalid","loc":["body",0],"msg":"JSON decode error","input":{},"ctx":{"error":"Invalid JSON"}})])),&context).await}
        } else {
            json!(String::from_utf8_lossy(&bytes))
        }
    } else {
        Value::Null
    };
    let identity = match state
        .api
        .dependencies
        .identity
        .resolve(&headers, &operation.path())
        .await
    {
        Ok(v) => v,
        Err(e) => return response(&state, Err(e), &context).await,
    };
    if operation.body() && !body.is_object() {
        return response(
            &state,
            Err(ServiceConfigError::validation(vec![error(
                if body.is_null() {
                    "missing"
                } else {
                    "model_attributes_type"
                },
                vec![json!("body")],
                &body,
            )])),
            &context,
        )
        .await;
    }
    response(
        &state,
        state.api.execute(operation, &identity, &body).await,
        &context,
    )
    .await
}

const PYTHON_ALNUM_RANGES: &[(u32, u32)] = &[
    (0x30, 0x39),
    (0x41, 0x5a),
    (0x61, 0x7a),
    (0xaa, 0xaa),
    (0xb2, 0xb3),
    (0xb5, 0xb5),
    (0xb9, 0xba),
    (0xbc, 0xbe),
    (0xc0, 0xd6),
    (0xd8, 0xf6),
    (0xf8, 0x2c1),
    (0x2c6, 0x2d1),
    (0x2e0, 0x2e4),
    (0x2ec, 0x2ec),
    (0x2ee, 0x2ee),
    (0x370, 0x374),
    (0x376, 0x377),
    (0x37a, 0x37d),
    (0x37f, 0x37f),
    (0x386, 0x386),
    (0x388, 0x38a),
    (0x38c, 0x38c),
    (0x38e, 0x3a1),
    (0x3a3, 0x3f5),
    (0x3f7, 0x481),
    (0x48a, 0x52f),
    (0x531, 0x556),
    (0x559, 0x559),
    (0x560, 0x588),
    (0x5d0, 0x5ea),
    (0x5ef, 0x5f2),
    (0x620, 0x64a),
    (0x660, 0x669),
    (0x66e, 0x66f),
    (0x671, 0x6d3),
    (0x6d5, 0x6d5),
    (0x6e5, 0x6e6),
    (0x6ee, 0x6fc),
    (0x6ff, 0x6ff),
    (0x710, 0x710),
    (0x712, 0x72f),
    (0x74d, 0x7a5),
    (0x7b1, 0x7b1),
    (0x7c0, 0x7ea),
    (0x7f4, 0x7f5),
    (0x7fa, 0x7fa),
    (0x800, 0x815),
    (0x81a, 0x81a),
    (0x824, 0x824),
    (0x828, 0x828),
    (0x840, 0x858),
    (0x860, 0x86a),
    (0x870, 0x887),
    (0x889, 0x88e),
    (0x8a0, 0x8c9),
    (0x904, 0x939),
    (0x93d, 0x93d),
    (0x950, 0x950),
    (0x958, 0x961),
    (0x966, 0x96f),
    (0x971, 0x980),
    (0x985, 0x98c),
    (0x98f, 0x990),
    (0x993, 0x9a8),
    (0x9aa, 0x9b0),
    (0x9b2, 0x9b2),
    (0x9b6, 0x9b9),
    (0x9bd, 0x9bd),
    (0x9ce, 0x9ce),
    (0x9dc, 0x9dd),
    (0x9df, 0x9e1),
    (0x9e6, 0x9f1),
    (0x9f4, 0x9f9),
    (0x9fc, 0x9fc),
    (0xa05, 0xa0a),
    (0xa0f, 0xa10),
    (0xa13, 0xa28),
    (0xa2a, 0xa30),
    (0xa32, 0xa33),
    (0xa35, 0xa36),
    (0xa38, 0xa39),
    (0xa59, 0xa5c),
    (0xa5e, 0xa5e),
    (0xa66, 0xa6f),
    (0xa72, 0xa74),
    (0xa85, 0xa8d),
    (0xa8f, 0xa91),
    (0xa93, 0xaa8),
    (0xaaa, 0xab0),
    (0xab2, 0xab3),
    (0xab5, 0xab9),
    (0xabd, 0xabd),
    (0xad0, 0xad0),
    (0xae0, 0xae1),
    (0xae6, 0xaef),
    (0xaf9, 0xaf9),
    (0xb05, 0xb0c),
    (0xb0f, 0xb10),
    (0xb13, 0xb28),
    (0xb2a, 0xb30),
    (0xb32, 0xb33),
    (0xb35, 0xb39),
    (0xb3d, 0xb3d),
    (0xb5c, 0xb5d),
    (0xb5f, 0xb61),
    (0xb66, 0xb6f),
    (0xb71, 0xb77),
    (0xb83, 0xb83),
    (0xb85, 0xb8a),
    (0xb8e, 0xb90),
    (0xb92, 0xb95),
    (0xb99, 0xb9a),
    (0xb9c, 0xb9c),
    (0xb9e, 0xb9f),
    (0xba3, 0xba4),
    (0xba8, 0xbaa),
    (0xbae, 0xbb9),
    (0xbd0, 0xbd0),
    (0xbe6, 0xbf2),
    (0xc05, 0xc0c),
    (0xc0e, 0xc10),
    (0xc12, 0xc28),
    (0xc2a, 0xc39),
    (0xc3d, 0xc3d),
    (0xc58, 0xc5a),
    (0xc5d, 0xc5d),
    (0xc60, 0xc61),
    (0xc66, 0xc6f),
    (0xc78, 0xc7e),
    (0xc80, 0xc80),
    (0xc85, 0xc8c),
    (0xc8e, 0xc90),
    (0xc92, 0xca8),
    (0xcaa, 0xcb3),
    (0xcb5, 0xcb9),
    (0xcbd, 0xcbd),
    (0xcdd, 0xcde),
    (0xce0, 0xce1),
    (0xce6, 0xcef),
    (0xcf1, 0xcf2),
    (0xd04, 0xd0c),
    (0xd0e, 0xd10),
    (0xd12, 0xd3a),
    (0xd3d, 0xd3d),
    (0xd4e, 0xd4e),
    (0xd54, 0xd56),
    (0xd58, 0xd61),
    (0xd66, 0xd78),
    (0xd7a, 0xd7f),
    (0xd85, 0xd96),
    (0xd9a, 0xdb1),
    (0xdb3, 0xdbb),
    (0xdbd, 0xdbd),
    (0xdc0, 0xdc6),
    (0xde6, 0xdef),
    (0xe01, 0xe30),
    (0xe32, 0xe33),
    (0xe40, 0xe46),
    (0xe50, 0xe59),
    (0xe81, 0xe82),
    (0xe84, 0xe84),
    (0xe86, 0xe8a),
    (0xe8c, 0xea3),
    (0xea5, 0xea5),
    (0xea7, 0xeb0),
    (0xeb2, 0xeb3),
    (0xebd, 0xebd),
    (0xec0, 0xec4),
    (0xec6, 0xec6),
    (0xed0, 0xed9),
    (0xedc, 0xedf),
    (0xf00, 0xf00),
    (0xf20, 0xf33),
    (0xf40, 0xf47),
    (0xf49, 0xf6c),
    (0xf88, 0xf8c),
    (0x1000, 0x102a),
    (0x103f, 0x1049),
    (0x1050, 0x1055),
    (0x105a, 0x105d),
    (0x1061, 0x1061),
    (0x1065, 0x1066),
    (0x106e, 0x1070),
    (0x1075, 0x1081),
    (0x108e, 0x108e),
    (0x1090, 0x1099),
    (0x10a0, 0x10c5),
    (0x10c7, 0x10c7),
    (0x10cd, 0x10cd),
    (0x10d0, 0x10fa),
    (0x10fc, 0x1248),
    (0x124a, 0x124d),
    (0x1250, 0x1256),
    (0x1258, 0x1258),
    (0x125a, 0x125d),
    (0x1260, 0x1288),
    (0x128a, 0x128d),
    (0x1290, 0x12b0),
    (0x12b2, 0x12b5),
    (0x12b8, 0x12be),
    (0x12c0, 0x12c0),
    (0x12c2, 0x12c5),
    (0x12c8, 0x12d6),
    (0x12d8, 0x1310),
    (0x1312, 0x1315),
    (0x1318, 0x135a),
    (0x1369, 0x137c),
    (0x1380, 0x138f),
    (0x13a0, 0x13f5),
    (0x13f8, 0x13fd),
    (0x1401, 0x166c),
    (0x166f, 0x167f),
    (0x1681, 0x169a),
    (0x16a0, 0x16ea),
    (0x16ee, 0x16f8),
    (0x1700, 0x1711),
    (0x171f, 0x1731),
    (0x1740, 0x1751),
    (0x1760, 0x176c),
    (0x176e, 0x1770),
    (0x1780, 0x17b3),
    (0x17d7, 0x17d7),
    (0x17dc, 0x17dc),
    (0x17e0, 0x17e9),
    (0x17f0, 0x17f9),
    (0x1810, 0x1819),
    (0x1820, 0x1878),
    (0x1880, 0x1884),
    (0x1887, 0x18a8),
    (0x18aa, 0x18aa),
    (0x18b0, 0x18f5),
    (0x1900, 0x191e),
    (0x1946, 0x196d),
    (0x1970, 0x1974),
    (0x1980, 0x19ab),
    (0x19b0, 0x19c9),
    (0x19d0, 0x19da),
    (0x1a00, 0x1a16),
    (0x1a20, 0x1a54),
    (0x1a80, 0x1a89),
    (0x1a90, 0x1a99),
    (0x1aa7, 0x1aa7),
    (0x1b05, 0x1b33),
    (0x1b45, 0x1b4c),
    (0x1b50, 0x1b59),
    (0x1b83, 0x1ba0),
    (0x1bae, 0x1be5),
    (0x1c00, 0x1c23),
    (0x1c40, 0x1c49),
    (0x1c4d, 0x1c7d),
    (0x1c80, 0x1c88),
    (0x1c90, 0x1cba),
    (0x1cbd, 0x1cbf),
    (0x1ce9, 0x1cec),
    (0x1cee, 0x1cf3),
    (0x1cf5, 0x1cf6),
    (0x1cfa, 0x1cfa),
    (0x1d00, 0x1dbf),
    (0x1e00, 0x1f15),
    (0x1f18, 0x1f1d),
    (0x1f20, 0x1f45),
    (0x1f48, 0x1f4d),
    (0x1f50, 0x1f57),
    (0x1f59, 0x1f59),
    (0x1f5b, 0x1f5b),
    (0x1f5d, 0x1f5d),
    (0x1f5f, 0x1f7d),
    (0x1f80, 0x1fb4),
    (0x1fb6, 0x1fbc),
    (0x1fbe, 0x1fbe),
    (0x1fc2, 0x1fc4),
    (0x1fc6, 0x1fcc),
    (0x1fd0, 0x1fd3),
    (0x1fd6, 0x1fdb),
    (0x1fe0, 0x1fec),
    (0x1ff2, 0x1ff4),
    (0x1ff6, 0x1ffc),
    (0x2070, 0x2071),
    (0x2074, 0x2079),
    (0x207f, 0x2089),
    (0x2090, 0x209c),
    (0x2102, 0x2102),
    (0x2107, 0x2107),
    (0x210a, 0x2113),
    (0x2115, 0x2115),
    (0x2119, 0x211d),
    (0x2124, 0x2124),
    (0x2126, 0x2126),
    (0x2128, 0x2128),
    (0x212a, 0x212d),
    (0x212f, 0x2139),
    (0x213c, 0x213f),
    (0x2145, 0x2149),
    (0x214e, 0x214e),
    (0x2150, 0x2189),
    (0x2460, 0x249b),
    (0x24ea, 0x24ff),
    (0x2776, 0x2793),
    (0x2c00, 0x2ce4),
    (0x2ceb, 0x2cee),
    (0x2cf2, 0x2cf3),
    (0x2cfd, 0x2cfd),
    (0x2d00, 0x2d25),
    (0x2d27, 0x2d27),
    (0x2d2d, 0x2d2d),
    (0x2d30, 0x2d67),
    (0x2d6f, 0x2d6f),
    (0x2d80, 0x2d96),
    (0x2da0, 0x2da6),
    (0x2da8, 0x2dae),
    (0x2db0, 0x2db6),
    (0x2db8, 0x2dbe),
    (0x2dc0, 0x2dc6),
    (0x2dc8, 0x2dce),
    (0x2dd0, 0x2dd6),
    (0x2dd8, 0x2dde),
    (0x2e2f, 0x2e2f),
    (0x3005, 0x3007),
    (0x3021, 0x3029),
    (0x3031, 0x3035),
    (0x3038, 0x303c),
    (0x3041, 0x3096),
    (0x309d, 0x309f),
    (0x30a1, 0x30fa),
    (0x30fc, 0x30ff),
    (0x3105, 0x312f),
    (0x3131, 0x318e),
    (0x3192, 0x3195),
    (0x31a0, 0x31bf),
    (0x31f0, 0x31ff),
    (0x3220, 0x3229),
    (0x3248, 0x324f),
    (0x3251, 0x325f),
    (0x3280, 0x3289),
    (0x32b1, 0x32bf),
    (0x3400, 0x4dbf),
    (0x4e00, 0xa48c),
    (0xa4d0, 0xa4fd),
    (0xa500, 0xa60c),
    (0xa610, 0xa62b),
    (0xa640, 0xa66e),
    (0xa67f, 0xa69d),
    (0xa6a0, 0xa6ef),
    (0xa717, 0xa71f),
    (0xa722, 0xa788),
    (0xa78b, 0xa7ca),
    (0xa7d0, 0xa7d1),
    (0xa7d3, 0xa7d3),
    (0xa7d5, 0xa7d9),
    (0xa7f2, 0xa801),
    (0xa803, 0xa805),
    (0xa807, 0xa80a),
    (0xa80c, 0xa822),
    (0xa830, 0xa835),
    (0xa840, 0xa873),
    (0xa882, 0xa8b3),
    (0xa8d0, 0xa8d9),
    (0xa8f2, 0xa8f7),
    (0xa8fb, 0xa8fb),
    (0xa8fd, 0xa8fe),
    (0xa900, 0xa925),
    (0xa930, 0xa946),
    (0xa960, 0xa97c),
    (0xa984, 0xa9b2),
    (0xa9cf, 0xa9d9),
    (0xa9e0, 0xa9e4),
    (0xa9e6, 0xa9fe),
    (0xaa00, 0xaa28),
    (0xaa40, 0xaa42),
    (0xaa44, 0xaa4b),
    (0xaa50, 0xaa59),
    (0xaa60, 0xaa76),
    (0xaa7a, 0xaa7a),
    (0xaa7e, 0xaaaf),
    (0xaab1, 0xaab1),
    (0xaab5, 0xaab6),
    (0xaab9, 0xaabd),
    (0xaac0, 0xaac0),
    (0xaac2, 0xaac2),
    (0xaadb, 0xaadd),
    (0xaae0, 0xaaea),
    (0xaaf2, 0xaaf4),
    (0xab01, 0xab06),
    (0xab09, 0xab0e),
    (0xab11, 0xab16),
    (0xab20, 0xab26),
    (0xab28, 0xab2e),
    (0xab30, 0xab5a),
    (0xab5c, 0xab69),
    (0xab70, 0xabe2),
    (0xabf0, 0xabf9),
    (0xac00, 0xd7a3),
    (0xd7b0, 0xd7c6),
    (0xd7cb, 0xd7fb),
    (0xf900, 0xfa6d),
    (0xfa70, 0xfad9),
    (0xfb00, 0xfb06),
    (0xfb13, 0xfb17),
    (0xfb1d, 0xfb1d),
    (0xfb1f, 0xfb28),
    (0xfb2a, 0xfb36),
    (0xfb38, 0xfb3c),
    (0xfb3e, 0xfb3e),
    (0xfb40, 0xfb41),
    (0xfb43, 0xfb44),
    (0xfb46, 0xfbb1),
    (0xfbd3, 0xfd3d),
    (0xfd50, 0xfd8f),
    (0xfd92, 0xfdc7),
    (0xfdf0, 0xfdfb),
    (0xfe70, 0xfe74),
    (0xfe76, 0xfefc),
    (0xff10, 0xff19),
    (0xff21, 0xff3a),
    (0xff41, 0xff5a),
    (0xff66, 0xffbe),
    (0xffc2, 0xffc7),
    (0xffca, 0xffcf),
    (0xffd2, 0xffd7),
    (0xffda, 0xffdc),
    (0x10000, 0x1000b),
    (0x1000d, 0x10026),
    (0x10028, 0x1003a),
    (0x1003c, 0x1003d),
    (0x1003f, 0x1004d),
    (0x10050, 0x1005d),
    (0x10080, 0x100fa),
    (0x10107, 0x10133),
    (0x10140, 0x10178),
    (0x1018a, 0x1018b),
    (0x10280, 0x1029c),
    (0x102a0, 0x102d0),
    (0x102e1, 0x102fb),
    (0x10300, 0x10323),
    (0x1032d, 0x1034a),
    (0x10350, 0x10375),
    (0x10380, 0x1039d),
    (0x103a0, 0x103c3),
    (0x103c8, 0x103cf),
    (0x103d1, 0x103d5),
    (0x10400, 0x1049d),
    (0x104a0, 0x104a9),
    (0x104b0, 0x104d3),
    (0x104d8, 0x104fb),
    (0x10500, 0x10527),
    (0x10530, 0x10563),
    (0x10570, 0x1057a),
    (0x1057c, 0x1058a),
    (0x1058c, 0x10592),
    (0x10594, 0x10595),
    (0x10597, 0x105a1),
    (0x105a3, 0x105b1),
    (0x105b3, 0x105b9),
    (0x105bb, 0x105bc),
    (0x10600, 0x10736),
    (0x10740, 0x10755),
    (0x10760, 0x10767),
    (0x10780, 0x10785),
    (0x10787, 0x107b0),
    (0x107b2, 0x107ba),
    (0x10800, 0x10805),
    (0x10808, 0x10808),
    (0x1080a, 0x10835),
    (0x10837, 0x10838),
    (0x1083c, 0x1083c),
    (0x1083f, 0x10855),
    (0x10858, 0x10876),
    (0x10879, 0x1089e),
    (0x108a7, 0x108af),
    (0x108e0, 0x108f2),
    (0x108f4, 0x108f5),
    (0x108fb, 0x1091b),
    (0x10920, 0x10939),
    (0x10980, 0x109b7),
    (0x109bc, 0x109cf),
    (0x109d2, 0x10a00),
    (0x10a10, 0x10a13),
    (0x10a15, 0x10a17),
    (0x10a19, 0x10a35),
    (0x10a40, 0x10a48),
    (0x10a60, 0x10a7e),
    (0x10a80, 0x10a9f),
    (0x10ac0, 0x10ac7),
    (0x10ac9, 0x10ae4),
    (0x10aeb, 0x10aef),
    (0x10b00, 0x10b35),
    (0x10b40, 0x10b55),
    (0x10b58, 0x10b72),
    (0x10b78, 0x10b91),
    (0x10ba9, 0x10baf),
    (0x10c00, 0x10c48),
    (0x10c80, 0x10cb2),
    (0x10cc0, 0x10cf2),
    (0x10cfa, 0x10d23),
    (0x10d30, 0x10d39),
    (0x10e60, 0x10e7e),
    (0x10e80, 0x10ea9),
    (0x10eb0, 0x10eb1),
    (0x10f00, 0x10f27),
    (0x10f30, 0x10f45),
    (0x10f51, 0x10f54),
    (0x10f70, 0x10f81),
    (0x10fb0, 0x10fcb),
    (0x10fe0, 0x10ff6),
    (0x11003, 0x11037),
    (0x11052, 0x1106f),
    (0x11071, 0x11072),
    (0x11075, 0x11075),
    (0x11083, 0x110af),
    (0x110d0, 0x110e8),
    (0x110f0, 0x110f9),
    (0x11103, 0x11126),
    (0x11136, 0x1113f),
    (0x11144, 0x11144),
    (0x11147, 0x11147),
    (0x11150, 0x11172),
    (0x11176, 0x11176),
    (0x11183, 0x111b2),
    (0x111c1, 0x111c4),
    (0x111d0, 0x111da),
    (0x111dc, 0x111dc),
    (0x111e1, 0x111f4),
    (0x11200, 0x11211),
    (0x11213, 0x1122b),
    (0x1123f, 0x11240),
    (0x11280, 0x11286),
    (0x11288, 0x11288),
    (0x1128a, 0x1128d),
    (0x1128f, 0x1129d),
    (0x1129f, 0x112a8),
    (0x112b0, 0x112de),
    (0x112f0, 0x112f9),
    (0x11305, 0x1130c),
    (0x1130f, 0x11310),
    (0x11313, 0x11328),
    (0x1132a, 0x11330),
    (0x11332, 0x11333),
    (0x11335, 0x11339),
    (0x1133d, 0x1133d),
    (0x11350, 0x11350),
    (0x1135d, 0x11361),
    (0x11400, 0x11434),
    (0x11447, 0x1144a),
    (0x11450, 0x11459),
    (0x1145f, 0x11461),
    (0x11480, 0x114af),
    (0x114c4, 0x114c5),
    (0x114c7, 0x114c7),
    (0x114d0, 0x114d9),
    (0x11580, 0x115ae),
    (0x115d8, 0x115db),
    (0x11600, 0x1162f),
    (0x11644, 0x11644),
    (0x11650, 0x11659),
    (0x11680, 0x116aa),
    (0x116b8, 0x116b8),
    (0x116c0, 0x116c9),
    (0x11700, 0x1171a),
    (0x11730, 0x1173b),
    (0x11740, 0x11746),
    (0x11800, 0x1182b),
    (0x118a0, 0x118f2),
    (0x118ff, 0x11906),
    (0x11909, 0x11909),
    (0x1190c, 0x11913),
    (0x11915, 0x11916),
    (0x11918, 0x1192f),
    (0x1193f, 0x1193f),
    (0x11941, 0x11941),
    (0x11950, 0x11959),
    (0x119a0, 0x119a7),
    (0x119aa, 0x119d0),
    (0x119e1, 0x119e1),
    (0x119e3, 0x119e3),
    (0x11a00, 0x11a00),
    (0x11a0b, 0x11a32),
    (0x11a3a, 0x11a3a),
    (0x11a50, 0x11a50),
    (0x11a5c, 0x11a89),
    (0x11a9d, 0x11a9d),
    (0x11ab0, 0x11af8),
    (0x11c00, 0x11c08),
    (0x11c0a, 0x11c2e),
    (0x11c40, 0x11c40),
    (0x11c50, 0x11c6c),
    (0x11c72, 0x11c8f),
    (0x11d00, 0x11d06),
    (0x11d08, 0x11d09),
    (0x11d0b, 0x11d30),
    (0x11d46, 0x11d46),
    (0x11d50, 0x11d59),
    (0x11d60, 0x11d65),
    (0x11d67, 0x11d68),
    (0x11d6a, 0x11d89),
    (0x11d98, 0x11d98),
    (0x11da0, 0x11da9),
    (0x11ee0, 0x11ef2),
    (0x11f02, 0x11f02),
    (0x11f04, 0x11f10),
    (0x11f12, 0x11f33),
    (0x11f50, 0x11f59),
    (0x11fb0, 0x11fb0),
    (0x11fc0, 0x11fd4),
    (0x12000, 0x12399),
    (0x12400, 0x1246e),
    (0x12480, 0x12543),
    (0x12f90, 0x12ff0),
    (0x13000, 0x1342f),
    (0x13441, 0x13446),
    (0x14400, 0x14646),
    (0x16800, 0x16a38),
    (0x16a40, 0x16a5e),
    (0x16a60, 0x16a69),
    (0x16a70, 0x16abe),
    (0x16ac0, 0x16ac9),
    (0x16ad0, 0x16aed),
    (0x16b00, 0x16b2f),
    (0x16b40, 0x16b43),
    (0x16b50, 0x16b59),
    (0x16b5b, 0x16b61),
    (0x16b63, 0x16b77),
    (0x16b7d, 0x16b8f),
    (0x16e40, 0x16e96),
    (0x16f00, 0x16f4a),
    (0x16f50, 0x16f50),
    (0x16f93, 0x16f9f),
    (0x16fe0, 0x16fe1),
    (0x16fe3, 0x16fe3),
    (0x17000, 0x187f7),
    (0x18800, 0x18cd5),
    (0x18d00, 0x18d08),
    (0x1aff0, 0x1aff3),
    (0x1aff5, 0x1affb),
    (0x1affd, 0x1affe),
    (0x1b000, 0x1b122),
    (0x1b132, 0x1b132),
    (0x1b150, 0x1b152),
    (0x1b155, 0x1b155),
    (0x1b164, 0x1b167),
    (0x1b170, 0x1b2fb),
    (0x1bc00, 0x1bc6a),
    (0x1bc70, 0x1bc7c),
    (0x1bc80, 0x1bc88),
    (0x1bc90, 0x1bc99),
    (0x1d2c0, 0x1d2d3),
    (0x1d2e0, 0x1d2f3),
    (0x1d360, 0x1d378),
    (0x1d400, 0x1d454),
    (0x1d456, 0x1d49c),
    (0x1d49e, 0x1d49f),
    (0x1d4a2, 0x1d4a2),
    (0x1d4a5, 0x1d4a6),
    (0x1d4a9, 0x1d4ac),
    (0x1d4ae, 0x1d4b9),
    (0x1d4bb, 0x1d4bb),
    (0x1d4bd, 0x1d4c3),
    (0x1d4c5, 0x1d505),
    (0x1d507, 0x1d50a),
    (0x1d50d, 0x1d514),
    (0x1d516, 0x1d51c),
    (0x1d51e, 0x1d539),
    (0x1d53b, 0x1d53e),
    (0x1d540, 0x1d544),
    (0x1d546, 0x1d546),
    (0x1d54a, 0x1d550),
    (0x1d552, 0x1d6a5),
    (0x1d6a8, 0x1d6c0),
    (0x1d6c2, 0x1d6da),
    (0x1d6dc, 0x1d6fa),
    (0x1d6fc, 0x1d714),
    (0x1d716, 0x1d734),
    (0x1d736, 0x1d74e),
    (0x1d750, 0x1d76e),
    (0x1d770, 0x1d788),
    (0x1d78a, 0x1d7a8),
    (0x1d7aa, 0x1d7c2),
    (0x1d7c4, 0x1d7cb),
    (0x1d7ce, 0x1d7ff),
    (0x1df00, 0x1df1e),
    (0x1df25, 0x1df2a),
    (0x1e030, 0x1e06d),
    (0x1e100, 0x1e12c),
    (0x1e137, 0x1e13d),
    (0x1e140, 0x1e149),
    (0x1e14e, 0x1e14e),
    (0x1e290, 0x1e2ad),
    (0x1e2c0, 0x1e2eb),
    (0x1e2f0, 0x1e2f9),
    (0x1e4d0, 0x1e4eb),
    (0x1e4f0, 0x1e4f9),
    (0x1e7e0, 0x1e7e6),
    (0x1e7e8, 0x1e7eb),
    (0x1e7ed, 0x1e7ee),
    (0x1e7f0, 0x1e7fe),
    (0x1e800, 0x1e8c4),
    (0x1e8c7, 0x1e8cf),
    (0x1e900, 0x1e943),
    (0x1e94b, 0x1e94b),
    (0x1e950, 0x1e959),
    (0x1ec71, 0x1ecab),
    (0x1ecad, 0x1ecaf),
    (0x1ecb1, 0x1ecb4),
    (0x1ed01, 0x1ed2d),
    (0x1ed2f, 0x1ed3d),
    (0x1ee00, 0x1ee03),
    (0x1ee05, 0x1ee1f),
    (0x1ee21, 0x1ee22),
    (0x1ee24, 0x1ee24),
    (0x1ee27, 0x1ee27),
    (0x1ee29, 0x1ee32),
    (0x1ee34, 0x1ee37),
    (0x1ee39, 0x1ee39),
    (0x1ee3b, 0x1ee3b),
    (0x1ee42, 0x1ee42),
    (0x1ee47, 0x1ee47),
    (0x1ee49, 0x1ee49),
    (0x1ee4b, 0x1ee4b),
    (0x1ee4d, 0x1ee4f),
    (0x1ee51, 0x1ee52),
    (0x1ee54, 0x1ee54),
    (0x1ee57, 0x1ee57),
    (0x1ee59, 0x1ee59),
    (0x1ee5b, 0x1ee5b),
    (0x1ee5d, 0x1ee5d),
    (0x1ee5f, 0x1ee5f),
    (0x1ee61, 0x1ee62),
    (0x1ee64, 0x1ee64),
    (0x1ee67, 0x1ee6a),
    (0x1ee6c, 0x1ee72),
    (0x1ee74, 0x1ee77),
    (0x1ee79, 0x1ee7c),
    (0x1ee7e, 0x1ee7e),
    (0x1ee80, 0x1ee89),
    (0x1ee8b, 0x1ee9b),
    (0x1eea1, 0x1eea3),
    (0x1eea5, 0x1eea9),
    (0x1eeab, 0x1eebb),
    (0x1f100, 0x1f10c),
    (0x1fbf0, 0x1fbf9),
    (0x20000, 0x2a6df),
    (0x2a700, 0x2b739),
    (0x2b740, 0x2b81d),
    (0x2b820, 0x2cea1),
    (0x2ceb0, 0x2ebe0),
    (0x2f800, 0x2fa1d),
    (0x30000, 0x3134a),
    (0x31350, 0x323af),
];

#[cfg(test)]
mod manual_tenant_scope_regressions {
    use super::*;
    use std::sync::Mutex;

    struct UnusedIdentity;
    impl TenantPoliciesIdentityResolver for UnusedIdentity {
        fn resolve<'a>(&'a self, _: &'a HeaderMap, _: &'a str) -> ServiceConfigFuture<'a, TenantPoliciesIdentity> {
            Box::pin(async { panic!("execute-level regression must not resolve identity") })
        }
    }
    #[derive(Default)]
    struct CaptureDatabase(Mutex<Vec<TenantPoliciesStatement>>);
    impl TenantPoliciesDatabase for CaptureDatabase {
        fn config_audit_available(&self) -> bool { true }
        fn run(&self, statement: TenantPoliciesStatement) -> TenantPoliciesStoreFuture<'_, Vec<Value>> {
            Box::pin(async move {
                self.0.lock().unwrap().push(statement);
                Ok(vec![])
            })
        }
    }
    fn setup() -> (TenantPoliciesApi, Arc<CaptureDatabase>) {
        let database = Arc::new(CaptureDatabase::default());
        let api = TenantPoliciesApi::new(TenantPoliciesDependencies {
            identity: Arc::new(UnusedIdentity), database: Some(database.clone()),
            tool_invalidation: None, mcp_invalidation: None,
        });
        (api, database)
    }
    fn actor(role: &str, tenant: &str) -> TenantPoliciesIdentity {
        TenantPoliciesIdentity { user_id: "actor".into(), tenant_id: tenant.into(), roles: vec![role.into()], tier: "normal".into() }
    }
    #[tokio::test]
    async fn foreign_policy_denied_before_storage_own_and_platform_allowed() {
        for (role, tenant, target, allowed) in [
            ("admin", "a", "b", false), ("admin", "", "b", false),
            ("user", "a", "a", false), ("admin", "a", "a", true),
            ("platform_admin", "a", "b", true),
        ] {
            let (api, database) = setup();
            let result = api.execute(TenantPoliciesOperation::ToolDelete { tenant_id: target.into() }, &actor(role, tenant), &json!({})).await;
            if allowed {
                assert!(result.is_ok());
                assert_eq!(database.0.lock().unwrap()[0].arguments, vec![json!(target)]);
            } else {
                assert!(matches!(result, Err(ServiceConfigError::Http { status: 403, .. })));
                assert!(database.0.lock().unwrap().is_empty());
            }
        }
    }
    #[tokio::test]
    async fn ordinary_admin_lists_and_audits_always_carry_actor_tenant() {
        for operation in [
            TenantPoliciesOperation::ToolList,
            TenantPoliciesOperation::McpList,
            TenantPoliciesOperation::Audit(TenantAuditQuery::default()),
            TenantPoliciesOperation::ConfigAudit(ConfigAuditQuery {
                tenant_id: None, user_id: None, resource_type: None,
                resource_id: None, action: None, limit: json!(50), offset: json!(0),
            }),
        ] {
            let (api, database) = setup();
            api.execute(operation, &actor("admin", "a"), &json!({})).await.unwrap();
            let calls = database.0.lock().unwrap();
            assert!(!calls.is_empty());
            for call in calls.iter() {
                assert!(call.sql.contains("tenant_id = $1"));
                assert_eq!(call.arguments.first(), Some(&json!("a")));
            }
        }
    }
    #[tokio::test]
    async fn missing_or_foreign_audit_tenant_denied_before_storage() {
        for (tenant, requested) in [("", None), ("a", Some("b"))] {
            let (api, database) = setup();
            let query = TenantAuditQuery { tenant_id: requested.map(str::to_owned), ..Default::default() };
            let result = api.execute(TenantPoliciesOperation::Audit(query), &actor("admin", tenant), &json!({})).await;
            assert!(matches!(result, Err(ServiceConfigError::Http { status: 403, .. })));
            assert!(database.0.lock().unwrap().is_empty());
        }
    }
}
