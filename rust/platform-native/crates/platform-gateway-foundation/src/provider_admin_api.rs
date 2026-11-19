//! Opt-in provider administration from `src/api/v1/providers.py`.
//!
//! All nine routes are present. Construction opens no connection, starts no task,
//! reads no environment/credentials and does not mount the default gateway. User
//! identity is supplied only by a trusted resolver, never from body/query fields.
//! The native admission adapter resolves current JWT/database-key authority. A host
//! needing guest/disabled-auth UserResolver modes must inject that trusted resolver.
//!
//! JSON request validation mirrors the canonical Pydantic provider schemas: ignored
//! extras, Unicode lengths, nullable updates and non-strict bool coercion. Existing
//! CreateProvider/UpdateProvider service contracts are reused. Non-JSON Python
//! objects, nonfinite numbers and arbitrary-precision JSON numbers are outside this
//! boundary. Response records must contain ISO datetime strings (native DB output),
//! not Pydantic's additional numeric-epoch datetime coercions. Invalid service rows
//! fail closed; public projections remove credentials and internal catalog fields.
//!
//! Transport boundary: JSON syntax is parsed before dependencies/authentication;
//! non-JSON bytes and semantic validation follow them, as in FastAPI. Malformed
//! JSON diagnostic offsets/text, body limits and invalid percent/UTF-8 diagnostics
//! remain bounded native transport behavior, not exact FastAPI diagnostics.
//! Semantic body/query errors use the canonical 422 detail array. Authentication
//! and dependency failures precede semantic validation, which precedes capability
//! checks. The host retains its body-limit/trace middleware; this factory adds none.
//!
//! Broad create catches (including metadata HTTPException -> 500), template update
//! without probe invalidation, audit-after-write errors, sequential invalidations,
//! and partial commits are deliberately retained. Dropping a future cancels the
//! current await without spawning/retrying/rolling back later work. External
//! adapters own cancellation cleanup and commit ambiguity.
//!
//! ProviderService preserves SQLSTATE 23505 as a safe UniqueConflict marker; its
//! fixed 'duplicate key' text retains create's canonical 409 without exposing SQL.
//! Other native errors retain existing sanitized service/probe text. Catalog sync
//! preserves InvalidIntegerLiteral as source ValueError/400 with fixed safe detail;
//! invalid metadata types and the native i64 range boundary remain Internal. Raw
//! Python exception strings are not reconstructed by these native adapters.
//! The HTTP boundary sanitizes legacy broad-create 500 details; source main.py
//! otherwise preserves HTTP/request-validation details and maps uncaught exceptions
//! to code 2000 with trace. Typed cancellation remains host-owned. HEAD/method
//! rejection and invalid single-segment IDs precede all collaborator calls.
//! Trailing-slash redirects require host policy with a trusted origin; Host and
//! forwarded headers never establish an origin in this unmounted factory.
use crate::{
    model_catalog_sync::{
        CatalogHttp, CatalogModels, CatalogProviders, CatalogSyncError, ModelCatalogSyncService,
    },
    model_failover::has_secret_field,
    protocol::ProtocolTimestamp,
    provider_health::{self, ProviderProbeRuntime, ProviderProbeStore},
    provider_service::{
        CreateProvider, ProviderCipher, ProviderClock, ProviderDatabase, ProviderEnvironment,
        ProviderGoogleAuth, ProviderHttp, ProviderService, UpdateProvider,
    },
    provider_templates::{get_provider_template, list_provider_templates},
    usage_parser::{python_value_to_string, python_whitespace},
};
use ai_platform_auth::{
    admission::AdmissionService,
    rbac::{Capability, Rbac, check_capability, permission_denied_detail},
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{FromRequest, MatchedPath, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

pub type ProviderAdminFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ProviderAdminBackendError>> + Send + 'a>>;

/// Error text is private and Debug-redacted. Legacy direct-domain create results
/// retain the source shape; the HTTP boundary sanitizes backend 500 details.
/// Adapters must never put credentials into exception strings.
pub struct ProviderAdminBackendError {
    message: String,
    value_error: bool,
    cancelled: bool,
}
impl ProviderAdminBackendError {
    pub fn cancelled() -> Self {
        Self {
            message: String::new(),
            value_error: false,
            cancelled: true,
        }
    }
    pub fn exception(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            value_error: false,
            cancelled: false,
        }
    }
    pub fn value_error(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            value_error: true,
            cancelled: false,
        }
    }
}
impl fmt::Debug for ProviderAdminBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProviderAdminBackendError(<internal>)")
    }
}

#[derive(Clone, PartialEq)]
pub enum ProviderAdminError {
    Http { status: u16, detail: Value },
    Internal,
    Cancelled,
}
impl fmt::Debug for ProviderAdminError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { status, .. } => write!(f, "ProviderAdminError::Http({status})"),
            Self::Internal => f.write_str("ProviderAdminError::Internal"),
            Self::Cancelled => f.write_str("ProviderAdminError::Cancelled"),
        }
    }
}
impl ProviderAdminError {
    pub fn http(status: u16, detail: impl Into<Value>) -> Self {
        Self::Http {
            status,
            detail: detail.into(),
        }
    }
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            Self::Internal => Some(500),
            Self::Cancelled => None,
        }
    }
    pub fn response_body(&self, trace_id: Option<&str>) -> Option<Value> {
        Some(match self {
            Self::Cancelled => return None,
            Self::Http { detail, .. } => json!({"detail": detail}),
            Self::Internal => {
                let mut body = json!({"error": {"code": 2000, "message": "Internal server error"}});
                if let Some(trace) = trace_id.filter(|s| !s.is_empty()) {
                    body["error"]["trace_id"] = trace.into();
                }
                body
            }
        })
    }
}
type ApiResult<T> = Result<T, ProviderAdminError>;

/// Trusted output of UserResolver/admission. Intentionally not Deserialize.
/// `roles` includes permission-like roles exactly as canonical UserContext does;
/// no separate permission list is considered by provider endpoint RBAC.
#[derive(Clone)]
pub struct ProviderAdminUser {
    user_id: String,
    tenant_id: String,
    roles: Vec<String>,
    is_authenticated: bool,
}
impl ProviderAdminUser {
    pub fn from_resolved_identity(
        user_id: String,
        tenant_id: String,
        roles: Vec<String>,
        is_authenticated: bool,
    ) -> Self {
        Self {
            user_id,
            tenant_id,
            roles,
            is_authenticated,
        }
    }
    pub fn user_id(&self) -> &str {
        &self.user_id
    }
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }
    pub fn roles(&self) -> &[String] {
        &self.roles
    }
    pub fn is_authenticated(&self) -> bool {
        self.is_authenticated
    }
    fn tenant(&self) -> &str {
        if self.tenant_id.is_empty() {
            "default"
        } else {
            &self.tenant_id
        }
    }
}
pub type ProviderAdminAuthFuture<'a> =
    Pin<Box<dyn Future<Output = ApiResult<ProviderAdminUser>> + Send + 'a>>;
