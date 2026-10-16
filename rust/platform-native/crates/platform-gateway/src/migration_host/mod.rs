//! Gateway process assembly migrated from src/main.py and src/container.py.
//! Construction does not bind a listener; serve is the explicit runtime boundary.
pub mod application;
pub mod authorities;
pub mod bootstrap;
pub mod catalog;
pub(super) mod local_node_channel;
pub(super) mod local_node_channel_store;
pub(super) mod local_node_tls;
pub mod native_public_contract;
pub use ai_platform_gateway_integrations::chatgpt_plan;
pub mod content;
pub mod context;
pub mod context_metrics;
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
    local_node_https: Option<LocalNodeHttpsService>,
}
struct LocalNodeHttpsService {
    router: Router,
    configuration: local_node_tls::GatewayHttpsConfiguration,
}
impl GatewayHost {
    pub fn new(
        business_routes: Router,
        config: MiddlewareConfig,
        lifecycle: lifecycle::Lifecycle,
    ) -> Self {
        let drain = Arc::new(DrainState::default());
        let governance = ai_platform_gateway_foundation::request_governance::RequestGovernance::new(
            config.request_governance.clone(),
        );
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
                governance,
                ai_platform_gateway_foundation::request_governance::govern,
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
            local_node_https: None,
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
        let governance = ai_platform_gateway_foundation::request_governance::RequestGovernance::new(
            config.request_governance.clone(),
        );
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
            .layer(from_fn_with_state(
                governance,
                ai_platform_gateway_foundation::request_governance::govern,
            ))
            .layer(axum::middleware::from_fn(middleware::distributed_tracing))
            .layer(axum::middleware::from_fn(middleware::security_headers))
            .layer(from_fn_with_state(state, middleware::tracing_context));
        Self {
            router,
            drain,
            lifecycle,
            local_node_https: None,
        }
    }
    /// Attach the companion-only router to a dedicated TLS listener. Worker
    /// routes remain on `router`, where their capability proof is enforced.
    pub fn with_local_node_https(
        mut self,
        router: Router,
        configuration: local_node_tls::GatewayHttpsConfiguration,
    ) -> Self {
        self.local_node_https = Some(LocalNodeHttpsService {
            router,
            configuration,
        });
        self
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
        if self.local_node_https.is_some() {
            return self
                .serve_with_local_node_https(address, shutdown, drain_timeout, cleanup_timeout)
                .await;
        }
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

    async fn serve_with_local_node_https(
        mut self,
        address: SocketAddr,
        shutdown: impl Future<Output = ()> + Send + 'static,
        drain_timeout: std::time::Duration,
        cleanup_timeout: std::time::Duration,
    ) -> Result<(), lifecycle::HostError> {
        use std::future::IntoFuture;
        self.lifecycle.start().await?;
        let http_listener = match tokio::net::TcpListener::bind(address).await {
            Ok(listener) => listener,
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
        let https = self
            .local_node_https
            .take()
            .ok_or(lifecycle::HostError::Configuration)?;
        let https_listener =
            match local_node_tls::GatewayHttpsListener::bind(&https.configuration).await {
                Ok(listener) => listener,
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

        let https_router = https
            .router
            .layer(axum::middleware::from_fn(local_node_tls::mark_https));
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let mut http_shutdown = shutdown_rx.clone();
        let http_drain = self.drain.clone();
        let http_serving = axum::serve(
            http_listener,
            self.router
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            wait_for_host_shutdown(&mut http_shutdown).await;
            http_drain.begin_drain();
        })
        .into_future();
        let mut https_shutdown = shutdown_rx;
        let https_drain = self.drain.clone();
        let https_serving = axum::serve(
            https_listener,
            https_router.into_make_service_with_connect_info::<local_node_tls::TlsPeerAddress>(),
        )
        .with_graceful_shutdown(async move {
            wait_for_host_shutdown(&mut https_shutdown).await;
            https_drain.begin_drain();
        })
        .into_future();
        tokio::pin!(http_serving);
        tokio::pin!(https_serving);
        tokio::pin!(shutdown);
        let outcome = tokio::select! {
            result = &mut http_serving => result.map_err(|_| lifecycle::HostError::Listener),
            result = &mut https_serving => result.map_err(|_| lifecycle::HostError::Listener),
            _ = &mut shutdown => Ok(()),
        };
        self.drain.begin_drain();
        let _ = shutdown_tx.send(true);
        let drain_result = tokio::time::timeout(drain_timeout, async {
            let (http, https) = tokio::join!(&mut http_serving, &mut https_serving);
            http.map_err(|_| lifecycle::HostError::Listener)?;
            https.map_err(|_| lifecycle::HostError::Listener)?;
            Ok::<(), lifecycle::HostError>(())
        })
        .await
        .map_err(|_| lifecycle::HostError::Shutdown("http_drain_deadline"))?;
        let stopped = tokio::time::timeout(
            cleanup_timeout,
            self.lifecycle
                .stop_with_drain_timeout(&self.drain, std::time::Duration::ZERO),
        )
        .await
        .map_err(|_| lifecycle::HostError::Shutdown("lifecycle_cleanup_deadline"))?;
        drain_result?;
        outcome?;
        stopped
    }
}

async fn wait_for_host_shutdown(receiver: &mut tokio::sync::watch::Receiver<bool>) {
    while !*receiver.borrow() {
        if receiver.changed().await.is_err() {
            break;
        }
    }
}

#[allow(dead_code)]
#[path = "../../../platform-openapi/src/document.rs"]
pub mod openapi_document;

pub mod credential_introspection;
