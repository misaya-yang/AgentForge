//! API version negotiation from ai_gateway_core.proxy.version_middleware.
use axum::{
    extract::{Request, State},
    http::HeaderValue,
    middleware::Next,
    response::Response,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, LazyLock},
};
#[derive(Clone)]
pub struct ApiVersion(pub String);
#[derive(Clone)]
pub struct VersionConfig {
    pub default_version: String,
    pub deprecated_routes: BTreeMap<String, String>,
}
impl Default for VersionConfig {
    fn default() -> Self {
        Self {
            default_version: "v1".into(),
            deprecated_routes: BTreeMap::new(),
        }
    }
}
static ACCEPT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"application/vnd\.ai-gateway\.(v\d+)\+json").expect("constant expression")
});
static PATH: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^/(?:api/)?(v\d+)(?:/|$)").expect("constant expression"));
pub async fn negotiate(
    State(config): State<Arc<VersionConfig>>,
    mut request: Request,
    next: Next,
) -> Response {
    let accept = request
        .headers()
        .get("accept")
        .map(|v| {
            v.as_bytes()
                .iter()
                .map(|b| char::from(*b))
                .collect::<String>()
        })
        .unwrap_or_default();
    let version = ACCEPT
        .captures(&accept)
        .or_else(|| PATH.captures(request.uri().path()))
        .map(|c| c[1].to_owned())
        .unwrap_or_else(|| config.default_version.clone());
    let sunset = config
        .deprecated_routes
        .get(request.uri().path())
        .filter(|v| !v.is_empty())
        .cloned();
    request.extensions_mut().insert(ApiVersion(version));
    let mut response = next.run(request).await;
    if let Some(sunset) = sunset {
        response
            .headers_mut()
            .append("deprecation", HeaderValue::from_static("true"));
        let bytes: Option<Vec<u8>> = sunset
            .chars()
            .map(|c| u8::try_from(u32::from(c)).ok())
            .collect();
        if let Some(bytes) = bytes
            && let Ok(v) = HeaderValue::from_bytes(&bytes)
        {
            response.headers_mut().append("sunset", v);
        }
    }
    response
}
