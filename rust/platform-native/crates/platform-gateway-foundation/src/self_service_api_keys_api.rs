//! Five opt-in self-service routes; construction never mounts a host router.
//!
//! Source database dependency precedes authenticated UserContext resolution;
//! syntactic JSON parsing precedes both, request model/query validation follows
//! both. Exact `admin` bypasses ownership only; tenant scope is always enforced.
//! There is no role/permission/tier/body-identity grant at this endpoint. Issuance
//! is limited to the two knowledge scopes, including when the caller is admin.
//!
//! Reuses trusted AdmissionService, shared error rendering, request context, bool /
//! integer conversion, protocol timestamps and global access-cache invalidation.
//! Hosts may inject their richer UserContext resolver for guest/disabled-auth paths.
//! No default anonymous resolver, response-on-cancel, or entropy fallback is supplied.
//! Database writes committed before a later cache/response failure remain committed.
//!
//! JSON finite/i64 domain; Unicode string lengths count scalar values. Timestamp
//! parsing uses the established ISO subset plus tested numeric seconds/milliseconds.
//! Integral numeric strings are exact; extreme decimal strings around 1e14 can
//! differ from speedate conversion/precision and are outside verified parity.
//! Invalid datetime wording, malformed-JSON positions, invalid UTF-8, arbitrary
//! Python objects/integers and trusted-origin slash redirects are explicit boundaries.
use crate::{
    config_audit_writer::ConfigAuditRequestContext,
    protocol::ProtocolTimestamp,
    rate_config_api::{boolean, integer},
    self_service_api_keys::{
        CreateKey, DEFAULT_SCOPES, KeyFilter, KeyFuture, SelfServiceApiKeyError,
        SelfServiceApiKeyService,
    },
    service_access_resolver::clear_service_access_constraint_cache,
    service_config_api::ServiceConfigCancellation,
    usage_parser::python_value_to_string,
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
use chrono::{DateTime, Datelike, Timelike, Utc};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

pub use crate::native_contract::api_keys::SELF_SERVICE_API_KEY_ROUTES;
use crate::native_contract::api_keys::{BASE, KEY, REVOKE, NAME_MAX, DESCRIPTION_MAX};
/// Only construct from authenticated authority. No Deserialize implementation.
#[derive(Clone)]
pub struct SelfServiceKeyIdentity {
    pub user_id: String,
    pub tenant_id: String,
    pub roles: Vec<String>,
    pub tier: String,
    pub is_authenticated: bool,
}
pub trait SelfServiceKeyIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> KeyFuture<'a, SelfServiceKeyIdentity>;
    fn context(&self, headers: &HeaderMap) -> ConfigAuditRequestContext {
        ConfigAuditRequestContext {
            headers: headers.clone(),
            ..Default::default()
        }
    }
}
impl SelfServiceKeyIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> KeyFuture<'a, SelfServiceKeyIdentity> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let user = request
                .resolve()
                .await
                .map_err(|e| SelfServiceApiKeyError::detail(e.status(), e.to_string()))?;
            Ok(SelfServiceKeyIdentity {
                user_id: user.user_id().into(),
                tenant_id: user.tenant_id().into(),
                roles: user.roles().to_vec(),
                tier: user.tier().into(),
                is_authenticated: true,
            })
        })
    }
}
pub trait SelfServiceKeyInvalidation: Send + Sync {
    fn clear(&self) -> Result<(), SelfServiceApiKeyError>;
}
/// Same process-wide cache as the existing native service access resolver.
pub struct GlobalSelfServiceKeyInvalidation;
impl SelfServiceKeyInvalidation for GlobalSelfServiceKeyInvalidation {
    fn clear(&self) -> Result<(), SelfServiceApiKeyError> {
        clear_service_access_constraint_cache();
        Ok(())
    }
}
pub struct SelfServiceKeyDependencies {
    pub identity: Arc<dyn SelfServiceKeyIdentityResolver>,
    /// None is canonical get_database's 503, not an unauthenticated bypass.
    pub service: Option<Arc<dyn SelfServiceApiKeyService>>,
    pub invalidation: Arc<dyn SelfServiceKeyInvalidation>,
}
pub struct SelfServiceKeysApi {
    dependencies: SelfServiceKeyDependencies,
}
#[derive(Clone)]
pub enum SelfServiceKeyOperation {
    Create,
    List { include_inactive: Value },
    Get { key_id: String },
    Revoke { key_id: String },
    Delete { key_id: String },
}
impl SelfServiceKeyOperation {
    fn path(&self) -> String {
        match self {
            Self::Create | Self::List { .. } => BASE.into(),
            Self::Get { key_id } | Self::Delete { key_id } => format!("{BASE}/{key_id}"),
            Self::Revoke { key_id } => format!("{BASE}/{key_id}/revoke"),
        }
    }
}
impl SelfServiceKeysApi {
    pub fn new(dependencies: SelfServiceKeyDependencies) -> Self {
        Self { dependencies }
    }
    pub async fn handle(
        &self,
        operation: SelfServiceKeyOperation,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<Value, SelfServiceApiKeyError> {
        let service = self
            .dependencies
            .service
            .as_ref()
            .ok_or_else(|| SelfServiceApiKeyError::detail(503, "Database not available"))?;
        let identity = self
            .dependencies
            .identity
            .resolve(headers, &operation.path())
            .await?;
        if !identity.is_authenticated {
            return Err(SelfServiceApiKeyError::detail(
                401,
                "Authentication required",
            ));
        }
        let admin = identity.roles.iter().any(|r| r == "admin");
        let tenant = if identity.tenant_id.is_empty() {
            "default"
        } else {
            &identity.tenant_id
        };
        let filter = KeyFilter {
            user_id: if admin {
                None
            } else {
                Some(identity.user_id.clone())
            },
            tenant_id: Some(tenant.into()),
            include_inactive: false,
        };
        match operation {
            SelfServiceKeyOperation::Create => {
                let input = validate_create(body)?;
                let scopes = input["scopes"]
                    .as_array()
                    .expect("validated scopes")
                    .iter()
                    .map(|v| v.as_str().expect("validated scope").to_owned())
                    .collect::<Vec<_>>();
                let disallowed = scopes
                    .iter()
                    .filter(|s| !DEFAULT_SCOPES.contains(&s.as_str()))
                    .cloned()
                    .collect::<BTreeSet<_>>();
                if !disallowed.is_empty() {
                    return Err(SelfServiceApiKeyError::detail(
                        403,
                        json!({"message":"Requested API key scopes are not available for self-service issuance","disallowed_scopes":disallowed,"allowed_scopes":DEFAULT_SCOPES}),
                    ));
                }
                let input = CreateKey {
                    name: input["name"].as_str().unwrap().into(),
                    description: input["description"].as_str().unwrap_or("").into(),
                    user_id: identity.user_id,
                    tenant_id: identity.tenant_id,
                    scopes,
                    tier: if identity.tier.is_empty() {
                        "normal".into()
                    } else {
                        identity.tier
                    },
                    expires_at: parse_datetime(&input["expires_at"])?,
                };
                let result = service.create(&input).await?;
                self.dependencies.invalidation.clear()?;
                project_response(&result, true)
            }
            SelfServiceKeyOperation::List { include_inactive } => {
                let include_inactive = if include_inactive.is_null() {
                    false
                } else {
                    boolean(&include_inactive).map_err(|kind| {
                        SelfServiceApiKeyError::validation(vec![validation(
                            kind,
                            json!(["query", "include_inactive"]),
                            &include_inactive,
                        )])
                    })?
                };
                let rows = service
                    .list(&KeyFilter {
                        include_inactive,
                        ..filter
                    })
                    .await?;
                rows.iter()
                    .map(|v| project_response(v, false))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Value::Array)
            }
            SelfServiceKeyOperation::Get { key_id } => {
                let row = service
                    .get(&key_id, Some(tenant))
                    .await?
                    .ok_or_else(not_found)?;
                let owner = row.get("user_id").ok_or(SelfServiceApiKeyError::Internal)?;
                if !admin && python_value_to_string(owner) != identity.user_id {
                    return Err(SelfServiceApiKeyError::detail(403, "Access denied"));
                }
                project_response(&row, false)
            }
            SelfServiceKeyOperation::Revoke { key_id } => {
                if !service.revoke(&key_id, &filter).await? {
                    return Err(not_found());
                }
                self.dependencies.invalidation.clear()?;
                Ok(json!({"message":"API Key revoked","key_id":key_id}))
            }
            SelfServiceKeyOperation::Delete { key_id } => {
                if !service.delete(&key_id, &filter).await? {
                    return Err(not_found());
                }
                self.dependencies.invalidation.clear()?;
                Ok(json!({"message":"API Key deleted","key_id":key_id}))
            }
        }
    }
}
fn not_found() -> SelfServiceApiKeyError {
    SelfServiceApiKeyError::detail(404, "API Key not found")
}
fn validation(kind: &str, loc: Value, input: &Value) -> Value {
    let msg = match kind {
        "missing" => "Field required",
        "string_type" => "Input should be a valid string",
        "list_type" => "Input should be a valid list",
        "bool_parsing" => "Input should be a valid boolean, unable to interpret input",
        "bool_type" => "Input should be a valid boolean",
        _ => "Input should be a valid dictionary or object to extract fields from",
    };
    json!({"type":kind,"loc":loc,"msg":msg,"input":input})
}
pub fn validate_create(body: &Value) -> Result<Value, SelfServiceApiKeyError> {
    let Some(object) = body.as_object() else {
        return Err(SelfServiceApiKeyError::validation(vec![validation(
            if body.is_null() {
                "missing"
            } else {
                "model_attributes_type"
            },
            json!(["body"]),
            body,
        )]));
    };
    let mut errors = Vec::new();
    let mut result = json!({"name":null,"description":null,"scopes":DEFAULT_SCOPES,"tier":"normal","expires_at":null});
    for (field, max, required) in [
        ("name", Some(NAME_MAX), true),
        ("description", Some(DESCRIPTION_MAX), false),
        ("scopes", None, false),
        ("tier", None, false),
        ("expires_at", None, false),
    ] {
        let Some(value) = object.get(field) else {
            if required {
                errors.push(validation("missing", json!(["body", field]), &Value::Null));
            }
            continue;
        };
        if value.is_null() && !required {
            if field != "scopes" {
                result[field] = Value::Null;
            }
            continue;
        }
        if field == "scopes" {
            if let Some(items) = value.as_array() {
                for (i, item) in items.iter().enumerate() {
                    if !item.is_string() {
                        errors.push(validation("string_type", json!(["body", field, i]), item));
                    }
                }
                result[field] = value.clone();
            } else {
                errors.push(validation("list_type", json!(["body", field]), value));
            }
        } else if field == "expires_at" {
            match parse_datetime(value){Ok(Some(v))=>result[field]=json!(v.to_isoformat()),_=>errors.push(json!({"type":"datetime_from_date_parsing","loc":["body",field],"msg":"Input should be a valid datetime or date, invalid datetime","input":value,"ctx":{"error":"invalid datetime"}}))}
        } else if let Some(text) = value.as_str() {
            let count = text.chars().count();
            if required && count == 0 {
                errors.push(json!({"type":"string_too_short","loc":["body",field],"msg":"String should have at least 1 character","input":value,"ctx":{"min_length":1}}));
            } else if let Some(max) = max.filter(|max| count > *max) {
                errors.push(json!({"type":"string_too_long","loc":["body",field],"msg":format!("String should have at most {max} characters"),"input":value,"ctx":{"max_length":max}}));
            } else {
                result[field] = value.clone();
            }
        } else {
            errors.push(validation("string_type", json!(["body", field]), value));
        }
    }
    // Pydantic's missing error contains the complete input mapping.
    for error in &mut errors {
        if error["type"] == "missing" {
            error["input"] = body.clone();
        }
    }
    if errors.is_empty() {
        Ok(result)
    } else {
        Err(SelfServiceApiKeyError::validation(errors))
    }
}
fn timestamp_in_source_range(value: ProtocolTimestamp) -> bool {
    let (year, nanos) = match value {
        ProtocolTimestamp::Naive(v) => (v.year(), v.nanosecond()),
        ProtocolTimestamp::Offset(v) => (v.year(), v.nanosecond()),
    };
    (1..=9999).contains(&year) && nanos < 1_000_000_000
}
fn parse_iso_datetime(text: &str) -> Option<ProtocolTimestamp> {
    // Pydantic permits minute precision, lowercase t/z, underscore and comma
    // fractions, but not hour-only times, leap seconds or offset seconds.
    if !text.is_ascii() || text.len() < 10 || &text[4..5] != "-" || &text[7..8] != "-" {
        return None;
    }
    let mut normalized = text.to_owned();
    if text.len() > 10 {
        if !matches!(text.as_bytes()[10], b'T' | b't' | b' ' | b'_') {
            return None;
        }
        normalized.replace_range(10..11, "T");
        let time = &text[11..];
        let zone_start = time.find(['+', '-', 'Z', 'z']).unwrap_or(time.len());
        let clock = &time[..zone_start];
        if clock.len() < 5 || clock.as_bytes()[2] != b':' {
            return None;
        }
        if clock.len() > 5
            && (clock.len() < 8
                || clock.as_bytes()[5] != b':'
                || clock[6..8].parse::<u32>().ok()? > 59)
        {
            return None;
        }
        let zone = &time[zone_start..];
        if !zone.is_empty() && zone != "Z" && zone != "z" {
            let digits = &zone[1..];
            let valid = (digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_digit()))
                || (digits.len() == 5
                    && digits.as_bytes()[2] == b':'
                    && digits[..2]
                        .bytes()
                        .chain(digits[3..].bytes())
                        .all(|b| b.is_ascii_digit()));
            if !valid {
                return None;
            }
        }
        normalized = normalized.replace(',', ".");
        if normalized.ends_with('z') {
            normalized.pop();
            normalized.push('Z');
        }
        if clock.len() == 5 {
            // Reuse the existing timestamp parser after supplying omitted seconds.
            normalized.insert_str(11 + 5, ":00");
        }
    }
    ProtocolTimestamp::parse_isoformat(&normalized)
        .ok()
        .filter(|v| timestamp_in_source_range(*v))
}
fn parse_datetime(value: &Value) -> Result<Option<ProtocolTimestamp>, SelfServiceApiKeyError> {
    if value.is_null() {
        return Ok(None);
    }
    let is_text = value.is_string();
    if let Some(text) = value.as_str() {
        if let Some(parsed) = parse_iso_datetime(text) {
            return Ok(Some(parsed));
        }
        // Numeric strings accept decimal notation, not exponent/whitespace or
        // Rust float spellings. Their negative-fraction path differs from floats.
        let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
        if unsigned.is_empty()
            || unsigned.bytes().filter(|b| *b == b'.').count() > 1
            || !unsigned.bytes().any(|b| b.is_ascii_digit())
            || !unsigned.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        {
            return Err(SelfServiceApiKeyError::Internal);
        }
    }
    let number = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|v| v.parse::<f64>().ok()));
    if let Some(number) = number.filter(|v| v.is_finite()) {
        let total = if is_text {
            // Integral strings are exact, including millisecond values near
            // datetime.max. Keep decimal text on its separate source-compatible
            // path; it is not interchangeable with actual numeric float input.
            if let Some(integral) = value
                .as_str()
                .filter(|text| !text.contains('.'))
                .and_then(|text| text.parse::<i64>().ok())
            {
                integral.checked_mul(if integral.unsigned_abs() > 20_000_000_000 {
                    1000
                } else {
                    1_000_000
                })
            } else {
                let scaled = if number.abs() > 20_000_000_000.0 {
                    number / 1000.0
                } else {
                    number
                };
                let seconds = scaled.floor();
                if seconds >= i64::MIN as f64 && seconds < 9223372036854775808.0 {
                    (seconds as i64).checked_mul(1_000_000).and_then(|v| {
                        v.checked_add(((scaled - seconds) * 1_000_000.0).round() as i64)
                    })
                } else {
                    None
                }
            }
        } else {
            // pydantic-core float input floors its integer timestamp but uses
            // the absolute original fractional remainder. The millisecond
            // threshold is applied independently to the integer and fraction.
            let integral = number.floor();
            if integral >= i64::MIN as f64 && integral < 9223372036854775808.0 {
                let scale = if integral.abs() > 20_000_000_000.0 {
                    1000
                } else {
                    1_000_000
                };
                let fraction_scale = if number.abs() > 20_000_000_000.0 {
                    1000.0
                } else {
                    1_000_000.0
                };
                (integral as i64).checked_mul(scale).and_then(|v| {
                    v.checked_add((number.fract().abs() * fraction_scale).round() as i64)
                })
            } else {
                None
            }
        };
        if let Some(v) = total.and_then(DateTime::<Utc>::from_timestamp_micros) {
            let timestamp = ProtocolTimestamp::from(v.fixed_offset());
            if timestamp_in_source_range(timestamp) {
                return Ok(Some(timestamp));
            }
        }
    }
    Err(SelfServiceApiKeyError::Internal)
}
fn wire_datetime(value: &Value) -> Result<Value, SelfServiceApiKeyError> {
    let Some(parsed) = parse_datetime(value)? else {
        return Ok(Value::Null);
    };
    let text = parsed.to_isoformat();
    Ok(json!(if let Some(utc) = text.strip_suffix("+00:00") {
        format!("{utc}Z")
    } else {
        text
    }))
}
/// FastAPI response-model filtering: no ID, tenant, hash or extra fields leak.
/// Response validation failure becomes canonical 500, not a client 422.
pub fn project_response(value: &Value, create: bool) -> Result<Value, SelfServiceApiKeyError> {
    let input = value.as_object().ok_or(SelfServiceApiKeyError::Internal)?;
    let mut out = Map::new();
    for field in [
        "key_id",
        "key_prefix",
        "name",
        "description",
        "user_id",
        "derived_user_id",
        "scopes",
        "roles",
        "tier",
        "rate_limit",
        "is_active",
        "created_at",
        "last_used_at",
        "expires_at",
        "use_count",
    ] {
        let required = matches!(field, "key_id" | "name" | "is_active" | "created_at");
        let v = input.get(field).cloned().unwrap_or_else(|| {
            if field == "use_count" {
                json!(0)
            } else {
                Value::Null
            }
        });
        if required && v.is_null() {
            return Err(SelfServiceApiKeyError::Internal);
        }
        let parsed = match field {
            "rate_limit" => v,
            "is_active" => json!(boolean(&v).map_err(|_| SelfServiceApiKeyError::Internal)?),
            "use_count" => json!(integer(&v).map_err(|_| SelfServiceApiKeyError::Internal)?),
            "created_at" | "last_used_at" | "expires_at" => wire_datetime(&v)?,
            "scopes" | "roles" => {
                if !v.is_null() && !v.as_array().is_some_and(|a| a.iter().all(Value::is_string)) {
                    return Err(SelfServiceApiKeyError::Internal);
                }
                v
            }
            _ => {
                if !v.is_null() && !v.is_string() {
                    return Err(SelfServiceApiKeyError::Internal);
                }
                v
            }
        };
        out.insert(field.into(), parsed);
    }
    if create {
        for field in ["api_key", "warning"] {
            let v = input
                .get(field)
                .filter(|v| v.is_string())
                .ok_or(SelfServiceApiKeyError::Internal)?;
            out.insert(field.into(), v.clone());
        }
    }
    Ok(Value::Object(out))
}
#[derive(Clone)]
struct HttpState {
    api: Arc<SelfServiceKeysApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
}
pub fn router(
    api: Arc<SelfServiceKeysApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
) -> Router {
    Router::new()
        .route(BASE, any(dispatch))
        .route(KEY, any(dispatch))
        .route(REVOKE, any(dispatch))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn response(
    state: &HttpState,
    result: Result<Value, SelfServiceApiKeyError>,
    context: &ConfigAuditRequestContext,
) -> Response {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(SelfServiceApiKeyError::Cancelled) => state.cancellation.cancelled().await,
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
    let allowed: &[&str] = match matched.as_str() {
        BASE => &["POST", "GET"],
        KEY => &["GET", "DELETE"],
        _ => &["POST"],
    };
    let method = request.method().as_str();
    if !allowed.contains(&method) {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", allowed[0])],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response();
    }
    let id = params.get("key_id").cloned().unwrap_or_default();
    let operation = match (matched.as_str(), method) {
        (BASE, "POST") => SelfServiceKeyOperation::Create,
        (BASE, "GET") => {
            let pairs = match Query::<Vec<(String, String)>>::try_from_uri(request.uri()) {
                Ok(Query(v)) => v,
                Err(e) => return e.into_response(),
            };
            SelfServiceKeyOperation::List {
                include_inactive: pairs
                    .iter()
                    .rev()
                    .find(|(k, _)| k == "include_inactive")
                    .map(|(_, v)| json!(v))
                    .unwrap_or(Value::Null),
            }
        }
        (_, "DELETE") => SelfServiceKeyOperation::Delete { key_id: id },
        (_, "GET") => SelfServiceKeyOperation::Get { key_id: id },
        _ => SelfServiceKeyOperation::Revoke { key_id: id },
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
    let body = if matches!(operation, SelfServiceKeyOperation::Create) {
        let bytes = match Bytes::from_request(request, &state).await {
            Ok(v) => v,
            Err(e) => return e.into_response(),
        };
        if bytes.is_empty() {
            Value::Null
        } else {
            let is_json = crate::http_content_type::is_json_content_type(&headers);
            if is_json {
                match serde_json::from_slice(&bytes){Ok(v)=>v,Err(_)=>return response(&state,Err(SelfServiceApiKeyError::validation(vec![json!({"type":"json_invalid","loc":["body",0],"msg":"JSON decode error","input":{},"ctx":{"error":"Invalid JSON"}})])),&context).await}
            } else {
                json!(String::from_utf8_lossy(&bytes))
            }
        }
    } else {
        Value::Null
    };
    let result = state.api.handle(operation, &headers, &body).await;
    response(&state, result, &context).await
}
