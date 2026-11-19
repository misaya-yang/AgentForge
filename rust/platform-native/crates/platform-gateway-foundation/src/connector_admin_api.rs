//! Five opt-in connector catalog administration routes; inert and unmounted.
//! AuthContext and UserContext tenant agreement is checked before capabilities;
//! writes additionally require a lowercased (not trimmed) tenant-admin role.
//! Global empty-tenant templates are visible, never mutable. OAuth, MCP runtime,
//! token management, host middleware and production mounting are outside this API.
//! Injected destination validation MUST perform canonical getaddrinfo-equivalent
//! resolution and reject any disallowed address across ALL records. URL grammar
//! validation is not a substitute. No permissive validator/default is provided.
//! Encryption keys are supplied privately by the host, never read from env here.
//! NativeProviderCipher preserves the canonical SHA256/Fernet envelope; a missing
//! key or plaintext cipher fallback fails closed before any secret persistence.
//! JSON validation errors carry locations/types only, never input or context.
//! Columns, tools and extra metadata are separate statements (partial effects).
//! Hosts own authenticated identity resolution, request IDs, denial-event lifetime,
//! cancellation transport, body limits and trailing-slash/decoded-path semantics.
//! Malformed JSON diagnostic positions, nonfinite/unbounded Python numeric JSON,
//! Python custom objects remain transport boundaries, not claimed parity. No network request is performed by this module.
use crate::{
    config_audit_writer::ConfigAuditRequestContext,
    connector_admin_store::*,
    provider_cipher::is_encrypted,
    provider_service::ProviderCipher,
    rate_config_api::boolean,
    service_config_api::{ServiceConfigCancellation, ServiceConfigError, ServiceConfigFuture},
    service_registry::python_truthy,
    usage_parser::python_value_to_string,
};
use ai_platform_auth::{
    admission::AdmissionService,
    rbac::{Capability, Rbac, check_capability, permission_denied_detail},
};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{FromRequest, MatchedPath, Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
};