pub trait ProviderAdminIdentityResolver: Send + Sync {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap, path: &'a str) -> ProviderAdminAuthFuture<'a>;
}
impl ProviderAdminIdentityResolver for AdmissionService {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap, path: &'a str) -> ProviderAdminAuthFuture<'a> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let principal = request
                .resolve()
                .await
                .map_err(|e| ProviderAdminError::http(e.status(), e.to_string()))?;
            Ok(ProviderAdminUser::from_resolved_identity(
                principal.user_id().into(),
                principal.tenant_id().into(),
                principal.roles().to_vec(),
                true,
            ))
        })
    }
}
fn require(user: &ProviderAdminUser, capability: Capability) -> ApiResult<()> {
    let rbac = Rbac::new(HashMap::from([("admin".into(), vec!["admin:*".into()])]));
    let roles: Vec<String> = user
        .roles
        .iter()
        .map(|role| role.trim_matches(python_whitespace).to_owned())
        .collect();
    if check_capability(&rbac, &roles, &[], capability).allowed() {
        Ok(())
    } else {
        Err(ProviderAdminError::http(
            403,
            permission_denied_detail(capability, "", None),
        ))
    }
}

pub trait ProviderAdminProviders: Send + Sync {
    fn list<'a>(
        &'a self,
        tenant: &'a str,
        include_disabled: bool,
    ) -> ProviderAdminFuture<'a, Vec<Value>>;
    fn get<'a>(&'a self, tenant: &'a str, id: &'a str) -> ProviderAdminFuture<'a, Option<Value>>;
    fn create<'a>(
        &'a self,
        tenant: &'a str,
        body: CreateProvider,
    ) -> ProviderAdminFuture<'a, Value>;
    fn update<'a>(
        &'a self,
        tenant: &'a str,
        id: &'a str,
        body: UpdateProvider,
    ) -> ProviderAdminFuture<'a, Option<Value>>;
    fn delete<'a>(&'a self, tenant: &'a str, id: &'a str) -> ProviderAdminFuture<'a, bool>;
    fn test<'a>(&'a self, tenant: &'a str, id: &'a str) -> ProviderAdminFuture<'a, Value>;
}
impl<
    D: ProviderDatabase,
    C: ProviderCipher,
    H: ProviderHttp,
    G: ProviderGoogleAuth,
    E: ProviderEnvironment,
    T: ProviderClock,
> ProviderAdminProviders for ProviderService<D, C, H, G, E, T>
{
    fn list<'a>(&'a self, tenant: &'a str, disabled: bool) -> ProviderAdminFuture<'a, Vec<Value>> {
        Box::pin(async move {
            self.list_providers(tenant, disabled)
                .await
                .map_err(|e| ProviderAdminBackendError::exception(e.to_string()))
        })
    }
    fn get<'a>(&'a self, tenant: &'a str, id: &'a str) -> ProviderAdminFuture<'a, Option<Value>> {
        Box::pin(async move {
            self.get_provider(tenant, id)
                .await
                .map_err(|e| ProviderAdminBackendError::exception(e.to_string()))
        })
    }
    fn create<'a>(
        &'a self,
        tenant: &'a str,
        body: CreateProvider,
    ) -> ProviderAdminFuture<'a, Value> {
        Box::pin(async move {
            self.create_provider(tenant, body)
                .await
                .map_err(|e| ProviderAdminBackendError::exception(e.to_string()))
        })
    }
    fn update<'a>(
        &'a self,
        tenant: &'a str,
        id: &'a str,
        body: UpdateProvider,
    ) -> ProviderAdminFuture<'a, Option<Value>> {
        Box::pin(async move {
            self.update_provider(tenant, id, body)
                .await
                .map_err(|e| ProviderAdminBackendError::exception(e.to_string()))
        })
    }
    fn delete<'a>(&'a self, tenant: &'a str, id: &'a str) -> ProviderAdminFuture<'a, bool> {
        Box::pin(async move {
            self.delete_provider(tenant, id)
                .await
                .map_err(|e| ProviderAdminBackendError::exception(e.to_string()))
        })
    }
    fn test<'a>(&'a self, tenant: &'a str, id: &'a str) -> ProviderAdminFuture<'a, Value> {
        Box::pin(async move {
            self.test_connection(tenant, id)
                .await
                .map_err(|e| ProviderAdminBackendError::exception(e.to_string()))
        })
    }
}
pub trait ProviderAdminCatalog: Send + Sync {
    fn sync<'a>(&'a self, tenant: &'a str, id: &'a str) -> ProviderAdminFuture<'a, Value>;
}
/// Owns only injected service handles; constructs the existing sync service per
/// call, with discovery enabled as in the canonical endpoint.
pub struct NativeProviderAdminCatalog {
    providers: Arc<dyn CatalogProviders>,
    models: Arc<dyn CatalogModels>,
    http: Arc<dyn CatalogHttp>,
}
impl NativeProviderAdminCatalog {
    pub fn new(
        providers: Arc<dyn CatalogProviders>,
        models: Arc<dyn CatalogModels>,
        http: Arc<dyn CatalogHttp>,
    ) -> Self {
        Self {
            providers,
            models,
            http,
        }
    }
}
impl ProviderAdminCatalog for NativeProviderAdminCatalog {
    fn sync<'a>(&'a self, tenant: &'a str, id: &'a str) -> ProviderAdminFuture<'a, Value> {
        Box::pin(async move {
            ModelCatalogSyncService::new(
                self.providers.as_ref(),
                self.models.as_ref(),
                self.http.as_ref(),
            )
            .sync_provider_models(tenant, id, true)
            .await
            .map_err(|e| match e {
                CatalogSyncError::ProviderNotFound | CatalogSyncError::InvalidIntegerLiteral => {
                    ProviderAdminBackendError::value_error(e.to_string())
                }
                _ => ProviderAdminBackendError::exception(e.to_string()),
            })
        })
    }
}

