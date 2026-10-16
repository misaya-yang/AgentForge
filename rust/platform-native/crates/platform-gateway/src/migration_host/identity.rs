//! Streaming identity injection. Endpoint authorization remains independent.
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue},
    middleware::Next,
    response::Response,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, sync::Arc};

pub type ClaimVerifier = dyn Fn(&str) -> Result<Value, String> + Send + Sync;
#[derive(Clone)]
pub struct IdentityConfig {
    pub jwt_enabled: bool,
    pub verify_jwt: Option<Arc<ClaimVerifier>>,
    pub api_key_enabled: bool,
    pub api_key_header: String,
    pub api_keys: Vec<String>,
    pub guest_session_enabled: bool,
    pub guest_session_header: String,
    pub anonymous_cookie: String,
    pub whitelist_paths: Vec<String>,
    pub trusted_proxy_cidrs: Vec<String>,
}
impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            jwt_enabled: false,
            verify_jwt: None,
            api_key_enabled: false,
            api_key_header: ai_platform_gateway_foundation::native_contract::API_KEY_HEADER_DEFAULT
                .into(),
            api_keys: vec![],
            guest_session_enabled: true,
            guest_session_header: "X-Guest-Session".into(),
            anonymous_cookie: "ag_anon_id".into(),
            whitelist_paths: [
                "/health",
                "/health/live",
                "/health/ready",
                "/docs",
                "/openapi.json",
                "/version",
                "/internal/v1/agent-model-plane/responses",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            trusted_proxy_cidrs: vec!["127.0.0.0/8".into(), "::1/128".into()],
        }
    }
}
#[derive(Clone)]
pub struct StreamingIdentity(pub Value);
#[derive(Clone)]
pub struct VerifiedJwtClaims(pub Value);
fn header(h: &HeaderMap, key: &str) -> String {
    h.get(key)
        .and_then(|v| std::str::from_utf8(v.as_bytes()).ok())
        .unwrap_or("")
        .to_owned()
}
pub fn client_ip(headers: &HeaderMap, direct: Option<&str>, cidrs: &[String]) -> String {
    let trusted = crate::client_ip::TrustedProxies::from_cidrs(cidrs.iter().map(String::as_str));
    let headers: Vec<(&[u8], &[u8])> = headers
        .iter()
        .map(|(k, v)| (k.as_str().as_bytes(), v.as_bytes()))
        .collect();
    crate::client_ip::get_client_ip_from_scope_headers(&headers, direct, Some(&trusted))
}
fn equal_secret(a: &str, b: &str) -> bool {
    let mut difference = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        difference |= (a.as_bytes().get(i).copied().unwrap_or(0)
            ^ b.as_bytes().get(i).copied().unwrap_or(0)) as usize;
    }
    difference == 0
}
fn guest_valid(s: &str) -> bool {
    if s.is_empty()
        || s.chars().count() > 128
        || s.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return false;
    }
    if let Some(s) = s.strip_prefix("guest_") {
        return s.len() >= 16 && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
    }
    uuid::Uuid::parse_str(s)
        .is_ok_and(|u| u.get_version_num() == 4 && u.to_string() == s.to_lowercase())
}
fn pystr(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        v => v.to_string(),
    }
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
    }
}
pub async fn inject(
    State(config): State<Arc<IdentityConfig>>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let streaming = path.ends_with("/stream")
        || path.ends_with("/sse")
        || path == "/v1/responses"
        || path.to_lowercase().contains("/stream") && !path.to_lowercase().contains("upstream")
        || path.starts_with("/api/v1/langgraph/") && path.contains("/events");
    if !streaming
        && config
            .whitelist_paths
            .iter()
            .any(|p| path == p || path.starts_with(&format!("{p}/")))
    {
        return next.run(request).await;
    }
    let direct = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|v| v.0.ip().to_string());
    let ip = client_ip(
        request.headers(),
        direct.as_deref(),
        &config.trusted_proxy_cidrs,
    );
    let authorization = header(request.headers(), "authorization");
    let mut identity = None;
    if config.jwt_enabled
        && authorization.to_lowercase().starts_with("bearer ")
        && let Some(verify) = &config.verify_jwt
        && let Ok(claims) = verify(&authorization[7..])
    {
        let id = claims
            .get("sub")
            .filter(|v| truthy(v))
            .or_else(|| claims.get("user_id").filter(|v| truthy(v)))
            .map(pystr)
            .unwrap_or_default();
        if !id.is_empty() {
            identity = Some(
                json!({"user_id":id,"user_type":"user","tenant_id":claims.get("tenant_id").map(pystr).unwrap_or_default(),"tier":claims.get("tier").map(pystr).unwrap_or_else(||"normal".into()),"is_authenticated":true,"ip":ip,"roles":claims.get("roles").cloned().unwrap_or(json!(["user"]))}),
            );
        }
        request.extensions_mut().insert(VerifiedJwtClaims(claims));
    }
    let key = header(request.headers(), &config.api_key_header);
    if identity.is_none()
        && !key.is_empty()
        && config.api_key_enabled
        && config.api_keys.iter().any(|v| equal_secret(&key, v))
    {
        let hash = format!("{:x}", Sha256::digest(key.as_bytes()));
        identity = Some(
            json!({"user_id":format!("apikey:{}",&hash[..16]),"user_type":"user","tenant_id":"","tier":"normal","is_authenticated":true,"ip":ip,"roles":["user"]}),
        );
    }
    let guest = header(request.headers(), &config.guest_session_header);
    if identity.is_none() && config.guest_session_enabled && guest_valid(&guest) {
        identity = Some(
            json!({"user_id":guest,"user_type":"guest","tenant_id":"public","tier":"anonymous","is_authenticated":false,"session_id":guest,"ip":ip,"roles":["guest"]}),
        );
    }
    let identity=identity.unwrap_or_else(||{let raw=header(request.headers(),"x-ag-anonymous-id");let raw=if raw.is_empty(){header(request.headers(),"cookie").split(';').find_map(|s|s.trim().split_once('=').filter(|(k,_)|*k==config.anonymous_cookie).map(|(_,v)|v.to_owned())).unwrap_or_default()}else{raw};let clean:String=raw.chars().filter(|c|c.is_ascii_alphanumeric()||matches!(c,'.'|'_'|':'|'-')).take(64).collect();json!({"user_id":format!("anon:{}",if clean.is_empty(){&ip}else{&clean}),"user_type":"anonymous","tenant_id":"public","tier":"anonymous","is_authenticated":false,"ip":ip,"roles":["guest"]})});
    if let Some(telemetry) = request
        .extensions()
        .get::<super::logging::TelemetryIdentity>()
    {
        telemetry.0.lock().unwrap_or_else(|e| e.into_inner()).0 =
            identity["user_id"].as_str().unwrap_or("").into();
    }
    request
        .extensions_mut()
        .insert(StreamingIdentity(identity.clone()));
    let mut response = next.run(request).await;
    for (key, field) in [("x-user-id", "user_id"), ("x-user-type", "user_type")] {
        response.headers_mut().remove(key);
        if let Ok(v) =
            HeaderValue::from_bytes(identity[field].as_str().unwrap_or("unknown").as_bytes())
        {
            response.headers_mut().append(key, v);
        }
    }
    response
}
