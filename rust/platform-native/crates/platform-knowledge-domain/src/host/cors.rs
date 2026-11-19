//! Starlette-compatible CORS response and preflight policy, without collecting streams.
use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;
#[derive(Clone)]
pub struct CorsConfig {
    pub allow_origins: Vec<String>,
    pub allow_credentials: bool,
    pub allow_methods: Vec<String>,
    pub allow_headers: Vec<String>,
    pub expose_headers: Vec<String>,
    pub max_age: u64,
    pub allow_origin_regex: Option<regex::Regex>,
}
impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allow_origins: vec!["*".into()],
            allow_credentials: false,
            allow_methods: vec!["*".into()],
            allow_headers: vec!["*".into()],
            expose_headers: vec![],
            max_age: 600,
            allow_origin_regex: None,
        }
    }
}
impl CorsConfig {
    pub fn normalized(mut self) -> Self {
        if self.allow_origins.iter().any(|v| v == "*") {
            self.allow_credentials = false;
        }
        if self.allow_methods.iter().any(|v| v == "*") {
            self.allow_methods = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]
                .into_iter()
                .map(str::to_owned)
                .collect();
        }
        self
    }
    fn origin_allowed(&self, origin: &str) -> bool {
        self.allow_origins.iter().any(|v| v == "*" || v == origin)
            || self.allow_origin_regex.as_ref().is_some_and(|r| {
                r.find(origin)
                    .is_some_and(|m| m.start() == 0 && m.end() == origin.len())
            })
    }
}
fn value(h: &HeaderMap, k: &str) -> Option<String> {
    h.get(k)
        .map(|v| v.as_bytes().iter().map(|b| char::from(*b)).collect())
}
fn set(h: &mut HeaderMap, k: &str, v: &str) {
    if let (Ok(k), Ok(v)) = (
        HeaderName::from_bytes(k.as_bytes()),
        HeaderValue::from_str(v),
    ) {
        h.insert(k, v);
    }
}
fn vary(h: &mut HeaderMap) {
    let existing = value(h, "vary");
    set(
        h,
        "vary",
        &existing
            .map(|v| format!("{v}, Origin"))
            .unwrap_or_else(|| "Origin".into()),
    );
}
pub async fn apply(
    State(config): State<Arc<CorsConfig>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(origin) = value(request.headers(), "origin") else {
        return next.run(request).await;
    };
    let all = config.allow_origins.iter().any(|v| v == "*");
    let explicit = !all || config.allow_credentials;
    if request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key("access-control-request-method")
    {
        let mut headers = HeaderMap::new();
        if explicit {
            set(&mut headers, "vary", "Origin");
        } else {
            set(&mut headers, "access-control-allow-origin", "*");
        }
        set(
            &mut headers,
            "access-control-allow-methods",
            &config.allow_methods.join(", "),
        );
        set(
            &mut headers,
            "access-control-max-age",
            &config.max_age.to_string(),
        );
        if config.allow_credentials {
            set(&mut headers, "access-control-allow-credentials", "true");
        }
        let all_headers = config.allow_headers.iter().any(|v| v == "*");
        let mut allowed: Vec<String> = [
            "Accept",
            "Accept-Language",
            "Content-Language",
            "Content-Type",
        ]
        .into_iter()
        .map(str::to_owned)
        .chain(config.allow_headers.iter().cloned())
        .collect();
        allowed.sort();
        allowed.dedup();
        if !all_headers {
            set(
                &mut headers,
                "access-control-allow-headers",
                &allowed.join(", "),
            );
        }
        let mut failures = Vec::new();
        if config.origin_allowed(&origin) {
            if explicit {
                set(&mut headers, "access-control-allow-origin", &origin);
            }
        } else {
            failures.push("origin");
        }
        let method = value(request.headers(), "access-control-request-method").unwrap_or_default();
        if !config.allow_methods.contains(&method) {
            failures.push("method");
        }
        if let Some(requested) = value(request.headers(), "access-control-request-headers") {
            if all_headers {
                set(&mut headers, "access-control-allow-headers", &requested);
            } else if requested
                .to_lowercase()
                .split(',')
                .any(|v| !allowed.iter().any(|a| a.to_lowercase() == v.trim()))
            {
                failures.push("headers");
            }
        }
        let (status, body) = if failures.is_empty() {
            (StatusCode::OK, "OK".to_owned())
        } else {
            (
                StatusCode::BAD_REQUEST,
                format!("Disallowed CORS {}", failures.join(", ")),
            )
        };
        return (status, headers, body).into_response();
    }
    let cookies = request.headers().contains_key("cookie");
    let mut response = next.run(request).await;
    if all {
        set(response.headers_mut(), "access-control-allow-origin", "*");
    }
    if config.allow_credentials {
        set(
            response.headers_mut(),
            "access-control-allow-credentials",
            "true",
        );
    }
    if !config.expose_headers.is_empty() {
        set(
            response.headers_mut(),
            "access-control-expose-headers",
            &config.expose_headers.join(", "),
        );
    }
    if all && cookies || !all && config.origin_allowed(&origin) {
        set(
            response.headers_mut(),
            "access-control-allow-origin",
            &origin,
        );
        vary(response.headers_mut());
    }
    response
}
