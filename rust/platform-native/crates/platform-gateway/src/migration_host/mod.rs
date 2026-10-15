//! Gateway process assembly migrated from src/main.py and src/container.py.
//! Construction does not bind a listener; serve is the explicit runtime boundary.
pub mod application;
pub mod authorities;
pub mod bootstrap;
pub mod catalog;
pub use ai_platform_gateway_integrations::chatgpt_plan;
pub mod content;
pub mod context;
pub mod cors;
pub mod errors;
pub mod foundation;
pub mod google_auth;
pub mod idempotency;
pub mod identity;
pub mod jwt;
pub mod knowledge_bootstrap;
pub mod knowledge_proxy;
pub mod lifecycle;
pub mod logging;
pub mod memory;
pub mod metrics;
pub mod metrics_http;
pub mod middleware;
pub mod model_pricing;
pub mod observability;
pub mod pdf_backend;
pub mod rate;
pub mod readiness;
pub mod release;
pub mod resources;
pub mod settings;
pub mod studio;
mod studio_sql;
pub mod version;
use axum::{Router, middleware::from_fn_with_state};
use middleware::{DrainState, MiddlewareConfig, MiddlewareState};
use std::{future::Future, net::SocketAddr, sync::Arc};

pub struct GatewayHost {
    pub router: Router,
    pub drain: Arc<DrainState>,
    pub lifecycle: lifecycle::Lifecycle,
}
impl GatewayHost {
    pub fn new(
        business_routes: Router,
        config: MiddlewareConfig,
        lifecycle: lifecycle::Lifecycle,
    ) -> Self {
        let drain = Arc::new(DrainState::default());
        let state = MiddlewareState {
            config: Arc::new(config),
            drain: drain.clone(),
        };
        let router = business_routes
            .layer(from_fn_with_state(
                state.clone(),
                middleware::request_body_limit,
            ))
            .layer(from_fn_with_state(
                state.clone(),
                middleware::request_context,
            ))
            .layer(from_fn_with_state(state, middleware::graceful_drain))
            .layer(axum::middleware::from_fn(middleware::security_headers));
        Self {
            router,
            drain,
            lifecycle,
        }
    }
    /// Full source middleware order. Endpoint routers perform authorization;
    /// the identity layer only supplies verified/fallback request context.
    pub fn assembled(
        business_routes: Router,
        config: MiddlewareConfig,
        identity: identity::IdentityConfig,
        cors: cors::CorsConfig,
        version: version::VersionConfig,
        rate: rate::RateState,
        logging: Arc<logging::LoggingState>,
        lifecycle: lifecycle::Lifecycle,
    ) -> Self {
        Self::assembled_with_drain(
            business_routes,
            config,
            identity,
            cors,
            version,
            rate,
            logging,
            lifecycle,
            Arc::new(DrainState::default()),
        )
    }
    /// Share the exact drain authority with readiness and request middleware.
    pub fn assembled_with_drain(
        business_routes: Router,
        config: MiddlewareConfig,
        identity: identity::IdentityConfig,
        cors: cors::CorsConfig,
        version: version::VersionConfig,
        rate: rate::RateState,
        logging: Arc<logging::LoggingState>,
        lifecycle: lifecycle::Lifecycle,
        drain: Arc<DrainState>,
    ) -> Self {
        let state = MiddlewareState {
            config: Arc::new(config),
            drain: drain.clone(),
        };
        let router = business_routes
            .layer(from_fn_with_state(Arc::new(version), version::negotiate))
            .layer(from_fn_with_state(
                state.clone(),
                middleware::anonymous_identity,
            ))
            .layer(from_fn_with_state(rate, rate::enforce))
            .layer(from_fn_with_state(
                state.clone(),
                middleware::request_body_limit,
            ))
            .layer(from_fn_with_state(Arc::new(identity), identity::inject))
            .layer(axum::middleware::from_fn(context::bridge))
            .layer(from_fn_with_state(logging, logging::record))
            .layer(from_fn_with_state(Arc::new(cors.normalized()), cors::apply))
            .layer(from_fn_with_state(
                state.clone(),
                middleware::graceful_drain,
            ))
            .layer(axum::middleware::from_fn(middleware::security_headers))
            .layer(from_fn_with_state(state, middleware::tracing_context));
        Self {
            router,
            drain,
            lifecycle,
        }
    }
    pub async fn serve(
        self,
        address: SocketAddr,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), lifecycle::HostError> {
        self.serve_with_shutdown_deadlines(
            address,
            shutdown,
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(30),
        )
        .await
    }
    /// Bounds request drain and lifecycle cleanup separately. At the deadline,
    /// dropping Axum's serve future does not prove its connection tasks stopped.
    /// Standalone binaries must end their owned runtime after this returns; an
    /// embedding host must arrange equivalent task ownership before reusing pools.
    pub async fn serve_with_shutdown_deadlines(
        self,
        address: SocketAddr,
        shutdown: impl Future<Output = ()> + Send + 'static,
        drain_timeout: std::time::Duration,
        cleanup_timeout: std::time::Duration,
    ) -> Result<(), lifecycle::HostError> {
        use std::future::IntoFuture;
        self.lifecycle.start().await?;
        let listener = match tokio::net::TcpListener::bind(address).await {
            Ok(v) => v,
            Err(_) => {
                let _ = tokio::time::timeout(
                    cleanup_timeout,
                    self.lifecycle
                        .stop_with_drain_timeout(&self.drain, std::time::Duration::ZERO),
                )
                .await;
                return Err(lifecycle::HostError::Listener);
            }
        };
        let drain = self.drain.clone();
        let (started, shutdown_started) = tokio::sync::oneshot::channel();
        let serving = axum::serve(
            listener,
            self.router
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            shutdown.await;
            drain.begin_drain();
            let _ = started.send(());
        })
        .into_future();
        tokio::pin!(serving);
        let outcome = tokio::select! {
            result = &mut serving => result.map_err(|_|lifecycle::HostError::Listener),
            _ = shutdown_started => {
                match tokio::time::timeout(drain_timeout, &mut serving).await {
                    Ok(result) => result.map_err(|_|lifecycle::HostError::Listener),
                    Err(_) => Err(lifecycle::HostError::Shutdown("http_drain_deadline")),
                }
            }
        };
        self.drain.begin_drain();
        let stopped = tokio::time::timeout(
            cleanup_timeout,
            self.lifecycle
                .stop_with_drain_timeout(&self.drain, std::time::Duration::ZERO),
        )
        .await
        .map_err(|_| lifecycle::HostError::Shutdown("lifecycle_cleanup_deadline"))?;
        outcome?;
        stopped
    }
}
