//! HTTP single-authority rate counting; route-level service/model checks receive
//! the precise set of dimensions already charged here.
use super::identity::{StreamingIdentity, client_ip};
use crate::rate_limits::{RatePolicy, SlidingWindowRateLimiter};
use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    http::{HeaderName, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    sync::Arc,
};
#[derive(Clone)]
pub struct RateConfig {
    pub enabled: bool,
    pub global_limit: i64,
    pub global_window: i64,
    pub user_limit: i64,
    pub user_window: i64,
    pub guest_limit: i64,
    pub guest_window: i64,
    pub ip_limit: i64,
    pub ip_window: i64,
    pub tenant_limit: Option<i64>,
    pub tenant_window: i64,
    pub user_tier_limits: BTreeMap<String, i64>,
    pub whitelist_paths: Vec<String>,
    pub trusted_proxy_cidrs: Vec<String>,
}
impl Default for RateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            global_limit: 1000,
            global_window: 60,
            user_limit: 30,
            user_window: 60,
            guest_limit: 10,
            guest_window: 60,
            ip_limit: 60,
            ip_window: 60,
            tenant_limit: None,
            tenant_window: 60,
            user_tier_limits: BTreeMap::new(),
            whitelist_paths: vec![
                "/health".into(),
                "/health/live".into(),
                "/health/ready".into(),
            ],
            trusted_proxy_cidrs: vec!["127.0.0.0/8".into(), "::1/128".into()],
        }
    }
}
#[derive(Clone)]
pub struct RateState {
    pub config: Arc<RateConfig>,
    pub limiter: Arc<SlidingWindowRateLimiter>,
}
#[derive(Clone)]
pub struct CountedDimensions(pub BTreeSet<String>);
fn append(response: &mut Response, key: &str, value: &str) {
    if let (Ok(key), Ok(value)) = (
        HeaderName::from_bytes(key.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        response.headers_mut().append(key, value);
    }
}
pub async fn enforce(State(state): State<RateState>, mut request: Request, next: Next) -> Response {
    let c = &state.config;
    if !c.enabled
        || c.whitelist_paths.iter().any(|p| {
            request.uri().path() == p || request.uri().path().starts_with(&format!("{p}/"))
        })
    {
        return next.run(request).await;
    }
    let direct = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|v| v.0.ip().to_string());
    let ip = client_ip(request.headers(), direct.as_deref(), &c.trusted_proxy_cidrs);
    let mut checks = vec![RatePolicy::new(
        "ratelimit:global",
        "global",
        c.global_limit,
        c.global_window,
    )];
    if let Some(user) = request.extensions().get::<StreamingIdentity>() {
        let field = |key: &str| user.0[key].as_str().unwrap_or("");
        match field("user_type") {
            "user" => {
                let tier = if field("tier").is_empty() {
                    "normal"
                } else {
                    field("tier")
                };
                let limit = c
                    .user_tier_limits
                    .get(tier)
                    .copied()
                    .unwrap_or(c.user_limit);
                checks.push(RatePolicy::new(
                    format!("ratelimit:user:{}", field("user_id")),
                    "user",
                    limit,
                    c.user_window,
                ));
                if let Some(limit) = c.tenant_limit
                    && !field("tenant_id").is_empty()
                {
                    checks.push(RatePolicy::new(
                        format!("ratelimit:tenant:{}", field("tenant_id")),
                        "tenant",
                        limit,
                        c.tenant_window,
                    ));
                }
            }
            "guest" | "anonymous" => {
                let key = if field("user_type") == "guest" {
                    field("user_id")
                } else {
                    &ip
                };
                checks.push(RatePolicy::new(
                    format!("ratelimit:guest:{key}"),
                    "guest",
                    c.guest_limit,
                    c.guest_window,
                ));
            }
            _ => {}
        }
    }
    checks.push(RatePolicy::new(
        format!("ratelimit:ip:{ip}"),
        "ip",
        c.ip_limit,
        c.ip_window,
    ));
    let result = match state.limiter.check_http_many(&checks).await {
        Ok(info) => info,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
        }
    };
    if !result.result.allowed {
        let info = result.result;
        let dimension = usize::try_from(result.dimension_index)
            .ok()
            .and_then(|i| checks.get(i))
            .map(|p| p.dimension.as_str())
            .unwrap_or("");
        let mut response=(StatusCode::TOO_MANY_REQUESTS,Json(json!({"error":{"code":"RATE_LIMIT_EXCEEDED","message":"Too many requests","dimension":dimension,"limit":info.limit,"remaining":0,"reset_at":info.reset_at,"retry_after":info.retry_after}}))).into_response();
        for (k, v) in [
            ("x-ratelimit-limit", info.limit.to_string()),
            ("x-ratelimit-remaining", "0".into()),
            ("x-ratelimit-reset", info.reset_at.to_string()),
            ("x-ratelimit-dimension", dimension.into()),
            ("retry-after", info.retry_after.to_string()),
        ] {
            append(&mut response, k, &v);
        }
        return response;
    }
    request.extensions_mut().insert(CountedDimensions(
        checks.into_iter().map(|p| p.dimension).collect(),
    ));
    let mut response = next.run(request).await;
    append(
        &mut response,
        "x-ratelimit-limit",
        &c.user_limit.to_string(),
    );
    append(
        &mut response,
        "x-ratelimit-window",
        &c.user_window.to_string(),
    );
    response
}
impl RateConfig {
    pub fn from_policy(policy: &crate::rate_limits::MultiDimensionConfig) -> Self {
        let tiers: BTreeMap<String, i64> = policy
            .user_tiers
            .iter()
            .map(|(name, tier)| {
                (
                    name.clone(),
                    tier.requests.saturating_add(tier.burst).max(1),
                )
            })
            .collect();
        let ip = policy
            .ip
            .unwrap_or(crate::rate_limits::TierLimit::new(30, 60));
        Self {
            global_limit: 5000,
            guest_limit: 200,
            user_limit: tiers.get("normal").copied().unwrap_or(60),
            user_tier_limits: tiers,
            ip_limit: if ip.requests == 0 { 30 } else { ip.requests },
            ip_window: if ip.window == 0 { 60 } else { ip.window },
            tenant_limit: policy
                .tenant
                .filter(|_| policy.tenant_limits.is_empty())
                .map(|t| t.requests),
            tenant_window: policy.tenant.map(|t| t.window).unwrap_or(60),
            whitelist_paths: [
                "/health",
                "/health/live",
                "/health/ready",
                "/docs",
                "/openapi.json",
                "/version",
                "/api/v1/services",
                "/api/v1/assistant/config",
                "/api/v1/assistant/models",
                "/api/v1/assistant/datasets",
                "/internal/v1/agent-model-plane/responses",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            ..Default::default()
        }
    }
}
