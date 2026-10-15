//! Eleven opt-in routes from `src/api/v1/connectors.py`.
//!
//! OAuth callback is state-authenticated and never resolves bearer identity.
//! Ordinary user routes use only UserContext; principal routes additionally use
//! existing MCP identity/capability/admin policy. JSON syntax is checked before
//! dependencies; model/path validation follows dependencies, before authorization.
//! The host owns mounting, body limits, cancellation transport, forwarded-origin
//! preprocessing and root_path. Do not wrap the callback in bearer-only middleware.
//! Construction is inert. No provider requests or persistence are performed here.
//! JSON parser diagnostic wording/Unicode offsets and nonfinite JSON values
//! remain declared host transport boundaries, not claimed exact Python parity.
use crate::{
    config_audit_writer::ConfigAuditRequestContext,
    connector_principal_validation::{project_principal, validate_create},
    connector_user_service::ConnectorService,
    connector_user_types::{ConnectorError, ConnectorSearchLimit, ConnectorUser},
    mcp_registry_api::{McpRegistryIdentity, McpRegistryIdentityResolver},
    mcp_registry_store::{McpContext, McpError, McpRepository},
    mcp_registry_validation::{field_error, uuid_value},
    rate_config_api::integer,
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
    extract::{FromRequest, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

pub use crate::native_contract::connectors::CONNECTOR_USER_ROUTES;

/// Canonical get_user_context seam, separate from principal AuthContext. Hosts
/// supporting anonymous/guest users must retain their existing user resolution.
pub trait ConnectorUserIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, ConnectorUser>;
    fn context(&self, headers: &HeaderMap) -> ConfigAuditRequestContext {
        ConfigAuditRequestContext {
            headers: headers.clone(),
            ..Default::default()
        }
    }
}
impl ConnectorUserIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, ConnectorUser> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let user = request
                .resolve()
                .await
                .map_err(|e| ServiceConfigError::detail(e.status(), e.to_string()))?;
            Ok(ConnectorUser {
                user_id: user.user_id().into(),
                tenant_id: user.tenant_id().into(),
            })
        })
    }
}

