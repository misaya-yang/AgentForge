//! Route-level anonymous rate admission reuses the accepted native limiter.
//! Extensions are trusted host state, never values taken from HTTP headers.
use crate::{
    client_ip::{TrustedProxies, get_client_ip},
    quiz_types::QuizError,
    rate_limits::{
        MultiDimensionConfig, RateLimitContext, RateLimitResult, SlidingWindowRateLimiter,
    },
};
use axum::http::{Extensions, HeaderMap};
use std::{collections::BTreeSet, sync::Arc};
#[derive(Clone, Debug, Default)]
pub struct QuizRequestContext {
    pub direct_client_host: Option<String>,
    pub counted_dimensions: BTreeSet<String>,
}
#[derive(Clone)]
pub struct QuizRateAdmission {
    pub limiter: Arc<SlidingWindowRateLimiter>,
    pub config: Arc<MultiDimensionConfig>,
}
impl QuizRateAdmission {
    pub async fn check(
        &self,
        ip: &str,
        operation: &str,
        counted: &BTreeSet<String>,
    ) -> Result<RateLimitResult, QuizError> {
        let context = RateLimitContext {
            ip: ip.into(),
            operation: Some(operation.into()),
            ..Default::default()
        };
        let policies = self
            .config
            .policies(&context, counted)
            .map_err(|_| QuizError::Internal)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| QuizError::Internal)?
            .as_secs_f64();
        self.limiter
            .check_dimensions_at(&policies, now)
            .await
            .map_err(|_| QuizError::Internal)
    }
}
pub fn request_context(
    headers: &HeaderMap,
    extensions: &Extensions,
    trusted: &TrustedProxies,
) -> (String, BTreeSet<String>) {
    let context = extensions
        .get::<QuizRequestContext>()
        .cloned()
        .unwrap_or_default();
    let peer = context.direct_client_host.or_else(|| {
        extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|v| v.0.ip().to_string())
    });
    (
        get_client_ip(headers, peer.as_deref(), Some(trusted)),
        context.counted_dimensions,
    )
}