/// Trusted request context, not parsed from JSON. Trace/client-IP interpretation
/// is left to the same host policy used by the injected config audit writer.
#[derive(Clone, Default)]
pub struct ProviderAdminRequestContext {
    pub headers: HeaderMap,
    pub request_id: Option<Value>,
    pub trace_id: Option<Value>,
    pub client_host: Option<String>,
}
/// Raw snapshots are deliberately private-facing and never implement Debug or
/// Serialize. The ConfigAuditWriter adapter below redacts before persistence.
pub struct ProviderAdminChange {
    pub resource_id: String,
    pub action: &'static str,
    pub before: Option<Value>,
    pub after: Option<Value>,
}
pub trait ProviderAdminEffects: Send + Sync {
    fn invalidate(&self, tenant: &str, provider: &str);
    fn begin_probe(&self, tenant: &str, provider: &str) -> Option<String>;
    fn record_probe(&self, tenant: &str, provider: &str, result: &Value, probe_id: Option<&str>);
    fn audit<'a>(
        &'a self,
        context: &'a ProviderAdminRequestContext,
        user: &'a ProviderAdminUser,
        change: ProviderAdminChange,
    ) -> ProviderAdminFuture<'a, ()>;
    /// Implementations preserve `_publish_model_config_changed`: missing Redis
    /// and ordinary publish exceptions are nonfatal; cancellation still propagates.
    fn publish<'a>(
        &'a self,
        context: &'a ProviderAdminRequestContext,
        tenant: &'a str,
        model: &'a str,
        provider: &'a str,
    ) -> ProviderAdminFuture<'a, ()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderBodySchema {
    Create,
    Update,
    FromTemplate,
}
/// Validated body; no public fields, deserializer or Debug to bypass validation or
/// accidentally log an API key. `model_dump` is explicitly internal-only.
pub struct ProviderAdminBody {
    schema: ProviderBodySchema,
    values: Map<String, Value>,
}
impl ProviderAdminBody {
    pub fn model_dump_internal(&self, exclude_none: bool) -> Value {
        Value::Object(
            self.values
                .iter()
                .filter(|(_, v)| !exclude_none || !v.is_null())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    }
}
#[derive(Clone, Copy)]
enum Kind {
    String { min: usize, max: Option<usize> },
    Bool,
    Object,
}
fn validation_error(
    field: &str,
    input: &Value,
    kind: &str,
    msg: String,
    ctx: Option<Value>,
) -> Value {
    let loc = if field.is_empty() {
        json!(["body"])
    } else {
        json!(["body", field])
    };
    let mut error = json!({"type": kind, "loc":loc, "msg":msg, "input":input});
    if let Some(ctx) = ctx {
        error["ctx"] = ctx;
    }
    error
}
fn bool_value(input: &Value) -> Option<bool> {
    match input {
        Value::Bool(v) => Some(*v),
        Value::Number(v) => match v.as_f64()? {
            0.0 => Some(false),
            1.0 => Some(true),
            _ => None,
        },
        Value::String(v) => match v.to_ascii_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
            "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
            _ => None,
        },
        _ => None,
    }
}
fn field_value(field: &str, input: &Value, kind: Kind, nullable: bool) -> Result<Value, Value> {
    if input.is_null() && nullable {
        return Ok(Value::Null);
    }
    match kind {
        Kind::String { min, max } => {
            let Some(text) = input.as_str() else {
                return Err(validation_error(
                    field,
                    input,
                    "string_type",
                    "Input should be a valid string".into(),
                    None,
                ));
            };
            let length = text.chars().count();
            if length < min {
                return Err(validation_error(
                    field,
                    input,
                    "string_too_short",
                    format!(
                        "String should have at least {min} character{}",
                        if min == 1 { "" } else { "s" }
                    ),
                    Some(json!({"min_length":min})),
                ));
            }
            if let Some(max) = max.filter(|max| length > *max) {
                return Err(validation_error(
                    field,
                    input,
                    "string_too_long",
                    format!("String should have at most {max} characters"),
                    Some(json!({"max_length":max})),
                ));
            }
            Ok(input.clone())
        }
        Kind::Bool => bool_value(input).map(Value::Bool).ok_or_else(|| {
            let parsing = input.is_string()
                || input.is_i64()
                || input.as_f64().is_some_and(|value| {
                    value.fract() == 0.0 && value > i64::MIN as f64 && value < 9223372036854775808.0
                });
            validation_error(
                field,
                input,
                if parsing { "bool_parsing" } else { "bool_type" },
                if parsing {
                    "Input should be a valid boolean, unable to interpret input"
                } else {
                    "Input should be a valid boolean"
                }
                .into(),
                None,
            )
        }),
        Kind::Object => {
            if input.is_object() {
                Ok(input.clone())
            } else {
                Err(validation_error(
                    field,
                    input,
                    "dict_type",
                    "Input should be a valid dictionary".into(),
                    None,
                ))
            }
        }
    }
}
/// Semantic JSON validation derived from ProviderCreate/Update/FromTemplateCreate.
pub fn validate_provider_body(
    schema: ProviderBodySchema,
    input: &Value,
) -> ApiResult<ProviderAdminBody> {
    let Some(object) = input.as_object() else {
        return Err(ProviderAdminError::http(
            422,
            json!([validation_error(
                "",
                input,
                "model_attributes_type",
                "Input should be a valid dictionary or object to extract fields from".into(),
                None
            )]),
        ));
    };
    let string = |min, max| Kind::String { min, max };
    let fields: Vec<(&str, Kind, bool, Option<Value>)> = match schema {
        ProviderBodySchema::Create => vec![
            ("provider_id", string(1, Some(50)), false, None),
            ("display_name", string(1, Some(100)), false, None),
            ("api_type", string(0, None), false, Some(json!("openai"))),
            ("base_url", string(0, Some(500)), true, Some(Value::Null)),
            ("metadata", Kind::Object, false, Some(json!({}))),
            ("is_enabled", Kind::Bool, false, Some(json!(true))),
            ("api_key", string(0, None), true, Some(Value::Null)),
        ],
        ProviderBodySchema::Update => vec![
            (
                "display_name",
                string(1, Some(100)),
                true,
                Some(Value::Null),
            ),
            ("api_type", string(0, None), true, Some(Value::Null)),
            ("base_url", string(0, Some(500)), true, Some(Value::Null)),
            ("metadata", Kind::Object, true, Some(Value::Null)),
            ("api_key", string(0, None), true, Some(Value::Null)),
            ("is_enabled", Kind::Bool, true, Some(Value::Null)),
        ],
        ProviderBodySchema::FromTemplate => vec![
            ("template_id", string(1, None), false, None),
            ("provider_id", string(1, Some(50)), true, Some(Value::Null)),
            (
                "display_name",
                string(1, Some(100)),
                true,
                Some(Value::Null),
            ),
            ("base_url", string(0, Some(500)), true, Some(Value::Null)),
            ("metadata", Kind::Object, true, Some(Value::Null)),
            ("api_key", string(0, None), true, Some(Value::Null)),
            ("is_enabled", Kind::Bool, false, Some(json!(true))),
        ],
    };
    let mut values = Map::new();
    let mut errors = Vec::new();
    for (name, kind, nullable, default) in fields {
        match object.get(name) {
            Some(input) => match field_value(name, input, kind, nullable) {
                Ok(value) => {
                    values.insert(name.into(), value);
                }
                Err(error) => errors.push(error),
            },
            None => {
                if let Some(value) = default {
                    values.insert(name.into(), value);
                } else {
                    errors.push(validation_error(
                        name,
                        input,
                        "missing",
                        "Field required".into(),
                        None,
                    ));
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(ProviderAdminBody { schema, values })
    } else {
        Err(ProviderAdminError::http(422, json!(errors)))
    }
}
pub fn validate_include_disabled(input: Option<&str>) -> ApiResult<bool> {
    let Some(input) = input else {
        return Ok(false);
    };
    bool_value(&json!(input)).ok_or_else(|| ProviderAdminError::http(422, json!([{"type":"bool_parsing","loc":["query","include_disabled"],"msg":"Input should be a valid boolean, unable to interpret input","input":input}])))
}
fn safe_metadata(input: Option<&Value>) -> ApiResult<Option<Map<String, Value>>> {
    let Some(input) = input.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    if has_secret_field(input) {
        return Err(ProviderAdminError::http(
            422,
            "PROVIDER_METADATA_MUST_NOT_CONTAIN_SECRETS",
        ));
    }
    let object = input.as_object().ok_or(ProviderAdminError::Internal)?;
    Ok(Some(
        object
            .iter()
            .filter(|(k, v)| {
                !k.trim_matches(python_whitespace).is_empty() && !v.is_null() && **v != json!("")
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    ))
}
fn optional_string(values: &Map<String, Value>, key: &str) -> Option<String> {
    values.get(key).and_then(Value::as_str).map(str::to_owned)
}
fn make_create(values: &Map<String, Value>) -> ApiResult<CreateProvider> {
    Ok(CreateProvider {
        provider_id: values["provider_id"]
            .as_str()
            .ok_or(ProviderAdminError::Internal)?
            .into(),
        display_name: values["display_name"]
            .as_str()
            .ok_or(ProviderAdminError::Internal)?
            .into(),
        api_type: values["api_type"]
            .as_str()
            .ok_or(ProviderAdminError::Internal)?
            .into(),
        base_url: optional_string(values, "base_url"),
        api_key: optional_string(values, "api_key"),
        metadata: safe_metadata(values.get("metadata"))?,
        is_enabled: values["is_enabled"]
            .as_bool()
            .ok_or(ProviderAdminError::Internal)?,
    })
}
fn make_update(values: &Map<String, Value>) -> ApiResult<UpdateProvider> {
    Ok(UpdateProvider {
        display_name: optional_string(values, "display_name"),
        api_type: optional_string(values, "api_type"),
        base_url: optional_string(values, "base_url"),
        api_key: optional_string(values, "api_key"),
        metadata: safe_metadata(values.get("metadata"))?,
        is_enabled: values.get("is_enabled").and_then(Value::as_bool),
    })
}
fn backend_error(error: ProviderAdminBackendError) -> ProviderAdminError {
    if error.cancelled {
        ProviderAdminError::Cancelled
    } else {
        ProviderAdminError::Internal
    }
}
fn create_error(error: ProviderAdminBackendError) -> ProviderAdminError {
    if error.cancelled {
        return ProviderAdminError::Cancelled;
    }
    if error.message.to_lowercase().contains("duplicate key") {
        ProviderAdminError::http(409, "Provider already exists")
    } else {
        ProviderAdminError::http(500, error.message)
    }
}
fn create_validation_error(error: ProviderAdminError) -> ProviderAdminError {
    match error {
        ProviderAdminError::Http {
            status,
            detail: Value::String(detail),
        } => create_error(ProviderAdminBackendError::exception(format!(
            "{status}: {detail}"
        ))),
        _ => ProviderAdminError::Internal,
    }
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|n| n != 0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}

/// Explicit response-model projection. No API keys, encrypted credentials,
/// runtime credentials or arbitrary service-only keys reach the wire.
pub fn project_provider(value: &Value) -> ApiResult<Value> {
    let mut public_input = value.clone();
    if let Some(object) = public_input.as_object_mut() {
        object.remove("api_key");
    }
    let mut base = validate_provider_body(ProviderBodySchema::Create, &public_input)
        .map_err(|_| ProviderAdminError::Internal)?
        .values;
    base.remove("api_key");
    for (field, kind, default) in [
        ("tenant_id", Kind::String { min: 0, max: None }, None),
        ("has_api_key", Kind::Bool, None),
        (
            "allow_environment_credentials",
            Kind::Bool,
            Some(json!(false)),
        ),
    ] {
        let raw = value
            .get(field)
            .cloned()
            .or(default)
            .ok_or(ProviderAdminError::Internal)?;
        base.insert(
            field.into(),
            field_value(field, &raw, kind, false).map_err(|_| ProviderAdminError::Internal)?,
        );
    }
    for field in ["created_at", "updated_at"] {
        let raw = value
            .get(field)
            .and_then(Value::as_str)
            .ok_or(ProviderAdminError::Internal)?;
        let timestamp =
            ProtocolTimestamp::parse_isoformat(raw).map_err(|_| ProviderAdminError::Internal)?;
        use chrono::{Datelike, Timelike};
        let (local, offset) = match timestamp {
            ProtocolTimestamp::Naive(value) => (value, 0),
            ProtocolTimestamp::Offset(value) => {
                (value.naive_local(), value.offset().local_minus_utc())
            }
        };
        if !(1..=9999).contains(&local.year())
            || local.nanosecond() >= 1_000_000_000
            || offset % 60 != 0
        {
            return Err(ProviderAdminError::Internal);
        }
        let timestamp = timestamp.to_isoformat();
        let timestamp = timestamp
            .strip_suffix("+00:00")
            .map_or_else(|| timestamp.clone(), |raw| format!("{raw}Z"));
        base.insert(field.into(), json!(timestamp));
    }
    Ok(Value::Object(base))
}
fn project_templates() -> Value {
    json!(
        list_provider_templates()
            .iter()
            .map(|template| {
                let mut result = template.to_response();
                if let Some(models) = result["default_models"].as_array_mut() {
                    for model in models {
                        if let Some(model) = model.as_object_mut() {
                            model.remove("catalog_capabilities");
                        }
                    }
                }
                result
            })
            .collect::<Vec<_>>()
    )
}
pub fn project_sync(value: &Value) -> ApiResult<Value> {
    let object = value.as_object().ok_or(ProviderAdminError::Internal)?;
    let mut result = Map::new();
    for name in ["provider_id", "template_id"] {
        let raw = object
            .get(name)
            .cloned()
            .or_else(|| (name == "template_id").then_some(Value::Null))
            .ok_or(ProviderAdminError::Internal)?;
        if !raw.is_string() && !(name == "template_id" && raw.is_null()) {
            return Err(ProviderAdminError::Internal);
        }
        result.insert(name.into(), raw);
    }
    for name in ["created_models", "updated_models", "skipped_models"] {
        let rows = object
            .get(name)
            .and_then(Value::as_array)
            .ok_or(ProviderAdminError::Internal)?;
        let mut projected = Vec::new();
        for row in rows {
            let fields: &[&str] = if name == "skipped_models" {
                &["model_id", "reason"]
            } else {
                &["model_id", "provider_id", "display_name", "is_enabled"]
            };
            let mut map = Map::new();
            for field in fields {
                let input = row.get(field).ok_or(ProviderAdminError::Internal)?;
                let kind = if *field == "is_enabled" {
                    Kind::Bool
                } else {
                    Kind::String { min: 0, max: None }
                };
                map.insert(
                    (*field).into(),
                    field_value(field, input, kind, false)
                        .map_err(|_| ProviderAdminError::Internal)?,
                );
            }
            projected.push(Value::Object(map));
        }
        result.insert(name.into(), json!(projected));
    }
    let warnings = object
        .get("discovery_warnings")
        .and_then(Value::as_array)
        .ok_or(ProviderAdminError::Internal)?;
    if !warnings.iter().all(Value::is_string) {
        return Err(ProviderAdminError::Internal);
    }
    result.insert("discovery_warnings".into(), json!(warnings));
    Ok(Value::Object(result))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderAdminOperation {
    Templates,
    List,
    Create,
    FromTemplate,
    Get,
    Update,
    Delete,
    Test,
    Sync,
}
/// Typed opt-in handler core, also usable by a host with its own HTTP transport.
/// Missing dependencies preserve 503 instead of silently omitting routes.
mod catalog_review_routes;
pub struct ProviderAdminApi {
    review: Option<Arc<crate::model_catalog_review::CatalogReview>>,
    providers: Option<Arc<dyn ProviderAdminProviders>>,
    catalog: Option<Arc<dyn ProviderAdminCatalog>>,
    effects: Arc<dyn ProviderAdminEffects>,
}
impl ProviderAdminApi {
    pub fn new(
        providers: Option<Arc<dyn ProviderAdminProviders>>,
        catalog: Option<Arc<dyn ProviderAdminCatalog>>,
        effects: Arc<dyn ProviderAdminEffects>,
    ) -> Self {
        Self {
            providers,
            catalog,
            effects,
            review: None,
        }
    }
    pub fn with_review(mut self, review: Arc<crate::model_catalog_review::CatalogReview>) -> Self { self.review=Some(review); self }
    pub fn check_dependencies(&self, operation: ProviderAdminOperation) -> ApiResult<()> {
        if operation != ProviderAdminOperation::Templates && self.providers.is_none() {
            return Err(ProviderAdminError::http(
                503,
                "Provider service is not initialized.",
            ));
        }
        if operation == ProviderAdminOperation::Sync && self.catalog.is_none() {
            return Err(ProviderAdminError::http(
                503,
                "Model service is not initialized.",
            ));
        }
        Ok(())
    }
    /// Bodies must already have passed the matching canonical schema. A body of
    /// another schema is rejected; it cannot bypass required-field validation.
    pub async fn execute(
        &self,
        operation: ProviderAdminOperation,
        user: &ProviderAdminUser,
        provider_id: &str,
        body: Option<ProviderAdminBody>,
        include_disabled: bool,
        request: Option<&ProviderAdminRequestContext>,
    ) -> ApiResult<Value> {
        self.check_dependencies(operation)?;
        let capability = if matches!(
            operation,
            ProviderAdminOperation::Templates
                | ProviderAdminOperation::List
                | ProviderAdminOperation::Get
        ) {
            Capability::GatewayProviderConfigRead
        } else {
            Capability::GatewayProviderConfigWrite
        };
        require(user, capability)?;
        if operation == ProviderAdminOperation::Templates {
            return Ok(project_templates());
        }
        let providers = self
            .providers
            .as_ref()
            .ok_or(ProviderAdminError::Internal)?;
        let tenant = user.tenant();
        match operation {
            ProviderAdminOperation::Templates => unreachable!(),
            ProviderAdminOperation::List => providers
                .list(
                    tenant,
                    include_disabled && user.roles.iter().any(|role| role == "admin"),
                )
                .await
                .map_err(backend_error)?
                .iter()
                .map(project_provider)
                .collect::<ApiResult<Vec<_>>>()
                .map(|v| json!(v)),
            ProviderAdminOperation::Get => {
                let value = providers
                    .get(tenant, provider_id)
                    .await
                    .map_err(backend_error)?
                    .filter(truthy)
                    .ok_or_else(|| ProviderAdminError::http(404, "Provider not found"))?;
                project_provider(&value)
            }
            ProviderAdminOperation::Create => {
                let body = matching_body(body, ProviderBodySchema::Create)?;
                let input = make_create(&body.values).map_err(create_validation_error)?;
                let id = input.provider_id.clone();
                let result = providers
                    .create(tenant, input)
                    .await
                    .map_err(create_error)?;
                if let Some(request) = request {
                    self.effects.invalidate(tenant, &id);
                    self.effects
                        .audit(
                            request,
                            user,
                            ProviderAdminChange {
                                resource_id: id,
                                action: "create",
                                before: None,
                                after: Some(body.model_dump_internal(false)),
                            },
                        )
                        .await
                        .map_err(create_error)?;
                }
                project_provider(&result)
            }
            ProviderAdminOperation::FromTemplate => {
                let body = matching_body(body, ProviderBodySchema::FromTemplate)?;
                let template_id = body.values["template_id"]
                    .as_str()
                    .ok_or(ProviderAdminError::Internal)?;
                let template = get_provider_template(template_id)
                    .ok_or_else(|| ProviderAdminError::http(404, "Provider template not found"))?;
                let resolve = |field: &str, fallback: &str| {
                    body.values
                        .get(field)
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .unwrap_or(fallback)
                        .trim_matches(python_whitespace)
                        .to_owned()
                };
                let id = resolve("provider_id", &template.default_provider_id);
                let display = resolve("display_name", &template.display_name);
                let url = resolve("base_url", &template.default_base_url);
                if template.advanced {
                    if id.is_empty() || display.is_empty() || url.is_empty() {
                        return Err(ProviderAdminError::http(
                            422,
                            "ADVANCED_TEMPLATE_REQUIRES_PROVIDER_ID_DISPLAY_NAME_BASE_URL",
                        ));
                    }
                } else if body.values.get("provider_id").is_some_and(truthy)
                    && body.values["provider_id"] != json!(template.default_provider_id)
                {
                    return Err(ProviderAdminError::http(
                        422,
                        "TEMPLATE_PROVIDER_ID_IS_FIXED_FOR_MAINSTREAM_PROVIDERS",
                    ));
                }
                let mut metadata: Map<String, Value> =
                    template.default_metadata.iter().cloned().collect();
                metadata.extend(safe_metadata(body.values.get("metadata"))?.unwrap_or_default());
                let existing = providers.get(tenant, &id).await.map_err(backend_error)?;
                let before = Some(json!({"template_id":template_id}));
                let after = Some(body.model_dump_internal(false));
                let base_url = (!url.is_empty()).then_some(url);
                let api_key = optional_string(&body.values, "api_key");
                let enabled = body.values["is_enabled"]
                    .as_bool()
                    .ok_or(ProviderAdminError::Internal)?;
                let result = if existing.as_ref().is_some_and(truthy) {
                    let result = providers
                        .update(
                            tenant,
                            &id,
                            UpdateProvider {
                                display_name: Some(display),
                                api_type: Some(template.api_type.clone()),
                                base_url,
                                api_key,
                                metadata: Some(metadata),
                                is_enabled: Some(enabled),
                            },
                        )
                        .await
                        .map_err(backend_error)?
                        .filter(truthy)
                        .ok_or_else(|| ProviderAdminError::http(404, "Provider not found"))?;
                    if let Some(request) = request {
                        self.effects
                            .audit(
                                request,
                                user,
                                ProviderAdminChange {
                                    resource_id: id.clone(),
                                    action: "update_from_template",
                                    before,
                                    after,
                                },
                            )
                            .await
                            .map_err(backend_error)?;
                    }
                    result
                } else {
                    let result = providers
                        .create(
                            tenant,
                            CreateProvider {
                                provider_id: id.clone(),
                                display_name: display,
                                api_type: template.api_type.clone(),
                                base_url,
                                api_key,
                                metadata: Some(metadata),
                                is_enabled: enabled,
                            },
                        )
                        .await
                        .map_err(create_error)?;
                    if let Some(request) = request {
                        self.effects.invalidate(tenant, &id);
                        self.effects
                            .audit(
                                request,
                                user,
                                ProviderAdminChange {
                                    resource_id: id.clone(),
                                    action: "create_from_template",
                                    before,
                                    after,
                                },
                            )
                            .await
                            .map_err(create_error)?;
                    }
                    result
                };
                project_provider(&result)
            }
            ProviderAdminOperation::Update => {
                let body = matching_body(body, ProviderBodySchema::Update)?;
                let result = providers
                    .update(tenant, provider_id, make_update(&body.values)?)
                    .await
                    .map_err(backend_error)?
                    .filter(truthy)
                    .ok_or_else(|| ProviderAdminError::http(404, "Provider not found"))?;
                if let Some(request) = request {
                    self.effects.invalidate(tenant, provider_id);
                    self.effects
                        .audit(
                            request,
                            user,
                            ProviderAdminChange {
                                resource_id: provider_id.into(),
                                action: "update",
                                before: None,
                                after: Some(body.model_dump_internal(true)),
                            },
                        )
                        .await
                        .map_err(backend_error)?;
                }
                project_provider(&result)
            }
            ProviderAdminOperation::Delete => {
                if !providers
                    .delete(tenant, provider_id)
                    .await
                    .map_err(backend_error)?
                {
                    return Err(ProviderAdminError::http(404, "Provider not found"));
                }
                if let Some(request) = request {
                    self.effects.invalidate(tenant, provider_id);
                    self.effects
                        .audit(
                            request,
                            user,
                            ProviderAdminChange {
                                resource_id: provider_id.into(),
                                action: "delete",
                                before: Some(json!({"provider_id":provider_id})),
                                after: None,
                            },
                        )
                        .await
                        .map_err(backend_error)?;
                }
                Ok(json!({"provider_id":provider_id,"status":"deleted"}))
            }
            ProviderAdminOperation::Test => {
                let probe = request.and_then(|_| self.effects.begin_probe(tenant, provider_id));
                let result = providers
                    .test(tenant, provider_id)
                    .await
                    .map_err(backend_error)?;
                if request.is_some() {
                    self.effects
                        .record_probe(tenant, provider_id, &result, probe.as_deref());
                }
                Ok(result)
            }
            ProviderAdminOperation::Sync => {
                let result = self
                    .catalog
                    .as_ref()
                    .ok_or(ProviderAdminError::Internal)?
                    .sync(tenant, provider_id)
                    .await
                    .map_err(|error| {
                        if error.cancelled {
                            ProviderAdminError::Cancelled
                        } else if error.value_error {
                            if error.message == "PROVIDER_NOT_FOUND" {
                                ProviderAdminError::http(404, "Provider not found")
                            } else {
                                ProviderAdminError::http(400, error.message)
                            }
                        } else {
                            ProviderAdminError::Internal
                        }
                    })?;
                if let Some(changed) = result.get("capability_changed_models") {
                    let changed = changed.as_array().ok_or(ProviderAdminError::Internal)?;
                    for model in changed {
                        let model = model.as_object().ok_or(ProviderAdminError::Internal)?;
                        let id = model
                            .get("model_id")
                            .filter(|v| truthy(v))
                            .map(python_value_to_string)
                            .unwrap_or_default();
                        let provider = model
                            .get("provider_id")
                            .filter(|v| truthy(v))
                            .map(python_value_to_string)
                            .unwrap_or_else(|| provider_id.into());
                        if let Some(request) = request
                            && let Err(error) =
                                self.effects.publish(request, tenant, &id, &provider).await
                            && error.cancelled
                        {
                            return Err(ProviderAdminError::Cancelled);
                        }
                    }
                }
                project_sync(&result)
            }
        }
    }
}
fn matching_body(
    body: Option<ProviderAdminBody>,
    schema: ProviderBodySchema,
) -> ApiResult<ProviderAdminBody> {
    body.filter(|body| body.schema == schema).ok_or_else(|| {
        ProviderAdminError::http(
            422,
            json!([{"type":"missing","loc":["body"],"msg":"Field required","input":null}]),
        )
    })
}

pub trait ProviderAdminCancellation: Send + Sync {
    /// The host owns connection termination/cancellation semantics. This hook is
    /// mandatory: the factory invents no HTTP status for Python CancelledError.
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = Response> + Send + '_>>;
}
#[derive(Clone)]
struct HttpState {
    cancellation: Arc<dyn ProviderAdminCancellation>,
    api: Arc<ProviderAdminApi>,
    identity: Arc<dyn ProviderAdminIdentityResolver>,
}
/// Explicit opt-in only. No listener, default gateway mount or implicit services.
pub fn router(
    identity: Arc<dyn ProviderAdminIdentityResolver>,
    api: Arc<ProviderAdminApi>,
    cancellation: Arc<dyn ProviderAdminCancellation>,
) -> Router {
    Router::new()
        .merge(catalog_review_routes::routes())
        .route(
            "/api/v1/provider-templates",
            any(http_method_fallback).get(http_templates),
        )
        .route(
            "/api/v1/providers",
            any(http_method_fallback).get(http_list).post(http_create),
        )
        .route(
            "/api/v1/providers/from-template",
            any(http_method_fallback)
                .post(http_from_template)
                .get(http_reserved_get)
                .put(http_reserved_update)
                .delete(http_reserved_delete),
        )
        .route(
            "/api/v1/providers/{provider_id}",
            any(http_method_fallback)
                .get(http_get)
                .put(http_update)
                .delete(http_delete),
        )
        .route(
            "/api/v1/providers/{provider_id}/test",
            any(http_method_fallback).post(http_test),
        )
        .route(
            "/api/v1/providers/{provider_id}/models/sync",
            any(http_method_fallback).post(http_sync),
        )
        .route_layer(middleware::from_fn(http_route_guard))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState {
            api,
            identity,
            cancellation,
        })
}
// `any` suppresses Axum's automatic Allow union after a fallback response.
// The route guard supplies source Allow only for true 405s, never a route 404.
async fn http_method_fallback() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}
async fn http_route_guard(
    matched: MatchedPath,
    params: Result<Path<HashMap<String, String>>, axum::extract::rejection::PathRejection>,
    request: Request,
    next: Next,
) -> Response {
    let params = match params {
        Ok(Path(params)) => params,
        Err(error) => return error.into_response(),
    };
    let invalid = params.get("provider_id").is_some_and(|id| id.contains('/'));
    let allowed: &[&str] = match matched.as_str() {
        "/api/v1/providers/{provider_id}/models/sync/reviews/{review_id}" => &["GET"],
        "/api/v1/provider-templates" => &["GET"],
        "/api/v1/providers" => &["GET", "POST"],
        "/api/v1/providers/{provider_id}" => &["GET", "PUT", "DELETE"],
        // FastAPI continues past the static POST for a full dynamic method match.
        "/api/v1/providers/from-template" => &["POST", "GET", "PUT", "DELETE"],
        _ => &["POST"],
    };
    if !invalid && allowed.contains(&request.method().as_str()) {
        return next.run(request).await;
    }
    // Axum sets Content-Length before suppressing the HEAD representation.
    if invalid {
        (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response()
    } else {
        (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", allowed[0])],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response()
    }
}
async fn dispatch(
    state: HttpState,
    operation: ProviderAdminOperation,
    id: String,
    request: Request,
) -> Response {
    let headers = request.headers().clone();
    let mut context = request
        .extensions()
        .get::<ProviderAdminRequestContext>()
        .cloned()
        .unwrap_or_else(|| ProviderAdminRequestContext {
            headers: headers.clone(),
            ..Default::default()
        });
    context.headers = headers.clone();
    let trace = if let Some(trace) = &context.trace_id {
        trace.as_str().map(str::to_owned)
    } else {
        headers
            .get("x-trace-id")
            .filter(|value| !value.is_empty())
            .or_else(|| headers.get("x-request-id"))
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let result = async {
        let schema = match operation {
            ProviderAdminOperation::Create => Some(ProviderBodySchema::Create),
            ProviderAdminOperation::Update => Some(ProviderBodySchema::Update),
            ProviderAdminOperation::FromTemplate => Some(ProviderBodySchema::FromTemplate),
            _ => None,
        };
        let uri = request.uri().clone();
        let raw_body = if schema.is_some() {
            let bytes = Bytes::from_request(request, &())
                .await
                .map_err(|_| ProviderAdminError::http(413, "Request body too large"))?;
            Some(crate::model_admin_api::http_body(&headers, bytes).map_err(
                |error| match error {
                    crate::model_admin_api::ModelAdminError::Http { status, body } => {
                        ProviderAdminError::http(status, body["detail"].clone())
                    }
                    crate::model_admin_api::ModelAdminError::Cancelled => {
                        ProviderAdminError::Cancelled
                    }
                },
            )?)
        } else {
            None
        };
        state.api.check_dependencies(operation)?;
        let user = state.identity.resolve(&headers, uri.path()).await?;
        let include_disabled = if operation == ProviderAdminOperation::List {
            let query = Query::<HashMap<String, String>>::try_from_uri(&uri)
                .map_err(|_| ProviderAdminError::http(422, "Invalid query parameters"))?;
            validate_include_disabled(query.get("include_disabled").map(String::as_str))?
        } else {
            false
        };
        let body = if let Some(schema) = schema {
            let value = raw_body.as_ref().expect("body-bearing operation");
            if value.is_null() {
                return Err(ProviderAdminError::http(
                    422,
                    json!([
                        {"type":"missing","loc":["body"],"msg":"Field required","input":null}
                    ]),
                ));
            }
            Some(validate_provider_body(schema, value)?)
        } else {
            None
        };
        state
            .api
            .execute(
                operation,
                &user,
                &id,
                body,
                include_disabled,
                Some(&context),
            )
            .await
    }
    .await;
    // Legacy domain helpers retain their source-derived broad catch shape, but
    // raw backend diagnostics must never cross the HTTP boundary.
    let result = result.map_err(|error| match error {
        ProviderAdminError::Http { status: 500, .. } => ProviderAdminError::Internal,
        error => error,
    });
    match result {
        Ok(value) => Json(value).into_response(),
        Err(ProviderAdminError::Cancelled) => state.cancellation.cancelled().await,
        Err(error) => (
            StatusCode::from_u16(error.status().expect("non-cancellation status"))
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(
                error
                    .response_body(trace.as_deref())
                    .expect("non-cancellation response"),
            ),
        )
            .into_response(),
    }
}
macro_rules! no_path_handler {
    ($name:ident,$op:ident) => {
        async fn $name(State(state): State<HttpState>, request: Request) -> Response {
            dispatch(state, ProviderAdminOperation::$op, String::new(), request).await
        }
    };
}
macro_rules! path_handler {
    ($name:ident,$op:ident) => {
        async fn $name(
            State(state): State<HttpState>,
            Path(id): Path<String>,
            request: Request,
        ) -> Response {
            dispatch(state, ProviderAdminOperation::$op, id, request).await
        }
    };
}
macro_rules! reserved_path_handler {
    ($name:ident,$op:ident) => {
        async fn $name(State(state): State<HttpState>, request: Request) -> Response {
            dispatch(
                state,
                ProviderAdminOperation::$op,
                "from-template".into(),
                request,
            )
            .await
        }
    };
}
reserved_path_handler!(http_reserved_get, Get);
reserved_path_handler!(http_reserved_update, Update);
reserved_path_handler!(http_reserved_delete, Delete);
no_path_handler!(http_templates, Templates);
no_path_handler!(http_list, List);
no_path_handler!(http_create, Create);
no_path_handler!(http_from_template, FromTemplate);
path_handler!(http_get, Get);
path_handler!(http_update, Update);
path_handler!(http_delete, Delete);
path_handler!(http_test, Test);
path_handler!(http_sync, Sync);

/// Native probe storage adapter. One instance must be shared across the app, as
/// in Python app.state; locks are never held across network/audit awaits.
pub struct ProviderAdminProbeState {
    store: Mutex<ProviderProbeStore>,
    runtime: Arc<dyn ProviderProbeRuntime + Send + Sync>,
}
impl ProviderAdminProbeState {
    pub fn new(
        store: ProviderProbeStore,
        runtime: Arc<dyn ProviderProbeRuntime + Send + Sync>,
    ) -> Self {
        Self {
            store: Mutex::new(store),
            runtime,
        }
    }
    /// Read the same application-scoped receipts used by provider administration.
    pub fn status(
        &self,
        tenant: &str,
        provider: &str,
        configured: bool,
    ) -> Result<provider_health::ProviderProbeStatus, provider_health::ProviderProbeError> {
        provider_health::provider_probe_status(
            Some(&self.store.lock().unwrap_or_else(|e| e.into_inner())),
            tenant,
            provider,
            configured,
            None,
            self.runtime.as_ref(),
        )
    }
    pub fn invalidate(&self, tenant: &str, provider: &str) {
        provider_health::invalidate_provider_probe(
            Some(&mut self.store.lock().unwrap_or_else(|e| e.into_inner())),
            tenant,
            provider,
        );
    }
    pub fn begin(&self, tenant: &str, provider: &str) -> Option<String> {
        provider_health::begin_provider_probe(
            Some(&mut self.store.lock().unwrap_or_else(|e| e.into_inner())),
            tenant,
            provider,
            self.runtime.as_ref(),
        )
    }
    pub fn record(&self, tenant: &str, provider: &str, result: &Value, id: Option<&str>) {
        if let Some(result) = result.as_object() {
            provider_health::record_provider_probe(
                Some(&mut self.store.lock().unwrap_or_else(|e| e.into_inner())),
                tenant,
                provider,
                result,
                id,
                None,
                self.runtime.as_ref(),
            );
        }
    }
}

impl crate::config_audit_writer::ConfigAuditIdentity for ProviderAdminUser {
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn tenant_id(&self) -> &str {
        &self.tenant_id
    }
}
pub trait ProviderAdminPublisher: Send + Sync {
    fn publish<'a>(&'a self, channel: &'a str, payload: &'a str) -> ProviderAdminFuture<'a, ()>;
}
/// Concrete orchestration bridge using the existing audit writer/probe store.
/// Redis itself remains injected; an absent publisher is the source no-op.
pub struct NativeProviderAdminEffects {
    probes: Arc<ProviderAdminProbeState>,
    audit: Arc<crate::config_audit_writer::ConfigAuditWriter>,
    publisher: Option<Arc<dyn ProviderAdminPublisher>>,
}
impl NativeProviderAdminEffects {
    pub fn new(
        probes: Arc<ProviderAdminProbeState>,
        audit: Arc<crate::config_audit_writer::ConfigAuditWriter>,
        publisher: Option<Arc<dyn ProviderAdminPublisher>>,
    ) -> Self {
        Self {
            probes,
            audit,
            publisher,
        }
    }
}
impl ProviderAdminEffects for NativeProviderAdminEffects {
    fn invalidate(&self, tenant: &str, provider: &str) {
        self.probes.invalidate(tenant, provider);
    }
    fn begin_probe(&self, tenant: &str, provider: &str) -> Option<String> {
        self.probes.begin(tenant, provider)
    }
    fn record_probe(&self, tenant: &str, provider: &str, result: &Value, probe_id: Option<&str>) {
        self.probes.record(tenant, provider, result, probe_id);
    }
    fn audit<'a>(
        &'a self,
        context: &'a ProviderAdminRequestContext,
        user: &'a ProviderAdminUser,
        change: ProviderAdminChange,
    ) -> ProviderAdminFuture<'a, ()> {
        Box::pin(async move {
            use crate::config_audit_writer::{
                ConfigAuditRequestContext, ConfigAuditValue, ConfigChange,
            };
            let request = ConfigAuditRequestContext {
                request_id: context.request_id.clone(),
                trace_id: context.trace_id.clone(),
                headers: context.headers.clone(),
                client_host: context.client_host.clone(),
            };
            let before = change.before.map(ConfigAuditValue::from);
            let after = change.after.map(ConfigAuditValue::from);
            self.audit
                .record_config_change(
                    &request,
                    user,
                    ConfigChange {
                        resource_type: "provider",
                        resource_id: &change.resource_id,
                        action: change.action,
                        before: before.as_ref(),
                        after: after.as_ref(),
                    },
                )
                .await
                .map_err(|e| {
                    if e == crate::config_audit_writer::ConfigAuditError::Cancelled {
                        ProviderAdminBackendError::cancelled()
                    } else {
                        ProviderAdminBackendError::exception(e.to_string())
                    }
                })
        })
    }
    fn publish<'a>(
        &'a self,
        _context: &'a ProviderAdminRequestContext,
        tenant: &'a str,
        model: &'a str,
        provider: &'a str,
    ) -> ProviderAdminFuture<'a, ()> {
        Box::pin(async move {
            if let Some(publisher) = &self.publisher {
                // Source json.dumps preserves this field order; encode each
                // string independently instead of serde_json Map's sorted order.
                let payload = format!(
                    "{{\"tenant_id\":{},\"model_id\":{},\"provider_id\":{}}}",
                    ascii_json_string(tenant),
                    ascii_json_string(model),
                    ascii_json_string(provider)
                );
                if let Err(error) = publisher
                    .publish("gateway:model-config:changed:v1", &payload)
                    .await
                    && error.cancelled
                {
                    return Err(error);
                }
            }
            Ok(())
        })
    }
}
fn ascii_json_string(input: &str) -> String {
    let json = serde_json::to_string(input).expect("string serialization");
    let mut output = String::new();
    for ch in json.chars() {
        if ch < '\u{7f}' {
            output.push(ch);
        } else {
            let mut buf = [0; 2];
            for part in ch.encode_utf16(&mut buf) {
                output.push_str(&format!("\\u{part:04x}"));
            }
        }
    }
    output
}

/// Shared request handler for a host-owned HTTP dispatcher. The router above
/// uses exactly the same dispatch path. Selecting an operation cannot bypass
/// resolution, service availability, validation, capability or tenant checks.
pub async fn handle_http(
    identity: Arc<dyn ProviderAdminIdentityResolver>,
    api: Arc<ProviderAdminApi>,
    cancellation: Arc<dyn ProviderAdminCancellation>,
    operation: ProviderAdminOperation,
    provider_id: String,
    request: Request,
) -> Response {
    dispatch(
        HttpState {
            identity,
            api,
            cancellation,
        },
        operation,
        provider_id,
        request,
    )
    .await
}