/// Optional host-provided equivalent of request.base_url, including ASGI root_path.
/// Forwarded headers are never trusted by this module itself.
#[derive(Clone)]
pub struct ConnectorRequestBaseUrl(pub String);
pub struct ConnectorUserDependencies {
    pub service: Arc<ConnectorService>,
    pub user_identity: Arc<dyn ConnectorUserIdentityResolver>,
    pub principal_identity: Arc<dyn McpRegistryIdentityResolver>,
    pub rbac: Rbac,
    pub principal_enabled: bool,
    pub principals: Option<Arc<dyn McpRepository>>,
    /// Separate app.database presence, just as in the source audit helper.
    pub audit: Option<Arc<dyn McpRepository>>,
}
pub struct ConnectorUserApi {
    dependencies: ConnectorUserDependencies,
}
/// Callback query values intentionally do not implement Debug.
#[derive(Clone, PartialEq, Eq)]
pub enum ConnectorUserOperation {
    Available,
    Mine,
    Begin {
        provider: String,
    },
    Callback {
        provider: String,
        code: Option<String>,
        error: Option<String>,
        state: String,
    },
    Disconnect {
        provider: String,
    },
    Activate {
        provider: String,
    },
    Status {
        provider: String,
    },
    Search {
        provider: String,
    },
    CreatePrincipal {
        provider: String,
    },
    ListPrincipals {
        provider: String,
    },
    RevokePrincipal {
        provider: String,
        grant_id: String,
    },
}
impl ConnectorUserOperation {
    pub fn path(&self) -> String {
        match self {
            Self::Available => "/api/v1/connectors/available".into(),
            Self::Mine => "/api/v1/connectors/mine".into(),
            Self::Begin { provider } => format!("/api/v1/connectors/auth/{provider}"),
            Self::Callback { provider, .. } => format!("/api/v1/connectors/callback/{provider}"),
            Self::Disconnect { provider } => format!("/api/v1/connectors/{provider}"),
            Self::Activate { provider } => format!("/api/v1/connectors/{provider}/activate"),
            Self::Status { provider } => format!("/api/v1/connectors/{provider}/mcp-status"),
            Self::Search { provider } => format!("/api/v1/connectors/{provider}/search"),
            Self::CreatePrincipal { provider } | Self::ListPrincipals { provider } => {
                format!("/api/v1/connectors/{provider}/principals")
            }
            Self::RevokePrincipal { provider, grant_id } => {
                format!("/api/v1/connectors/{provider}/principals/{grant_id}")
            }
        }
    }
    pub fn principal(&self) -> bool {
        matches!(
            self,
            Self::CreatePrincipal { .. }
                | Self::ListPrincipals { .. }
                | Self::RevokePrincipal { .. }
        )
    }
    fn body(&self) -> bool {
        matches!(self, Self::Search { .. } | Self::CreatePrincipal { .. })
    }
    fn status(&self) -> u16 {
        if matches!(self, Self::CreatePrincipal { .. }) {
            201
        } else {
            200
        }
    }
}
#[derive(Clone, PartialEq)]
pub enum ConnectorApiResponse {
    Json { status: u16, body: Value },
    Redirect { location: String },
}
impl ConnectorUserApi {
    pub fn new(dependencies: ConnectorUserDependencies) -> Self {
        Self { dependencies }
    }
    /// Direct dispatch for deterministic ordered-effect tests; body must already
    /// be decoded. HTTP syntax parsing is implemented in the router below.
    pub async fn handle(
        &self,
        operation: ConnectorUserOperation,
        headers: &HeaderMap,
        body: &Value,
        base_url: &str,
    ) -> Result<ConnectorApiResponse, ServiceConfigError> {
        let mut context = self.context(&operation, headers);
        self.execute(operation, headers, body, None, base_url, &mut context)
            .await
    }
    fn context(
        &self,
        operation: &ConnectorUserOperation,
        headers: &HeaderMap,
    ) -> ConfigAuditRequestContext {
        if operation.principal() {
            self.dependencies.principal_identity.context(headers)
        } else if matches!(operation, ConnectorUserOperation::Callback { .. }) {
            ConfigAuditRequestContext {
                headers: headers.clone(),
                ..Default::default()
            }
        } else {
            self.dependencies.user_identity.context(headers)
        }
    }
    async fn execute(
        &self,
        mut operation: ConnectorUserOperation,
        headers: &HeaderMap,
        body: &Value,
        raw_limit: Option<&str>,
        base_url: &str,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<ConnectorApiResponse, ServiceConfigError> {
        if let ConnectorUserOperation::Callback {
            provider,
            code,
            error,
            state,
        } = operation
        {
            if code.is_none() && error.is_none() {
                return Err(ServiceConfigError::validation(vec![
                    json!({"type":"missing","loc":["query","code"],"msg":"Field required","input":null}),
                ]));
            }
            let location = self
                .dependencies
                .service
                .callback(
                    &provider,
                    code.as_deref(),
                    error.as_deref(),
                    &state,
                    base_url,
                )
                .await
                .map_err(connector_error)?;
            return Ok(ConnectorApiResponse::Redirect { location });
        }
        if operation.principal() {
            let identity = self
                .dependencies
                .principal_identity
                .resolve(headers, &operation.path())
                .await?;
            let payload = normalize_principal(&mut operation, body, context)?;
            self.authorize(
                &identity,
                !matches!(operation, ConnectorUserOperation::ListPrincipals { .. }),
                context,
            )?;
            let identity = McpContext {
                tenant_id: identity.tenant_id,
                user_id: identity.user_id,
                authenticated: identity.authenticated,
            };
            let status = operation.status();
            let body = self
                .principal_operation(operation, &identity, payload, context)
                .await?;
            return Ok(ConnectorApiResponse::Json { status, body });
        }
        let user = self
            .dependencies
            .user_identity
            .resolve(headers, &operation.path())
            .await?;
        let result = match operation {
            ConnectorUserOperation::Available => self.dependencies.service.available(&user).await,
            ConnectorUserOperation::Mine => self.dependencies.service.mine(&user).await,
            ConnectorUserOperation::Begin { provider } => {
                self.dependencies
                    .service
                    .begin(&user, &provider, base_url)
                    .await
            }
            ConnectorUserOperation::Disconnect { provider } => {
                self.dependencies.service.disconnect(&user, &provider).await
            }
            ConnectorUserOperation::Activate { provider } => {
                self.dependencies.service.activate(&user, &provider).await
            }
            ConnectorUserOperation::Status { provider } => {
                self.dependencies.service.status(&user, &provider).await
            }
            ConnectorUserOperation::Search { provider } => {
                let (query, limit) = validate_search_with_raw_limit(body, raw_limit)?;
                self.dependencies
                    .service
                    .search_exact(&user, &provider, &query, &limit)
                    .await
            }
            _ => unreachable!("principal/callback already handled"),
        };
        Ok(ConnectorApiResponse::Json {
            status: 200,
            body: result.map_err(connector_error)?,
        })
    }
    fn repository(
        &self,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<&dyn McpRepository, ServiceConfigError> {
        if !self.dependencies.principal_enabled {
            return Err(mcp_error(
                context,
                503,
                "MCP_DISABLED",
                "MCP registry is disabled",
            ));
        }
        self.dependencies.principals.as_deref().ok_or_else(|| {
            mcp_error(
                context,
                503,
                "MCP_STORAGE_UNAVAILABLE",
                "MCP registry storage unavailable",
            )
        })
    }
    fn authorize(
        &self,
        identity: &McpRegistryIdentity,
        write: bool,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<(), ServiceConfigError> {
        if !identity.authenticated || identity.user_id.is_empty() {
            return Err(mcp_error(
                context,
                401,
                "AUTHENTICATION_REQUIRED",
                "Authentication required",
            ));
        }
        if identity.tenant_id.is_empty() || identity.tenant_id == "public" {
            return Err(mcp_error(
                context,
                403,
                "TENANT_REQUIRED",
                "Tenant identity required",
            ));
        }
        let capability = if write {
            Capability::GatewayMcpWrite
        } else {
            Capability::GatewayMcpRead
        };
        self.dependencies
            .principal_identity
            .capability_check_started(capability);
        if !check_capability(
            &self.dependencies.rbac,
            &identity.roles,
            &identity.permissions,
            capability,
        )
        .allowed()
        {
            self.dependencies
                .principal_identity
                .permission_denied(identity, capability, context);
            let trace = context
                .request_id
                .as_ref()
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    context
                        .trace_id
                        .as_ref()
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or("");
            return Err(ServiceConfigError::detail(
                403,
                permission_denied_detail(capability, trace, None),
            ));
        }
        if write
            && !identity.roles.iter().any(|role| {
                matches!(
                    role.to_lowercase().as_str(),
                    "admin" | "tenant_admin" | "superadmin" | "super_admin"
                )
            })
        {
            return Err(mcp_error(
                context,
                403,
                "MCP_ADMIN_REQUIRED",
                "Tenant administrator access is required",
            ));
        }
        Ok(())
    }
    async fn call(
        &self,
        operation: &str,
        identity: &McpContext,
        arguments: Value,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        self.repository(context)?
            .call(operation, identity, arguments)
            .await
            .map_err(|e| repository_error(context, e))
    }
    async fn audit(
        &self,
        identity: &McpContext,
        action: &str,
        resource_id: &str,
        summary: Value,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<String, ServiceConfigError> {
        let fallback = format!("request:{}", request_id(context));
        let Some(audit) = &self.dependencies.audit else {
            return Ok(fallback);
        };
        match audit.call("audit_mutation", identity, json!({"action":action,"resource_type":"connector_principal","resource_id":resource_id,"summary":summary})).await {
            Ok(value) if !value.is_null() => Ok(format!("audit:{}", python_value_to_string(&value))),
            Err(McpError::Cancelled) => Err(ServiceConfigError::Cancelled),
            _ => Ok(fallback),
        }
    }
    async fn principal_operation(
        &self,
        operation: ConnectorUserOperation,
        identity: &McpContext,
        mut payload: Value,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        let provider = match &operation {
            ConnectorUserOperation::CreatePrincipal { provider }
            | ConnectorUserOperation::ListPrincipals { provider }
            | ConnectorUserOperation::RevokePrincipal { provider, .. } => provider,
            _ => unreachable!(),
        };
        if provider != "confluence" {
            return Err(ServiceConfigError::detail(
                422,
                "Only existing V1 Connector types are supported",
            ));
        }
        match operation {
            ConnectorUserOperation::CreatePrincipal { provider } => {
                payload["provider"] = json!(provider);
                let row = self
                    .call(
                        "create_connector_principal",
                        identity,
                        payload.clone(),
                        context,
                    )
                    .await?;
                // Audit precedes response-model validation, as in the source.
                let id = row
                    .get("grant_id")
                    .ok_or(ServiceConfigError::Internal)
                    .map(python_value_to_string)?;
                let audit = self.audit(identity, "create", &id, json!({"provider":provider,"principal_type":payload["principal_type"],"owner_user_id":payload["owner_user_id"],"allowed_channels":payload["allowed_channels"]}), context).await?;
                let principal = project_principal(&row)?;
                Ok(
                    json!({"principal":principal,"request_id":request_id(context),"audit_ref":audit}),
                )
            }
            ConnectorUserOperation::ListPrincipals { provider } => {
                let rows = self
                    .call(
                        "list_connector_principals",
                        identity,
                        json!({"provider":provider}),
                        context,
                    )
                    .await?;
                let principals = rows
                    .as_array()
                    .ok_or(ServiceConfigError::Internal)?
                    .iter()
                    .map(project_principal)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(json!({"total":principals.len(),"principals":principals}))
            }
            ConnectorUserOperation::RevokePrincipal { provider, grant_id } => {
                self.call(
                    "revoke_connector_principal",
                    identity,
                    json!({"grant_id":grant_id}),
                    context,
                )
                .await?;
                let audit = self
                    .audit(
                        identity,
                        "revoke",
                        &grant_id,
                        json!({"provider":provider}),
                        context,
                    )
                    .await?;
                Ok(json!({"status":"revoked","request_id":request_id(context),"audit_ref":audit}))
            }
            _ => unreachable!(),
        }
    }
}

fn connector_error(error: ConnectorError) -> ServiceConfigError {
    if error.source_unhandled {
        ServiceConfigError::Internal
    } else {
        ServiceConfigError::detail(error.status, error.detail)
    }
}
fn request_id(context: &mut ConfigAuditRequestContext) -> String {
    let id = context
        .request_id
        .as_ref()
        .filter(|v| python_truthy(v))
        .or_else(|| context.trace_id.as_ref().filter(|v| python_truthy(v)))
        .map(python_value_to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    context.request_id = Some(json!(id));
    id
}
fn mcp_error(
    context: &mut ConfigAuditRequestContext,
    status: u16,
    code: &str,
    message: &str,
) -> ServiceConfigError {
    ServiceConfigError::detail(
        status,
        json!({"code":code,"message":message,"request_id":request_id(context)}),
    )
}
fn repository_error(
    context: &mut ConfigAuditRequestContext,
    error: McpError,
) -> ServiceConfigError {
    match error {
        McpError::NotFound(code) => mcp_error(context, 404, &code, "MCP resource not found"),
        McpError::Conflict(code) => mcp_error(
            context,
            409,
            &code,
            if code == "MCP_RESOURCE_CONFLICT" {
                "MCP resource conflict"
            } else {
                "MCP resource conflicts with existing state"
            },
        ),
        McpError::Validation(code) => {
            mcp_error(context, 422, &code, "MCP configuration is invalid")
        }
        McpError::Authorization(code) => {
            mcp_error(context, 403, &code, "MCP operation is not authorized")
        }
        McpError::Broker(code) => mcp_error(context, 502, &code, "MCP server unavailable"),
        McpError::Storage => mcp_error(
            context,
            503,
            "MCP_STORAGE_UNAVAILABLE",
            "MCP registry storage unavailable",
        ),
        McpError::Cancelled => ServiceConfigError::Cancelled,
        McpError::Unhandled => ServiceConfigError::Internal,
    }
}
fn redacted_validation(
    context: &mut ConfigAuditRequestContext,
    fields: Vec<Value>,
) -> ServiceConfigError {
    ServiceConfigError::detail(
        422,
        json!({"code":"REQUEST_VALIDATION_FAILED","message":"Request validation failed","request_id":request_id(context),"fields":fields}),
    )
}
fn normalize_principal(
    operation: &mut ConnectorUserOperation,
    body: &Value,
    context: &mut ConfigAuditRequestContext,
) -> Result<Value, ServiceConfigError> {
    let mut errors = Vec::new();
    if let ConnectorUserOperation::RevokePrincipal { grant_id, .. } = operation {
        match uuid_value(&json!(*grant_id)) {
            Ok(value) => *grant_id = value.as_str().unwrap().into(),
            Err(kind) => errors.push(field_error(["path".into(), "grant_id".into()], kind)),
        }
    }
    let payload = if matches!(operation, ConnectorUserOperation::CreatePrincipal { .. }) {
        match validate_create(body) {
            Ok(value) => value,
            Err(fields) => {
                errors.extend(fields);
                Value::Null
            }
        }
    } else {
        Value::Null
    };
    if errors.is_empty() {
        Ok(payload)
    } else {
        Err(redacted_validation(context, errors))
    }
}
/// Source search accepts unknown keys and has no minimum/maximum limit.
pub fn validate_search(body: &Value) -> Result<(String, i64), ServiceConfigError> {
    let Some(body) = body.as_object() else {
        return Err(ServiceConfigError::validation(vec![if body.is_null() {
            json!({"type":"missing","loc":["body"],"msg":"Field required","input":null})
        } else {
            json!({"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":body})
        }]));
    };
    let mut errors = Vec::new();
    let query = match body.get("query") {
        None => {
            errors.push(json!({"type":"missing","loc":["body","query"],"msg":"Field required","input":body}));
            String::new()
        }
        Some(Value::String(query)) => query.clone(),
        Some(value) => {
            errors.push(json!({"type":"string_type","loc":["body","query"],"msg":"Input should be a valid string","input":value}));
            String::new()
        }
    };
    let limit = match body.get("limit") {
        None => 10,
        Some(value) => match integer(value) {
            Ok(value) => value,
            Err(kind) => {
                let message = match kind {
                    "int_parsing" => {
                        "Input should be a valid integer, unable to parse string as an integer"
                    }
                    "int_from_float" => {
                        "Input should be a valid integer, got a number with a fractional part"
                    }
                    _ => "Input should be a valid integer",
                };
                errors
                    .push(json!({"type":kind,"loc":["body","limit"],"msg":message,"input":value}));
                10
            }
        },
    };
    if errors.is_empty() {
        Ok((query, limit))
    } else {
        Err(ServiceConfigError::validation(errors))
    }
}

/// Arbitrary-precision integer limits preserve Python's unbounded int contract.
/// HTTP additionally supplies the raw JSON integer token, before serde_json's
/// ordinary Value representation could round an integer larger than u64.
pub fn validate_search_exact(
    body: &Value,
) -> Result<(String, ConnectorSearchLimit), ServiceConfigError> {
    validate_search_with_raw_limit(body, None)
}
fn validate_search_with_raw_limit(
    body: &Value,
    raw_limit: Option<&str>,
) -> Result<(String, ConnectorSearchLimit), ServiceConfigError> {
    let Some(object) = body.as_object() else {
        return validate_search(body).map(|(query, limit)| {
            (
                query,
                ConnectorSearchLimit::from_decimal(&limit.to_string()).expect("i64 is decimal"),
            )
        });
    };
    let mut errors = Vec::new();
    let query = match object.get("query") {
        None => {
            errors.push(json!({"type":"missing","loc":["body","query"],"msg":"Field required","input":body}));
            String::new()
        }
        Some(Value::String(query)) => query.clone(),
        Some(value) => {
            errors.push(json!({"type":"string_type","loc":["body","query"],"msg":"Input should be a valid string","input":value}));
            String::new()
        }
    };
    let limit = match object.get("limit") {
        None => ConnectorSearchLimit::from_decimal("10"),
        Some(value) => match exact_limit(value, raw_limit) {
            Ok(limit) => Some(limit),
            Err(kind) => {
                let message = match kind {
                    "int_parsing" => {
                        "Input should be a valid integer, unable to parse string as an integer"
                    }
                    "int_from_float" => {
                        "Input should be a valid integer, got a number with a fractional part"
                    }
                    "finite_number" => "Input should be a finite number",
                    _ => "Input should be a valid integer",
                };
                errors
                    .push(json!({"type":kind,"loc":["body","limit"],"msg":message,"input":value}));
                None
            }
        },
    };
    if errors.is_empty() {
        Ok((query, limit.expect("valid/default decimal")))
    } else {
        Err(ServiceConfigError::validation(errors))
    }
}
fn exact_limit(value: &Value, raw: Option<&str>) -> Result<ConnectorSearchLimit, &'static str> {
    if let Some(raw) = raw {
        let raw = raw.trim();
        if !raw.contains(['.', 'e', 'E']) && !raw.starts_with('"') {
            if let Some(value) = ConnectorSearchLimit::from_decimal(raw) {
                return Ok(value);
            }
        }
    }
    match value {
        Value::Bool(value) => {
            Ok(ConnectorSearchLimit::from_decimal(if *value { "1" } else { "0" }).unwrap())
        }
        Value::String(value) => {
            let value = value.trim();
            let (integer, fraction) = value
                .split_once('.')
                .map_or((value, None), |(i, f)| (i, Some(f)));
            if fraction.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b == b'0')) {
                return Err("int_parsing");
            }
            let bytes = integer.as_bytes();
            if bytes.iter().enumerate().any(|(i, b)| {
                *b == b'_'
                    && (i == 0
                        || !bytes[i - 1].is_ascii_digit()
                        || !bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
            }) {
                return Err("int_parsing");
            }
            ConnectorSearchLimit::from_decimal(&integer.replace('_', "")).ok_or("int_parsing")
        }
        Value::Number(value) => {
            if value.is_i64() || value.is_u64() {
                return ConnectorSearchLimit::from_decimal(&value.to_string()).ok_or("int_parsing");
            }
            // Python json.loads makes decimal/exponent tokens binary floats;
            // Pydantic then converts that exact integral float into a Python int.
            let value = value.as_f64().ok_or("int_type")?;
            if !value.is_finite() {
                return Err("finite_number");
            }
            if value.fract() != 0.0 {
                return Err("int_from_float");
            }
            ConnectorSearchLimit::from_decimal(&format!("{value:.0}")).ok_or("int_parsing")
        }
        _ => Err("int_type"),
    }
}

#[derive(Clone)]
struct HttpState {
    api: Arc<ConnectorUserApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
}
/// All eleven routes. Callback must remain outside bearer-only host middleware.
/// Hosts merging custom-fallback routers must reset_fallback() before merge.
pub fn router(
    api: Arc<ConnectorUserApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
) -> Router {
    route_subset(api, cancellation, None)
}
/// Use this factory when the host wraps ordinary routes in authentication
/// middleware. Mount this callback router outside that middleware.
pub fn callback_router(
    api: Arc<ConnectorUserApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
) -> Router {
    route_subset(api, cancellation, Some(true))
}
/// Ten routes with identity dependencies. Callback is deliberately excluded.
pub fn authenticated_router(
    api: Arc<ConnectorUserApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
) -> Router {
    route_subset(api, cancellation, Some(false))
}
fn route_subset(
    api: Arc<ConnectorUserApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
    callback_only: Option<bool>,
) -> Router {
    let mut router = Router::new();
    let mut seen = Vec::new();
    for &(_, path, _) in CONNECTOR_USER_ROUTES {
        if callback_only.is_some_and(|only| only != (path == "/connectors/callback/{provider}")) {
            continue;
        }
        if !seen.contains(&path) {
            seen.push(path);
            router = router.route(&format!("/api/v1{path}"), any(dispatch));
        }
    }
    router
        .fallback(fallback)
        .with_state(HttpState { api, cancellation })
}
fn decoded_parts(path: &str) -> Vec<String> {
    path.split('/')
        .map(|part| {
            let escaped = part
                .replace('+', "%2B")
                .replace('#', "%23")
                .replace('&', "%26");
            reqwest::Url::parse(&format!("https://connector.invalid/?v={escaped}"))
                .ok()
                .and_then(|url| {
                    url.query_pairs()
                        .next()
                        .map(|(_, value)| value.into_owned())
                })
                .unwrap_or_else(|| part.into())
        })
        .collect()
}
fn match_template(template: &str, parts: &[String]) -> Option<HashMap<String, String>> {
    let expected = template.split('/').collect::<Vec<_>>();
    if expected.len() != parts.len() {
        return None;
    }
    let mut result = HashMap::new();
    for (template, actual) in expected.into_iter().zip(parts) {
        if template.starts_with('{') && template.ends_with('}') {
            if actual.is_empty() || actual.contains('/') {
                return None;
            }
            result.insert(template[1..template.len() - 1].into(), actual.clone());
        } else if template != actual {
            return None;
        }
    }
    Some(result)
}
fn query_parameters(query: &str) -> HashMap<String, String> {
    reqwest::Url::parse(&format!("https://connector.invalid/?{query}"))
        .map(|url| {
            url.query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}
/// Full method/path scan preserves FastAPI's partial-match fallthrough. For
/// example DELETE /available selects /{provider}, and GET /auth/principals
/// selects the earlier OAuth route rather than the later principal router.
pub fn operation_for_request(
    method: &str,
    path: &str,
    query: &str,
) -> Option<ConnectorUserOperation> {
    let parts = decoded_parts(path);
    let query = query_parameters(query);
    for &(verb, template, name) in CONNECTOR_USER_ROUTES {
        if verb != method {
            continue;
        }
        let Some(params) = match_template(&format!("/api/v1{template}"), &parts) else {
            continue;
        };
        let provider = params.get("provider").cloned().unwrap_or_default();
        return Some(match name {
            "list_available_connectors" => ConnectorUserOperation::Available,
            "list_my_connectors" => ConnectorUserOperation::Mine,
            "initiate_oauth" => ConnectorUserOperation::Begin { provider },
            "oauth_callback" => ConnectorUserOperation::Callback {
                provider,
                code: query.get("code").cloned(),
                error: query.get("error").cloned(),
                state: query.get("state").cloned().unwrap_or_default(),
            },
            "disconnect_connector" => ConnectorUserOperation::Disconnect { provider },
            "activate_connector_mcp" => ConnectorUserOperation::Activate { provider },
            "connector_mcp_status" => ConnectorUserOperation::Status { provider },
            "search_connector" => ConnectorUserOperation::Search { provider },
            "create_connector_principal" => ConnectorUserOperation::CreatePrincipal { provider },
            "list_connector_principals" => ConnectorUserOperation::ListPrincipals { provider },
            "revoke_connector_principal" => ConnectorUserOperation::RevokePrincipal {
                provider,
                grant_id: params.get("grant_id").cloned().unwrap_or_default(),
            },
            _ => unreachable!(),
        });
    }
    None
}
fn request_base_url(request: &Request) -> String {
    if let Some(base) = request.extensions().get::<ConnectorRequestBaseUrl>() {
        return base.0.clone();
    }
    let scheme = request.uri().scheme_str().unwrap_or("http");
    let host = request
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .or_else(|| request.uri().authority().map(|a| a.as_str()))
        .unwrap_or("");
    format!("{scheme}://{host}/")
}
async fn respond(
    state: &HttpState,
    result: Result<ConnectorApiResponse, ServiceConfigError>,
    context: &ConfigAuditRequestContext,
) -> Response {
    let mut response = match result {
        Ok(ConnectorApiResponse::Json { status, body }) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(body),
        )
            .into_response(),
        Ok(ConnectorApiResponse::Redirect { location }) => {
            (StatusCode::FOUND, [("location", location)]).into_response()
        }
        Err(ServiceConfigError::Cancelled) => state.cancellation.cancelled().await,
        Err(ServiceConfigError::Internal | ServiceConfigError::Exception(_)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
        Err(error) => {
            let (status, body) = error.response(context).expect("not cancelled");
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            // FastAPI's HTTPException handler does not emit detail JSON for
            // informational statuses or 204/205/304. Provider searches forward
            // arbitrary non-200 upstream statuses, so this branch is reachable.
            if status.as_u16() < 200 || matches!(status.as_u16(), 204 | 205 | 304) {
                let mut response = status.into_response();
                // Starlette Response populates length for 205, but not 1xx,
                // 204 or 304. None of these responses has a Content-Type.
                if status.as_u16() == 205 {
                    response.headers_mut().insert(
                        axum::http::header::CONTENT_LENGTH,
                        axum::http::HeaderValue::from_static("0"),
                    );
                }
                response
            } else {
                (status, Json(body)).into_response()
            }
        }
    };
    response.extensions_mut().insert(context.clone());
    response
}
async fn dispatch(State(state): State<HttpState>, request: Request) -> Response {
    let Some(operation) = operation_for_request(
        request.method().as_str(),
        request.uri().path(),
        request.uri().query().unwrap_or(""),
    ) else {
        let parts = decoded_parts(request.uri().path());
        let allow = CONNECTOR_USER_ROUTES
            .iter()
            .find(|(_, template, _)| {
                match_template(&format!("/api/v1{template}"), &parts).is_some()
            })
            .map(|(method, _, _)| *method);
        return match allow {
            Some(allow) => (
                StatusCode::METHOD_NOT_ALLOWED,
                [("allow", allow)],
                Json(json!({"detail":"Method Not Allowed"})),
            )
                .into_response(),
            None => (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response(),
        };
    };
    let base_url = request_base_url(&request);
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
        .unwrap_or_else(|| state.api.context(&operation, &headers));
    context.headers = headers.clone();
    let mut raw_limit = None;
    let body = if operation.body() {
        let bytes = match Bytes::from_request(request, &state).await {
            Ok(bytes) => bytes,
            Err(error) => return error.into_response(),
        };
        if bytes.is_empty() {
            Value::Null
        } else if crate::http_content_type::is_json_content_type(&headers) {
            let mut exact_body = None;
            if matches!(operation, ConnectorUserOperation::Search { .. }) {
                if let Ok(mut object) = serde_json::from_slice::<
                    HashMap<String, Box<serde_json::value::RawValue>>,
                >(&bytes)
                {
                    raw_limit = object.get("limit").map(|value| value.get().to_owned());
                    if let Some(raw) = raw_limit.as_deref().filter(|raw| {
                        ConnectorSearchLimit::from_decimal(raw).is_some()
                            && raw.parse::<i64>().is_err()
                            && raw.parse::<u64>().is_err()
                    }) {
                        // Keep an unbounded integer out of Value's floating-point
                        // fallback altogether. Its exact token travels separately.
                        let quoted = serde_json::to_string(raw).expect("string encoding");
                        object.insert(
                            "limit".into(),
                            serde_json::value::RawValue::from_string(quoted)
                                .expect("encoded JSON string"),
                        );
                        exact_body = serde_json::to_vec(&object).ok();
                    }
                }
            }
            match serde_json::from_slice(exact_body.as_deref().unwrap_or(&bytes)) {
                Ok(value) => value,
                Err(error) => {
                    let prefix = bytes
                        .split(|b| *b == b'\n')
                        .take(error.line().saturating_sub(1))
                        .map(|line| line.len() + 1)
                        .sum::<usize>();
                    let offset = prefix
                        + if error.is_eof() {
                            error.column()
                        } else {
                            error.column().saturating_sub(1)
                        };
                    let error = if operation.principal() {
                        redacted_validation(
                            &mut context,
                            vec![field_error(
                                ["body".into(), offset.to_string()],
                                "json_invalid",
                            )],
                        )
                    } else {
                        // No raw body or provider credentials in decoder diagnostics.
                        ServiceConfigError::validation(vec![
                            json!({"type":"json_invalid","loc":["body",offset],"msg":"JSON decode error","input":{},"ctx":{"error":"Invalid JSON"}}),
                        ])
                    };
                    return respond(&state, Err(error), &context).await;
                }
            }
        } else {
            json!(String::from_utf8_lossy(&bytes))
        }
    } else {
        Value::Null
    };
    let result = state
        .api
        .execute(
            operation,
            &headers,
            &body,
            raw_limit.as_deref(),
            &base_url,
            &mut context,
        )
        .await;
    respond(&state, result, &context).await
}
async fn fallback(request: Request) -> Response {
    if request.uri().path().ends_with('/') {
        let canonical = request.uri().path().trim_end_matches('/');
        let parts = decoded_parts(canonical);
        if CONNECTOR_USER_ROUTES
            .iter()
            .any(|(_, template, _)| match_template(&format!("/api/v1{template}"), &parts).is_some())
        {
            let scheme = request.uri().scheme_str().unwrap_or("http");
            let host = request
                .headers()
                .get("host")
                .and_then(|h| h.to_str().ok())
                .or_else(|| request.uri().authority().map(|a| a.as_str()));
            let query = request
                .uri()
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default();
            let location = host
                .map(|host| format!("{scheme}://{host}{canonical}{query}"))
                .unwrap_or_else(|| format!("{canonical}{query}"));
            return (StatusCode::TEMPORARY_REDIRECT, [("location", location)]).into_response();
        }
    }
    (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response()
}
