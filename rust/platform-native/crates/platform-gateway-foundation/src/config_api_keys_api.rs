//! Unmounted legacy admin API-key configuration routes from config.py.
//!
//! Both operations use roles-only Rbac.require("admin"), without trimming or an
//! explicit-permissions override. The list is intentionally cross-tenant. Runtime
//! list rows contain synthetic indexed names, not the separate creation metadata.
//! Save/append precedes mature service-access cache invalidation, then awaited
//! mature audit, then one-time output. Audit failure/cancellation cannot roll back
//! a stored key. Runtime raw credentials live only in RuntimeAuthStore.
//!
//! Host supplies cryptographically secure 32-byte entropy. Construction is inert.
//! Database error rendering is an explicit trusted boundary: default native errors
//! are sanitized, and no SQL/provider/backend text is forwarded. Exact synthetic
//! source exception text can be supplied by the trusted renderer in parity tests.
//! Validation echoes user input as Pydantic does; no generated credentials enter
//! validation errors. Debug implementations omit all user payloads and secrets.
//! JSON syntax offsets, invalid encodings, body limits and slash redirects remain
//! transport boundaries. Missing Content-Type preserves bytes (current FastAPI).
//! Cancellation requires explicit host transport policy. Factory remains unmounted.
use crate::{
    auth_config_api::RuntimeAuthStore,
    config_api_keys_store::{ConfigApiKeySave, ConfigApiKeysDatabase, ConfigApiKeysStoreError},
    config_audit_writer::{
        ConfigAuditError, ConfigAuditRequestContext, ConfigAuditValue, ConfigAuditWriter,
        ConfigChange,
    },
    service_access_resolver::ServiceAccessConstraintResolver,
    service_config_api::{
        ServiceConfigCancellation, ServiceConfigError, ServiceConfigIdentity,
        ServiceConfigIdentityResolver,
    },
};
use ai_platform_auth::rbac::Rbac;
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fmt, sync::Arc};