pub const CONNECTOR_ADMIN_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/connectors/admin/configs", "list_connector_configs"),
    (
        "POST",
        "/connectors/admin/configs",
        "create_connector_config",
    ),
    (
        "PUT",
        "/connectors/admin/configs/{provider}",
        "update_connector_config",
    ),
    (
        "PATCH",
        "/connectors/admin/configs/{provider}/enabled",
        "toggle_connector_config",
    ),
    (
        "DELETE",
        "/connectors/admin/configs/{provider}",
        "delete_connector_config",
    ),
];
#[derive(Clone, Default)]
pub struct ConnectorAdminIdentity {
    pub auth_authenticated: bool,
    pub user_authenticated: bool,
    pub auth_tenant_id: String,
    pub user_tenant_id: String,
    pub user_id: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
}
/// Resolve both canonical contexts, including disagreement, rather than trusting
/// request tenant headers. AdmissionService supplies one already-verified identity.
pub trait ConnectorAdminIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, ConnectorAdminIdentity>;
    fn context(&self, headers: &HeaderMap) -> ConfigAuditRequestContext {
        ConfigAuditRequestContext {
            headers: headers.clone(),
            ..Default::default()
        }
    }
    /// Optional observability hook for source-parity tracing. It cannot grant
    /// access; the canonical RBAC decision below always runs independently.
    fn capability_check_started(&self, _capability: Capability) {}
    fn permission_denied(
        &self,
        _identity: &ConnectorAdminIdentity,
        _capability: Capability,
        _context: &ConfigAuditRequestContext,
    ) {
    }
}
impl ConnectorAdminIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, ConnectorAdminIdentity> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let user = request
                .resolve()
                .await
                .map_err(|e| ServiceConfigError::detail(e.status(), e.to_string()))?;
            Ok(ConnectorAdminIdentity {
                auth_authenticated: true,
                user_authenticated: true,
                auth_tenant_id: user.tenant_id().into(),
                user_tenant_id: user.tenant_id().into(),
                user_id: user.user_id().into(),
                roles: user.roles().to_vec(),
                permissions: user.permissions().to_vec(),
            })
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorDestinationError {
    Unresolved,
    Disallowed,
    Cancelled,
}
/// Required trusted DNS boundary. Success means every resolved address was
/// checked using safe_fetch.is_safe_destination semantics. Runtime must resolve
/// and pin again when connecting. This interface does not authorize a bypass.
pub trait ConnectorDestinationValidator: Send + Sync {
    fn validate<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), ConnectorDestinationError>> + Send + 'a>,
    >;
}
pub struct ConnectorAdminDependencies {
    pub identity: Arc<dyn ConnectorAdminIdentityResolver>,
    pub rbac: Rbac,
    pub database: Option<Arc<dyn ConnectorAdminDatabase>>,
    pub destination: Arc<dyn ConnectorDestinationValidator>,
    pub cipher: Arc<dyn ProviderCipher>,
    pub encryption_key: String,
}
pub struct ConnectorAdminApi {
    dependencies: ConnectorAdminDependencies,
}
#[derive(Clone, Debug)]
pub enum ConnectorAdminOperation {
    List,
    Create,
    Update { provider: String },
    Toggle { provider: String },
    Delete { provider: String },
}
impl ConnectorAdminOperation {
    fn path(&self) -> String {
        match self {
            Self::List | Self::Create => "/api/v1/connectors/admin/configs".into(),
            Self::Update { provider } | Self::Delete { provider } => {
                format!("/api/v1/connectors/admin/configs/{provider}")
            }
            Self::Toggle { provider } => {
                format!("/api/v1/connectors/admin/configs/{provider}/enabled")
            }
        }
    }
    fn body(&self) -> bool {
        matches!(
            self,
            Self::Create | Self::Update { .. } | Self::Toggle { .. }
        )
    }
}
fn internal_store(e: ConnectorStoreError) -> ServiceConfigError {
    if e == ConnectorStoreError::Cancelled {
        ServiceConfigError::Cancelled
    } else {
        ServiceConfigError::Internal
    }
}
fn stmt(
    kind: ConnectorStatementKind,
    sql: impl Into<String>,
    arguments: Vec<ConnectorArgument>,
) -> ConnectorStatement {
    ConnectorStatement {
        kind,
        sql: sql.into(),
        arguments,
    }
}
fn key_args(tenant: &str, provider: &str) -> Vec<ConnectorArgument> {
    vec![
        ConnectorArgument::text(tenant),
        ConnectorArgument::text(provider),
    ]
}
impl ConnectorAdminApi {
    pub fn new(dependencies: ConnectorAdminDependencies) -> Self {
        Self { dependencies }
    }
    pub async fn handle(
        &self,
        op: ConnectorAdminOperation,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<Value, ServiceConfigError> {
        let context = self.dependencies.identity.context(headers);
        let identity = self
            .dependencies
            .identity
            .resolve(headers, &op.path())
            .await?;
        self.execute(op, &identity, body, &context).await
    }
    fn db(&self) -> Result<&dyn ConnectorAdminDatabase, ServiceConfigError> {
        self.dependencies
            .database
            .as_deref()
            .ok_or_else(|| ServiceConfigError::detail(500, "Database not available"))
    }
    fn authorize(
        &self,
        id: &ConnectorAdminIdentity,
        write: bool,
        context: &ConfigAuditRequestContext,
    ) -> Result<(), ServiceConfigError> {
        if !id.auth_authenticated
            || !id.user_authenticated
            || id.user_tenant_id.is_empty()
            || id.user_tenant_id == "public"
            || id.auth_tenant_id != id.user_tenant_id
        {
            return Err(ServiceConfigError::detail(
                403,
                "Authenticated tenant context is required",
            ));
        }
        let capability = if write {
            Capability::GatewayConnectorConfigWrite
        } else {
            Capability::GatewayConnectorConfigRead
        };
        self.dependencies
            .identity
            .capability_check_started(capability);
        if !check_capability(
            &self.dependencies.rbac,
            &id.roles,
            &id.permissions,
            capability,
        )
        .allowed()
        {
            self.dependencies
                .identity
                .permission_denied(id, capability, context);
            let trace = context
                .request_id
                .as_ref()
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| context.trace_id.as_ref().and_then(Value::as_str))
                .unwrap_or("");
            return Err(ServiceConfigError::detail(
                403,
                permission_denied_detail(capability, trace, None),
            ));
        }
        if write
            && !id.roles.iter().any(|r| {
                matches!(
                    r.to_lowercase().as_str(),
                    "admin" | "tenant_admin" | "superadmin" | "super_admin"
                )
            })
        {
            return Err(ServiceConfigError::detail(
                403,
                "Tenant administrator access is required",
            ));
        }
        Ok(())
    }
    async fn run(&self, s: ConnectorStatement) -> Result<Vec<Value>, ServiceConfigError> {
        self.db()?.run(s).await.map_err(internal_store)
    }
    async fn tenant_row(
        &self,
        tenant: &str,
        provider: &str,
    ) -> Result<Option<Value>, ServiceConfigError> {
        Ok(self
            .run(stmt(
                ConnectorStatementKind::TenantRow,
                "SELECT * FROM connector_configs WHERE tenant_id = $1 AND provider = $2",
                key_args(tenant, provider),
            ))
            .await?
            .into_iter()
            .next())
    }
    async fn mutable(&self, tenant: &str, provider: &str) -> Result<(), ServiceConfigError> {
        if self.tenant_row(tenant, provider).await?.is_some() {
            return Ok(());
        }
        if !self
            .run(stmt(
                ConnectorStatementKind::GlobalExists,
                "SELECT 1 FROM connector_configs WHERE tenant_id = '' AND provider = $1",
                vec![ConnectorArgument::text(provider)],
            ))
            .await?
            .is_empty()
        {
            return Err(ServiceConfigError::detail(
                403,
                "Global connector templates are read-only; create a tenant override",
            ));
        }
        Err(ServiceConfigError::detail(
            404,
            format!("Connector not configured: {provider}"),
        ))
    }
    async fn destinations(&self, body: &Value) -> Result<(), ServiceConfigError> {
        for field in ["auth_url", "token_url"] {
            if let Some(url) = body
                .get("auth")
                .and_then(|v| v.get(field))
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
            {
                let (host, port) =
                    oauth_destination(url).map_err(|_| ServiceConfigError::Internal)?;
                self.dependencies
                    .destination
                    .validate(&host, port)
                    .await
                    .map_err(|e| match e {
                        ConnectorDestinationError::Unresolved => {
                            ServiceConfigError::detail(422, "OAuth destination cannot be resolved")
                        }
                        ConnectorDestinationError::Disallowed => {
                            ServiceConfigError::detail(422, "OAuth destination is not permitted")
                        }
                        ConnectorDestinationError::Cancelled => ServiceConfigError::Cancelled,
                    })?;
            }
        }
        Ok(())
    }
    fn encrypt(&self, secret: &str) -> Result<String, ServiceConfigError> {
        if self.dependencies.encryption_key.is_empty() {
            return Err(ServiceConfigError::detail(
                503,
                "Connector secret encryption is not configured",
            ));
        }
        let encrypted = self
            .dependencies
            .cipher
            .encrypt(secret, &self.dependencies.encryption_key)
            .map_err(|_| ServiceConfigError::Internal)?;
        if !is_encrypted(&encrypted) {
            return Err(ServiceConfigError::detail(
                500,
                "Connector secret encryption failed",
            ));
        }
        Ok(encrypted)
    }
    async fn merge(
        &self,
        tenant: &str,
        provider: &str,
        extra: Value,
    ) -> Result<(), ServiceConfigError> {
        self.run(stmt(ConnectorStatementKind::MergeExtra,"UPDATE connector_configs SET extra_config = COALESCE(extra_config, '{}'::jsonb) || $1::jsonb, updated_at = NOW() WHERE tenant_id = $2 AND provider = $3",vec![ConnectorArgument::Json(extra),ConnectorArgument::text(tenant),ConnectorArgument::text(provider)])).await?;
        Ok(())
    }
    async fn persisted(&self, tenant: &str, provider: &str) -> Result<Value, ServiceConfigError> {
        let row = self
            .tenant_row(tenant, provider)
            .await?
            .ok_or_else(|| ServiceConfigError::detail(500, "Failed to persist connector config"))?;
        row_to_response(&row)
    }
    pub async fn execute(
        &self,
        op: ConnectorAdminOperation,
        id: &ConnectorAdminIdentity,
        body: &Value,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        let body = if op.body() {
            validate_payload(&op, body).map_err(|fields| redacted_validation(fields, context))?
        } else {
            Value::Null
        };
        self.authorize(id, !matches!(op, ConnectorAdminOperation::List), context)?;
        let tenant = &id.user_tenant_id;
        match op {
            ConnectorAdminOperation::List => {
                let rows=self.run(stmt(ConnectorStatementKind::List,"SELECT * FROM ( SELECT DISTINCT ON (provider) * FROM connector_configs WHERE tenant_id = $1 OR tenant_id = '' ORDER BY provider, (tenant_id = $1) DESC ) AS effective ORDER BY display_name, provider",vec![ConnectorArgument::text(tenant)])).await?;
                rows.iter()
                    .map(row_to_response)
                    .collect::<Result<Vec<_>, _>>()
                    .map(Value::Array)
            }
            ConnectorAdminOperation::Create => {
                self.destinations(&body).await?;
                self.db()?;
                let provider = body["provider"]
                    .as_str()
                    .ok_or(ServiceConfigError::Internal)?;
                if !self
                    .run(stmt(
                        ConnectorStatementKind::TenantExists,
                        "SELECT 1 FROM connector_configs WHERE tenant_id = $1 AND provider = $2",
                        key_args(tenant, provider),
                    ))
                    .await?
                    .is_empty()
                {
                    return Err(ServiceConfigError::detail(
                        409,
                        format!("Connector already configured: {provider}"),
                    ));
                }
                let mut columns = columns(&body, true);
                if let Some(secret) = body
                    .get("auth")
                    .and_then(|a| a.get("client_secret"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    columns.push((
                        "client_secret",
                        ConnectorArgument::text(self.encrypt(secret)?),
                    ))
                }
                columns.push(("tenant_id", ConnectorArgument::text(tenant)));
                let names = columns
                    .iter()
                    .map(|(k, _)| *k)
                    .collect::<Vec<_>>()
                    .join(", ");
                let placeholders = (1..=columns.len())
                    .map(|i| format!("${i}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.run(stmt(
                    ConnectorStatementKind::Insert,
                    format!("INSERT INTO connector_configs ({names}) VALUES ({placeholders})"),
                    columns.into_iter().map(|(_, v)| v).collect(),
                ))
                .await?;
                let mut extra = body
                    .get("extra_config")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                if let Some(tools) = body.get("mcp_tools").filter(|v| python_truthy(v)) {
                    extra.insert("mcp_tools".into(), tools.clone());
                }
                if !extra.is_empty() {
                    self.merge(tenant, provider, Value::Object(extra)).await?
                }
                self.persisted(tenant, provider).await
            }
            ConnectorAdminOperation::Update { provider } => {
                self.db()?;
                self.mutable(tenant, &provider).await?;
                self.destinations(&body).await?;
                let mut columns = columns(&body, false);
                if let Some(secret) = body
                    .get("auth")
                    .and_then(|a| a.get("client_secret"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    columns.push((
                        "client_secret",
                        ConnectorArgument::text(self.encrypt(secret)?),
                    ))
                }
                let tools = body.get("mcp_tools").filter(|v| !v.is_null());
                let extra = body.get("extra_config").filter(|v| !v.is_null());
                if columns.is_empty() && tools.is_none() && extra.is_none() {
                    return Err(ServiceConfigError::detail(
                        422,
                        "No mutable fields provided",
                    ));
                }
                if !columns.is_empty() {
                    let sets = columns
                        .iter()
                        .enumerate()
                        .map(|(i, (k, _))| format!("{k} = ${}", i + 1))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let n = columns.len();
                    let mut args = columns.into_iter().map(|(_, v)| v).collect::<Vec<_>>();
                    args.extend(key_args(tenant, &provider));
                    self.run(stmt(ConnectorStatementKind::Update,format!("UPDATE connector_configs SET {sets}, updated_at = NOW() WHERE tenant_id = ${} AND provider = ${}",n+1,n+2),args)).await?;
                }
                if let Some(tools) = tools {
                    self.merge(tenant, &provider, json!({"mcp_tools":tools}))
                        .await?
                }
                if let Some(extra) = extra {
                    let mut extra = extra.as_object().cloned().unwrap_or_default();
                    extra.shift_remove("mcp_tools");
                    if !extra.is_empty() {
                        self.merge(tenant, &provider, Value::Object(extra)).await?
                    }
                }
                self.persisted(tenant, &provider).await
            }
            ConnectorAdminOperation::Toggle { provider } => {
                self.db()?;
                self.mutable(tenant, &provider).await?;
                self.run(stmt(ConnectorStatementKind::Toggle,"UPDATE connector_configs SET enabled = $1, updated_at = NOW() WHERE tenant_id = $2 AND provider = $3",vec![ConnectorArgument::Bool(body["enabled"].as_bool()),ConnectorArgument::text(tenant),ConnectorArgument::text(&provider)])).await?;
                self.persisted(tenant, &provider).await
            }
            ConnectorAdminOperation::Delete { provider } => {
                self.db()?;
                self.mutable(tenant, &provider).await?;
                if !self.run(stmt(ConnectorStatementKind::ConnectedUsers,"SELECT 1 FROM user_connectors WHERE tenant_id = $1 AND provider = $2 LIMIT 1",key_args(tenant,&provider))).await?.is_empty(){return Err(ServiceConfigError::detail(409,format!("Connector '{provider}' has connected users; disconnect them before deleting")))}
                self.run(stmt(
                    ConnectorStatementKind::Delete,
                    "DELETE FROM connector_configs WHERE tenant_id = $1 AND provider = $2",
                    key_args(tenant, &provider),
                ))
                .await?;
                Ok(json!({"status":"deleted","provider":provider}))
            }
        }
    }
}
const BASE: &[&str] = &[
    "provider",
    "display_name",
    "description",
    "icon_url",
    "mode",
    "enabled",
    "supports_sync",
    "supports_search",
    "mcp_tools",
    "extra_config",
];
const AUTH: &[&str] = &[
    "client_id",
    "client_secret",
    "auth_url",
    "token_url",
    "scopes",
    "redirect_uri",
];
fn columns(body: &Value, create: bool) -> Vec<(&'static str, ConnectorArgument)> {
    let mut out = vec![];
    for &field in &BASE[..8] {
        if field == "provider" && !create {
            continue;
        }
        if let Some(value) = body.get(field) {
            let arg = if matches!(field, "enabled" | "supports_sync" | "supports_search") {
                ConnectorArgument::Bool(value.as_bool())
            } else {
                ConnectorArgument::Text(value.as_str().map(str::to_owned))
            };
            out.push((field, arg));
        }
    }
    for &field in AUTH {
        if field == "client_secret" {
            continue;
        }
        if let Some(value) = body.get("auth").and_then(|a| a.get(field))
            && (create || !value.is_null())
        {
            out.push((
                field,
                ConnectorArgument::Text(value.as_str().map(str::to_owned)),
            ));
        }
    }
    out
}
fn field_error(loc: Vec<String>, kind: &str) -> Value {
    json!({"loc":loc,"type":kind})
}
fn at(field: &str) -> Vec<String> {
    vec!["body".into(), field.into()]
}
fn string_value(
    value: &Value,
    loc: Vec<String>,
    nullable: bool,
    min: usize,
    max: usize,
    errors: &mut Vec<Value>,
) -> bool {
    if value.is_null() && nullable {
        return true;
    }
    let Some(s) = value.as_str() else {
        errors.push(field_error(loc, "string_type"));
        return false;
    };
    let n = s.chars().count();
    if n < min {
        errors.push(field_error(loc, "string_too_short"));
        false
    } else if n > max {
        errors.push(field_error(loc, "string_too_long"));
        false
    } else {
        true
    }
}
/// Returns only explicitly supplied fields (Pydantic exclude_unset=True).
/// Never stores or prints a submitted value in a validation diagnostic.
pub fn validate_payload(op: &ConnectorAdminOperation, body: &Value) -> Result<Value, Vec<Value>> {
    let Some(fields) = body.as_object() else {
        return Err(vec![field_error(
            vec!["body".into()],
            if body.is_null() {
                "missing"
            } else {
                "model_attributes_type"
            },
        )]);
    };
    let toggle = matches!(op, ConnectorAdminOperation::Toggle { .. });
    let create = matches!(op, ConnectorAdminOperation::Create);
    let mut out = Map::new();
    let mut errors = vec![];
    if toggle {
        match fields.get("enabled") {
            None => errors.push(field_error(at("enabled"), "missing")),
            Some(v) => match boolean(v) {
                Ok(b) => {
                    out.insert("enabled".into(), json!(b));
                }
                Err(e) => errors.push(field_error(at("enabled"), e)),
            },
        }
        return if errors.is_empty() {
            Ok(Value::Object(out))
        } else {
            Err(errors)
        };
    }
    let order: Vec<&str> = if create {
        BASE.iter()
            .copied()
            .chain(std::iter::once("auth"))
            .collect()
    } else {
        vec![
            "display_name",
            "description",
            "icon_url",
            "mode",
            "enabled",
            "supports_sync",
            "supports_search",
            "auth",
            "mcp_tools",
            "extra_config",
        ]
    };
    for field in order {
        if field == "provider" && !create {
            continue;
        }
        let Some(v) = fields.get(field) else {
            if create && matches!(field, "provider" | "display_name") {
                errors.push(field_error(at(field), "missing"));
            }
            continue;
        };
        if matches!(
            field,
            "provider" | "display_name" | "description" | "icon_url"
        ) {
            let nullable =
                (!create && field != "provider") || matches!(field, "description" | "icon_url");
            let min = usize::from(matches!(field, "provider" | "display_name"));
            let max = match field {
                "provider" => 64,
                "display_name" => 256,
                "description" => 4000,
                _ => 512,
            };
            if string_value(v, at(field), nullable, min, max, &mut errors) {
                out.insert(field.into(), v.clone());
            }
        } else if field == "mode" {
            if (!create && v.is_null()) || matches!(v.as_str(), Some("live" | "ingest" | "both")) {
                out.insert(field.into(), v.clone());
            } else {
                errors.push(field_error(at(field), "literal_error"));
            }
        } else if matches!(field, "enabled" | "supports_sync" | "supports_search") {
            if !create && v.is_null() {
                out.insert(field.into(), Value::Null);
            } else {
                match boolean(v) {
                    Ok(b) => {
                        out.insert(field.into(), json!(b));
                    }
                    Err(e) => errors.push(field_error(at(field), e)),
                }
            }
        } else if field == "extra_config" {
            if v.is_object() || (!create && v.is_null()) {
                out.insert(field.into(), v.clone());
            } else {
                errors.push(field_error(at(field), "dict_type"));
            }
        } else if field == "mcp_tools" {
            if !create && v.is_null() {
                out.insert(field.into(), Value::Null);
                continue;
            }
            let Some(tools) = v.as_array() else {
                errors.push(field_error(at(field), "list_type"));
                continue;
            };
            let before = errors.len();
            let mut normalized = vec![];
            for (i, tool) in tools.iter().enumerate() {
                let loc = vec!["body".into(), field.into(), i.to_string()];
                let Some(tool) = tool.as_object() else {
                    errors.push(field_error(loc, "model_attributes_type"));
                    continue;
                };
                let mut item = json!({"name":"","description":""});
                for key in ["name", "description"] {
                    if let Some(value) = tool.get(key) {
                        let mut loc = loc.clone();
                        loc.push(key.into());
                        if string_value(value, loc, false, 0, usize::MAX, &mut errors) {
                            item[key] = value.clone();
                        }
                    }
                }
                normalized.push(item);
            }
            if tools.len() > 64 {
                errors.truncate(before);
                errors.push(field_error(at(field), "too_long"));
            } else {
                out.insert(field.into(), Value::Array(normalized));
            }
        } else if field == "auth" {
            if v.is_null() {
                out.insert(field.into(), Value::Null);
                continue;
            }
            let Some(auth) = v.as_object() else {
                errors.push(field_error(at(field), "model_attributes_type"));
                continue;
            };
            let mut auth_out = Map::new();
            for &key in AUTH {
                if let Some(value) = auth.get(key) {
                    let loc = vec!["body".into(), "auth".into(), key.into()];
                    if string_value(
                        value,
                        loc.clone(),
                        matches!(key, "client_secret" | "redirect_uri"),
                        0,
                        usize::MAX,
                        &mut errors,
                    ) {
                        if matches!(key, "auth_url" | "token_url")
                            && value.as_str().is_some_and(|v| !v.is_empty())
                            && oauth_destination(value.as_str().unwrap()).is_err()
                        {
                            errors.push(field_error(loc, "value_error"));
                        } else {
                            auth_out.insert(key.into(), value.clone());
                        }
                    }
                }
            }
            for key in auth.keys() {
                if !AUTH.contains(&key.as_str()) {
                    errors.push(field_error(
                        vec!["body".into(), "auth".into(), key.clone()],
                        "extra_forbidden",
                    ));
                }
            }
            out.insert(field.into(), Value::Object(auth_out));
        }
    }
    for key in fields.keys() {
        if !(BASE.contains(&key.as_str()) && (*key != "provider" || create)) && key != "auth" {
            errors.push(field_error(at(key), "extra_forbidden"));
        }
    }
    if errors.is_empty() {
        Ok(Value::Object(out))
    } else {
        Err(errors)
    }
}
fn redacted_validation(
    fields: Vec<Value>,
    context: &ConfigAuditRequestContext,
) -> ServiceConfigError {
    let request_id = context
        .request_id
        .as_ref()
        .filter(|v| python_truthy(v))
        .or_else(|| context.trace_id.as_ref().filter(|v| python_truthy(v)))
        .map(python_value_to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    ServiceConfigError::detail(
        422,
        json!({"code":"REQUEST_VALIDATION_FAILED","message":"Request validation failed","request_id":request_id,"fields":fields}),
    )
}
/// Redacted schema rejection; submitted URL values never enter diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectorOAuthEndpointError;
impl std::fmt::Display for ConnectorOAuthEndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid OAuth endpoint")
    }
}
impl std::error::Error for ConnectorOAuthEndpointError {}
/// urllib-style split of the security-relevant authority, preserving spelling.
/// No hostname grammar or HTTP request is invented; DNS validation follows this.
pub fn oauth_destination(input: &str) -> Result<(String, u16), ConnectorOAuthEndpointError> {
    let cleaned = input
        .trim_start_matches(|c: char| (c as u32) <= 0x20)
        .replace(['\t', '\r', '\n'], "");
    let (scheme, rest) = cleaned
        .split_once("://")
        .ok_or(ConnectorOAuthEndpointError)?;
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(ConnectorOAuthEndpointError);
    }
    let (without_fragment, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    if !fragment.is_empty() {
        return Err(ConnectorOAuthEndpointError);
    }
    let authority = without_fragment.split(['/', '?']).next().unwrap_or("");
    if authority.is_empty()
        || authority.contains('@')
        || authority
            .chars()
            .any(|c| NFKC_AUTHORITY_DELIMITERS.contains(&(c as u32)))
    {
        return Err(ConnectorOAuthEndpointError);
    }
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or(ConnectorOAuthEndpointError)?;
        // urllib's IPvFuture grammar is lowercase v + hex version + nonempty suffix.
        let future = host
            .strip_prefix('v')
            .and_then(|s| s.split_once('.'))
            .is_some_and(|(version, address)| {
                !version.is_empty()
                    && version.bytes().all(|c| c.is_ascii_hexdigit())
                    && !address.is_empty()
            });
        let scoped = host.split_once('%');
        let valid_scope = scoped.is_none_or(|(_, scope)| !scope.is_empty() && !scope.contains('%'));
        if !future
            && (!valid_scope
                || !matches!(
                    host.split('%').next().unwrap_or("").parse::<IpAddr>(),
                    Ok(IpAddr::V6(_))
                ))
        {
            return Err(ConnectorOAuthEndpointError);
        }
        (
            host,
            if tail.is_empty() {
                ""
            } else {
                tail.strip_prefix(':').ok_or(ConnectorOAuthEndpointError)?
            },
        )
    } else {
        if authority.contains(['[', ']']) {
            return Err(ConnectorOAuthEndpointError);
        }
        match authority.split_once(':') {
            Some(v) => v,
            None => (authority, ""),
        }
    };
    if host.is_empty() {
        return Err(ConnectorOAuthEndpointError);
    }
    let port = if port.is_empty() {
        443
    } else {
        if !port.bytes().all(|c| c.is_ascii_digit()) {
            return Err(ConnectorOAuthEndpointError);
        }
        let p = port
            .parse::<u16>()
            .map_err(|_| ConnectorOAuthEndpointError)?;
        if p == 0 {
            return Err(ConnectorOAuthEndpointError);
        }
        p
    };
    let host = match host.split_once('%') {
        Some((address, scope)) => format!("{}%{scope}", address.to_lowercase()),
        None => host.to_lowercase(),
    };
    if let Ok(ip) = host.split('%').next().unwrap_or("").parse::<IpAddr>()
        && !schema_global_ip(ip)
    {
        return Err(ConnectorOAuthEndpointError);
    }
    Ok((host, port))
}
// Unicode 15.0.0 NFKC folds introducing urllib authority delimiters /?:@#.
const NFKC_AUTHORITY_DELIMITERS: &[u32] = &[
    0x2047, 0x2048, 0x2049, 0x2100, 0x2101, 0x2105, 0x2106, 0x2a74, 0xfe13, 0xfe16, 0xfe55, 0xfe56,
    0xfe5f, 0xfe6b, 0xff03, 0xff0f, 0xff1a, 0xff1f, 0xff20,
];
// Python 3.12.14 ipaddress private/global tables, including backported exceptions.
// The oracle records literal classification from the unchanged source runtime.
fn v4_private(ip: Ipv4Addr) -> bool {
    let n = u32::from(ip);
    if matches!(n, 0xc0000009 | 0xc000000a) {
        return false;
    }
    [
        (0x00000000u32, 8),
        (0x0a000000, 8),
        (0x7f000000, 8),
        (0xa9fe0000, 16),
        (0xac100000, 12),
        (0xc0000000, 24),
        (0xc00000aa, 31),
        (0xc0000200, 24),
        (0xc0a80000, 16),
        (0xc6120000, 15),
        (0xc6336400, 24),
        (0xcb007100, 24),
        (0xf0000000, 4),
    ]
    .iter()
    .any(|&(base, bits)| n >> (32 - bits) == base >> (32 - bits))
}
fn schema_global_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !v4_private(ip) && u32::from(ip) >> 22 != 0x64400000u32 >> 22,
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return schema_global_ip(IpAddr::V4(v4));
            }
            let n = u128::from(ip);
            let contains = |base: u128, bits: u32| n >> (128 - bits) == base >> (128 - bits);
            let except = contains((0x20010001u128 << 96) | 1, 128)
                || contains((0x20010001u128 << 96) | 2, 128)
                || contains(0x20010003u128 << 96, 32)
                || contains(0x200100040112u128 << 80, 48)
                || contains(0x20010020u128 << 96, 28)
                || contains(0x20010030u128 << 96, 28);
            except
                || ![
                    (0, 128),
                    (1, 128),
                    (0x0064ff9b0001u128 << 80, 48),
                    (0x0100u128 << 112, 64),
                    (0x20010000u128 << 96, 23),
                    (0x20010db8u128 << 96, 32),
                    (0x2002u128 << 112, 16),
                    (0x3fffu128 << 112, 20),
                    (0xfc00u128 << 112, 7),
                    (0xfe80u128 << 112, 10),
                ]
                .iter()
                .any(|&(base, bits)| contains(base, bits))
        }
    }
}
/// Public projection explicitly excludes client_secret (including malformed rows).
pub fn row_to_response(row: &Value) -> Result<Value, ServiceConfigError> {
    let r = row.as_object().ok_or(ServiceConfigError::Internal)?;
    let get = |key: &str| r.get(key).cloned().unwrap_or(Value::Null);
    let raw = get("extra_config");
    let raw = if let Some(s) = raw.as_str() {
        serde_json::from_str(s).unwrap_or(Value::Null)
    } else {
        raw
    };
    let mut extra = raw.as_object().cloned().unwrap_or_default();
    let tools = extra
        .shift_remove("mcp_tools")
        .filter(python_truthy)
        .unwrap_or_else(|| json!([]));
    let mut out = json!({"provider":get("provider"),"display_name":get("display_name"),"description":get("description"),"icon_url":get("icon_url"),"mode":r.get("mode").filter(|v|python_truthy(v)).cloned().unwrap_or_else(||json!("live")),"enabled":r.get("enabled").map(python_truthy).unwrap_or(true),"supports_sync":r.get("supports_sync").map(python_truthy).unwrap_or(false),"supports_search":r.get("supports_search").map(python_truthy).unwrap_or(true),"auth":null,"mcp_tools":tools,"extra_config":extra,"tenant_id":get("tenant_id").as_str().unwrap_or(""),"created_at":get("created_at"),"updated_at":get("updated_at")});
    if [
        "client_id",
        "auth_url",
        "token_url",
        "scopes",
        "redirect_uri",
    ]
    .iter()
    .any(|key| python_truthy(&get(key)))
    {
        let mut auth = json!({"redirect_uri":get("redirect_uri")});
        for key in ["client_id", "auth_url", "token_url", "scopes"] {
            auth[key] = get(key).as_str().map(|s| json!(s)).unwrap_or_else(|| {
                if python_truthy(&get(key)) {
                    get(key)
                } else {
                    json!("")
                }
            });
        }
        out["auth"] = auth;
    }
    // Response_model validation is part of HTTP parity: malformed stored metadata
    // produces a redacted server error, rather than leaking raw row/secret content.
    let mut base = out.clone();
    let map = base.as_object_mut().unwrap();
    for key in ["auth", "tenant_id", "created_at", "updated_at"] {
        map.shift_remove(key);
    }
    let normalized = validate_payload(&ConnectorAdminOperation::Create, &base)
        .map_err(|_| ServiceConfigError::Internal)?;
    out["mcp_tools"] = normalized["mcp_tools"].clone();
    if let Some(auth) = out["auth"].as_object() {
        for key in ["client_id", "auth_url", "token_url", "scopes"] {
            if !auth[key].is_string() {
                return Err(ServiceConfigError::Internal);
            }
        }
        if !auth["redirect_uri"].is_null() && !auth["redirect_uri"].is_string() {
            return Err(ServiceConfigError::Internal);
        }
    }
    if !get("tenant_id").is_null() && !get("tenant_id").is_string() {
        return Err(ServiceConfigError::Internal);
    }
    for key in ["created_at", "updated_at"] {
        if let Some(s) = out[key].as_str() {
            let time = chrono::DateTime::parse_from_rfc3339(s)
                .map_err(|_| ServiceConfigError::Internal)?;
            out[key] = json!(time.to_rfc3339_opts(
                if time.timestamp_subsec_micros() == 0 {
                    chrono::SecondsFormat::Secs
                } else {
                    chrono::SecondsFormat::Micros
                },
                true
            ));
        } else if !out[key].is_null() {
            return Err(ServiceConfigError::Internal);
        }
    }
    Ok(out)
}
#[derive(Clone)]
struct HttpState {
    api: Arc<ConnectorAdminApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
}
pub fn router(
    api: Arc<ConnectorAdminApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
) -> Router {
    Router::new()
        .route("/api/v1/connectors/admin/configs", any(dispatch))
        .route("/api/v1/connectors/admin/configs/{provider}", any(dispatch))
        .route(
            "/api/v1/connectors/admin/configs/{provider}/enabled",
            any(dispatch),
        )
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn respond(
    state: &HttpState,
    result: Result<Value, ServiceConfigError>,
    status: u16,
    context: &ConfigAuditRequestContext,
) -> Response {
    match result {
        Ok(value) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
            Json(value),
        )
            .into_response(),
        Err(ServiceConfigError::Cancelled) => state.cancellation.cancelled().await,
        Err(e) => {
            let (s, v) = e.response(context).expect("noncancelled");
            (
                StatusCode::from_u16(s).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(v),
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
        Ok(Path(p)) => p,
        Err(e) => return e.into_response(),
    };
    if params.values().any(|v| v.contains('/')) {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    }
    let provider = params.get("provider").cloned().unwrap_or_default();
    let method = request.method().as_str();
    let path = matched.as_str();
    let op = match (
        method,
        path.ends_with("/enabled"),
        params.contains_key("provider"),
    ) {
        ("GET", false, false) => ConnectorAdminOperation::List,
        ("POST", false, false) => ConnectorAdminOperation::Create,
        ("PUT", false, true) => ConnectorAdminOperation::Update { provider },
        ("DELETE", false, true) => ConnectorAdminOperation::Delete { provider },
        ("PATCH", true, true) => ConnectorAdminOperation::Toggle { provider },
        _ => {
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                [(
                    "allow",
                    if path.ends_with("/enabled") {
                        "PATCH"
                    } else if params.contains_key("provider") {
                        "PUT"
                    } else {
                        "GET"
                    },
                )],
                Json(json!({"detail":"Method Not Allowed"})),
            )
                .into_response();
        }
    };
    let status = if matches!(op, ConnectorAdminOperation::Create) {
        201
    } else {
        200
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
                .map(|Extension(c)| c.clone())
        })
        .unwrap_or_else(|| state.api.dependencies.identity.context(&headers));
    context.headers = headers.clone();
    let body = if op.body() {
        let bytes = match Bytes::from_request(request, &state).await {
            Ok(v) => v,
            Err(e) => return e.into_response(),
        };
        if bytes.is_empty() {
            Value::Null
        } else if crate::http_content_type::is_json_content_type(&headers) {
            match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(_) => {
                    return respond(
                        &state,
                        Err(redacted_validation(
                            vec![field_error(vec!["body".into(), "0".into()], "json_invalid")],
                            &context,
                        )),
                        status,
                        &context,
                    )
                    .await;
                }
            }
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
        .resolve(&headers, &op.path())
        .await
    {
        Ok(v) => v,
        Err(e) => return respond(&state, Err(e), status, &context).await,
    };
    respond(
        &state,
        state.api.execute(op, &identity, &body, &context).await,
        status,
        &context,
    )
    .await
}