pub const CONFIG_API_KEYS_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/config/api-keys", "list_api_keys"),
    ("POST", "/config/api-keys", "create_api_key"),
];
pub trait ConfigApiKeyRandom: Send + Sync {
    /// A production host must supply cryptographically secure, fresh entropy.
    fn bytes32(&self) -> Result<[u8; 32], ServiceConfigError>;
}
pub trait ConfigApiKeysErrorRenderer: Send + Sync {
    /// Trusted sanitized text only. Never format an underlying backend error.
    fn render(&self, error: &ConfigApiKeysStoreError) -> String;
}
pub struct RedactedConfigApiKeysErrors;
impl ConfigApiKeysErrorRenderer for RedactedConfigApiKeysErrors {
    fn render(&self, _: &ConfigApiKeysStoreError) -> String {
        "config_api_keys_database_error".into()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigApiKeysOperation {
    List,
    Create,
}
#[derive(Clone)]
pub struct ConfigApiKeyCreate {
    pub name: String,
    pub description: Option<String>,
    pub tenant_id: Option<String>,
    pub user_id: Option<String>,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub allowed_services: Vec<String>,
    pub allowed_models: Vec<String>,
    pub tier: String,
}
impl fmt::Debug for ConfigApiKeyCreate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConfigApiKeyCreate([REDACTED])")
    }
}
pub struct ConfigApiKeysDependencies {
    pub identity: Arc<dyn ServiceConfigIdentityResolver>,
    pub rbac: Rbac,
    pub database: Option<Arc<dyn ConfigApiKeysDatabase>>,
    pub runtime: Arc<RuntimeAuthStore>,
    pub access: ServiceAccessConstraintResolver,
    pub audit: ConfigAuditWriter,
    pub random: Arc<dyn ConfigApiKeyRandom>,
    pub errors: Arc<dyn ConfigApiKeysErrorRenderer>,
}
pub struct ConfigApiKeysApi {
    dependencies: ConfigApiKeysDependencies,
}
impl ConfigApiKeysApi {
    pub fn new(dependencies: ConfigApiKeysDependencies) -> Self {
        Self { dependencies }
    }
    pub async fn handle(
        &self,
        operation: ConfigApiKeysOperation,
        headers: &HeaderMap,
        body: &Value,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        let identity = self
            .dependencies
            .identity
            .resolve(headers, "/api/v1/config/api-keys")
            .await?;
        self.execute(operation, &identity, body, context).await
    }
    fn database_error(&self, error: ConfigApiKeysStoreError, saving: bool) -> ServiceConfigError {
        if error == ConfigApiKeysStoreError::Cancelled {
            return ServiceConfigError::Cancelled;
        }
        let prefix = if saving {
            "数据库保存失败"
        } else {
            "数据库查询失败"
        };
        ServiceConfigError::detail(
            500,
            format!("{prefix}: {}", self.dependencies.errors.render(&error)),
        )
    }
    pub async fn execute(
        &self,
        operation: ConfigApiKeysOperation,
        identity: &ServiceConfigIdentity,
        body: &Value,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        // FastAPI resolves identity then validates the request model before endpoint RBAC.
        let create = if operation == ConfigApiKeysOperation::Create {
            Some(validate_api_key_create(body)?)
        } else {
            None
        };
        self.dependencies
            .rbac
            .require(&identity.roles, "admin")
            .map_err(|error| ServiceConfigError::Http {
                status: 403,
                body: json!({"error":error}),
            })?;
        let database = self
            .dependencies
            .database
            .as_ref()
            .filter(|db| db.enabled());
        let Some(create) = create else {
            let keys = if let Some(database) = database {
                database
                    .list()
                    .await
                    .map_err(|e| self.database_error(e, false))?
            } else {
                (0..self.dependencies.runtime.key_count())
                    .map(|i| json!({"id":i,"name":format!("Key {}",i+1),"enabled":true}))
                    .collect()
            };
            return Ok(json!({"keys":keys}));
        };
        let api_key = format!(
            "gw_{}",
            URL_SAFE_NO_PAD.encode(self.dependencies.random.bytes32()?)
        );
        let key_hash = format!("{:x}", Sha256::digest(api_key.as_bytes()));
        let tenant = create
            .tenant_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("default");
        if let Some(database) = database {
            let save = ConfigApiKeySave {
                key_hash,
                name: Some(create.name.clone()),
                description: create.description.clone(),
                tenant_id: Some(tenant.into()),
                user_id: create.user_id.clone().filter(|s| !s.is_empty()),
                roles: create.roles.clone(),
                permissions: create.permissions.clone(),
                tier: create.tier.clone(),
                rate_limit: None,
                allowed_services: create.allowed_services.clone(),
                allowed_models: create.allowed_models.clone(),
                expires_at: None,
            };
            database
                .save(&save)
                .await
                .map_err(|e| self.database_error(e, true))?;
        } else {
            self.dependencies.runtime.append_api_key(api_key.clone(), json!({"key_hash":key_hash,"name":create.name,"description":create.description,"user_id":create.user_id,"roles":create.roles,"permissions":create.permissions,"allowed_services":create.allowed_services,"allowed_models":create.allowed_models,"tier":create.tier}));
        }
        self.dependencies.access.clear_cache();
        let after = ConfigAuditValue::from(
            json!({"name":create.name,"tenant_id":tenant,"user_id":create.user_id,"roles":create.roles,"permissions":create.permissions,"api_key":api_key}),
        );
        self.dependencies
            .audit
            .record_config_change(
                context,
                identity,
                ConfigChange {
                    resource_type: "api_key",
                    resource_id: &create.name,
                    action: "create",
                    before: None,
                    after: Some(&after),
                },
            )
            .await
            .map_err(|e| {
                if e == ConfigAuditError::Cancelled {
                    ServiceConfigError::Cancelled
                } else {
                    ServiceConfigError::Internal
                }
            })?;
        Ok(
            json!({"status":"success","api_key":api_key,"message":"请保存此 API Key，它不会再次显示"}),
        )
    }
}
fn validation(kind: &str, loc: Vec<Value>, input: &Value) -> Value {
    let msg = match kind {
        "missing" => "Field required",
        "model_type" => "Input should be a valid dictionary or instance of ApiKeyCreate",
        "model_attributes_type" => {
            "Input should be a valid dictionary or object to extract fields from"
        }
        "string_type" => "Input should be a valid string",
        "list_type" => "Input should be a valid list",
        _ => "Invalid value",
    };
    let mut error = json!({"type":kind,"loc":loc,"msg":msg,"input":input});
    if kind == "model_type" {
        error["ctx"] = json!({"class_name":"ApiKeyCreate"});
    }
    error
}
pub fn validate_api_key_create(body: &Value) -> Result<ConfigApiKeyCreate, ServiceConfigError> {
    let Some(fields) = body.as_object() else {
        return Err(ServiceConfigError::validation(vec![validation(
            "model_type",
            vec![json!("body")],
            body,
        )]));
    };
    let mut output = json!({"name":"","description":null,"tenant_id":null,"user_id":null,"roles":["user"],"permissions":[],"allowed_services":[],"allowed_models":[],"tier":"normal"});
    let mut errors = vec![];
    for field in [
        "name",
        "description",
        "tenant_id",
        "user_id",
        "roles",
        "permissions",
        "allowed_services",
        "allowed_models",
        "tier",
    ] {
        let Some(value) = fields.get(field) else {
            if field == "name" {
                errors.push(validation(
                    "missing",
                    vec![json!("body"), json!(field)],
                    body,
                ));
            }
            continue;
        };
        let loc = vec![json!("body"), json!(field)];
        if matches!(
            field,
            "roles" | "permissions" | "allowed_services" | "allowed_models"
        ) {
            if let Some(values) = value.as_array() {
                for (i, item) in values.iter().enumerate() {
                    if !item.is_string() {
                        let mut at = loc.clone();
                        at.push(json!(i));
                        errors.push(validation("string_type", at, item));
                    }
                }
                output[field] = value.clone();
            } else {
                errors.push(validation("list_type", loc, value));
            }
        } else if value.is_string()
            || (value.is_null() && matches!(field, "description" | "tenant_id" | "user_id"))
        {
            output[field] = value.clone();
        } else {
            errors.push(validation("string_type", loc, value));
        }
    }
    if !errors.is_empty() {
        return Err(ServiceConfigError::validation(errors));
    }
    let string = |name: &str| output[name].as_str().map(String::from);
    let array = |name: &str| {
        output[name]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().into())
            .collect()
    };
    Ok(ConfigApiKeyCreate {
        name: string("name").unwrap(),
        description: string("description"),
        tenant_id: string("tenant_id"),
        user_id: string("user_id"),
        roles: array("roles"),
        permissions: array("permissions"),
        allowed_services: array("allowed_services"),
        allowed_models: array("allowed_models"),
        tier: string("tier").unwrap(),
    })
}
#[derive(Clone)]
struct HttpState {
    api: Arc<ConfigApiKeysApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
}
pub fn router(
    api: Arc<ConfigApiKeysApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
) -> Router {
    Router::new()
        .route(
            "/api/v1/config/api-keys",
            any(|| async { StatusCode::METHOD_NOT_ALLOWED })
                .get(http_list)
                .post(http_create),
        )
        .route_layer(middleware::from_fn(route_guard))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn route_guard(request: Request, next: Next) -> Response {
    if matches!(request.method().as_str(), "GET" | "POST") {
        next.run(request).await
    } else {
        (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", "GET")],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response()
    }
}
fn context(
    state: &HttpState,
    headers: &HeaderMap,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
) -> ConfigAuditRequestContext {
    let mut context = trusted
        .map(|Extension(v)| v)
        .unwrap_or_else(|| state.api.dependencies.identity.context(headers));
    context.headers = headers.clone();
    context
}
async fn response(
    state: &HttpState,
    value: Result<Value, ServiceConfigError>,
    context: &ConfigAuditRequestContext,
) -> Response {
    match value {
        Ok(v) => Json(v).into_response(),
        Err(ServiceConfigError::Cancelled) => state.cancellation.cancelled().await,
        Err(e) => {
            let (status, body) = e.response(context).expect("noncancelled error");
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    }
}
async fn http_list(
    State(state): State<HttpState>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
) -> Response {
    let context = context(&state, &headers, trusted);
    response(
        &state,
        state
            .api
            .handle(
                ConfigApiKeysOperation::List,
                &headers,
                &Value::Null,
                &context,
            )
            .await,
        &context,
    )
    .await
}
async fn http_create(
    State(state): State<HttpState>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let context = context(&state, &headers, trusted);
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        let is_json = crate::http_content_type::is_json_content_type(&headers);
        if is_json {
            match serde_json::from_slice(&bytes){Ok(v)=>v,Err(_)=>return response(&state,Err(ServiceConfigError::validation(vec![json!({"type":"json_invalid","loc":["body",0],"msg":"JSON decode error","input":{},"ctx":{"error":"Invalid JSON"}})])),&context).await}
        } else {
            json!(String::from_utf8_lossy(&bytes))
        }
    };
    let identity = match state
        .api
        .dependencies
        .identity
        .resolve(&headers, "/api/v1/config/api-keys")
        .await
    {
        Ok(v) => v,
        Err(e) => return response(&state, Err(e), &context).await,
    };
    if !body.is_object() {
        let kind = if body.is_null() {
            "missing"
        } else {
            "model_attributes_type"
        };
        return response(
            &state,
            Err(ServiceConfigError::validation(vec![validation(
                kind,
                vec![json!("body")],
                &body,
            )])),
            &context,
        )
        .await;
    }
    response(
        &state,
        state
            .api
            .execute(ConfigApiKeysOperation::Create, &identity, &body, &context)
            .await,
        &context,
    )
    .await
}
