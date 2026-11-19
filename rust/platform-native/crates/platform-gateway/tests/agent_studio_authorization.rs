//! Native source matrix: tests/security/test_management_api_authorization.py.
//! Trusted identity and unreachable backend fixtures exercise the actual HTTP routers.
use ai_platform_auth::rbac::{Capability, Rbac, check_capability};
use ai_platform_gateway::migration_content::{
    ContentAccess, ContentDatabase, ContentError, ContentFuture, ContentIdentity,
    artifacts::Artifact,
    skills::{SkillRepository, SkillsApi},
};
use ai_platform_gateway_foundation::{
    dashboard_api as d, mcp_registry_api as c, metrics_admin_api as m, provider_admin_api as p,
    provider_service::{CreateProvider, UpdateProvider},
    realtime_metrics::RealtimeSnapshot,
    service_config_api as s,
    usage_admin_api::UsageAdminFuture,
};
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request},
    response::Response,
};
use serde_json::{Value, json};
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};
use tower::ServiceExt;
struct Fixture;
impl p::ProviderAdminIdentityResolver for Fixture {
    fn resolve<'a>(&'a self, _: &'a HeaderMap, _: &'a str) -> p::ProviderAdminAuthFuture<'a> {
        Box::pin(async {
            Ok(p::ProviderAdminUser::from_resolved_identity(
                "user-security".into(),
                "tenant-security".into(),
                vec!["user".into()],
                true,
            ))
        })
    }
}
impl p::ProviderAdminProviders for Fixture {
    fn list<'a>(&'a self, _: &'a str, _: bool) -> p::ProviderAdminFuture<'a, Vec<Value>> {
        panic!("denied request reached providers")
    }
    fn get<'a>(&'a self, _: &'a str, _: &'a str) -> p::ProviderAdminFuture<'a, Option<Value>> {
        panic!("denied request reached providers")
    }
    fn create<'a>(&'a self, _: &'a str, _: CreateProvider) -> p::ProviderAdminFuture<'a, Value> {
        panic!("denied request reached providers")
    }
    fn update<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: UpdateProvider,
    ) -> p::ProviderAdminFuture<'a, Option<Value>> {
        panic!("denied request reached providers")
    }
    fn delete<'a>(&'a self, _: &'a str, _: &'a str) -> p::ProviderAdminFuture<'a, bool> {
        panic!("denied request reached providers")
    }
    fn test<'a>(&'a self, _: &'a str, _: &'a str) -> p::ProviderAdminFuture<'a, Value> {
        panic!("denied request reached providers")
    }
}
impl p::ProviderAdminEffects for Fixture {
    fn invalidate(&self, _: &str, _: &str) {
        panic!("denied request mutated state")
    }
    fn begin_probe(&self, _: &str, _: &str) -> Option<String> {
        panic!("denied request began probe")
    }
    fn record_probe(&self, _: &str, _: &str, _: &Value, _: Option<&str>) {
        panic!("denied request recorded probe")
    }
    fn audit<'a>(
        &'a self,
        _: &'a p::ProviderAdminRequestContext,
        _: &'a p::ProviderAdminUser,
        _: p::ProviderAdminChange,
    ) -> p::ProviderAdminFuture<'a, ()> {
        panic!("denied request audited mutation")
    }
    fn publish<'a>(
        &'a self,
        _: &'a p::ProviderAdminRequestContext,
        _: &'a str,
        _: &'a str,
        _: &'a str,
    ) -> p::ProviderAdminFuture<'a, ()> {
        panic!("denied request published mutation")
    }
}
macro_rules! cancellation {
    ($trait:path) => {
        impl $trait for Fixture {
            fn cancelled(&self) -> Pin<Box<dyn Future<Output = Response> + Send + '_>> {
                panic!("unexpected cancellation")
            }
        }
    };
}
cancellation!(p::ProviderAdminCancellation);
cancellation!(m::MetricsAdminCancellation);
cancellation!(s::ServiceConfigCancellation);
impl m::MetricsAdminIdentityResolver for Fixture {
    fn resolve<'a>(&'a self, _: &'a HeaderMap, _: &'a str) -> m::MetricsAdminAuthFuture<'a> {
        Box::pin(async {
            Ok(m::MetricsAdminIdentity::from_resolved_identity(
                "user-security".into(),
                "tenant-security".into(),
                vec!["no_dashboard".into()],
                vec![],
                true,
            ))
        })
    }
}
impl d::DashboardRealtimeSource for Fixture {
    fn snapshot(&self) -> UsageAdminFuture<'_, RealtimeSnapshot> {
        panic!("denied request read metrics")
    }
    fn user_metrics<'a>(&'a self, _: &'a str, _: &'a str) -> UsageAdminFuture<'a, Value> {
        panic!("denied request read metrics")
    }
}
impl m::MetricsSummarySource for Fixture {
    fn today_summary(&self) -> m::MetricsAdminFuture<'_, Value> {
        panic!("denied request read summary")
    }
}
impl c::McpRegistryIdentityResolver for Fixture {
    fn resolve<'a>(
        &'a self,
        _: &'a HeaderMap,
        _: &'a str,
    ) -> s::ServiceConfigFuture<'a, c::McpRegistryIdentity> {
        Box::pin(async {
            Ok(c::McpRegistryIdentity {
                user_id: "user-security".into(),
                tenant_id: "tenant-security".into(),
                authenticated: true,
                roles: vec!["user".into()],
                permissions: vec![],
            })
        })
    }
}
impl ContentAccess for Fixture {
    fn resolve<'a>(&'a self, _: &'a HeaderMap) -> ContentFuture<'a, ContentIdentity> {
        Box::pin(async {
            Ok(ContentIdentity {
                user_id: "user-security".into(),
                tenant_id: "tenant-security".into(),
                roles: vec!["user".into()],
                is_authenticated: true,
                ..Default::default()
            })
        })
    }
    fn require_capability<'a>(
        &'a self,
        user: &'a ContentIdentity,
        name: &'a str,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            let cap: Capability = serde_json::from_value(json!(name)).unwrap();
            if check_capability(
                &Rbac::new(HashMap::new()),
                &user.roles,
                &user.permissions,
                cap,
            )
            .allowed()
            {
                Ok(())
            } else {
                Err(ContentError::new(403, "Permission denied"))
            }
        })
    }
    fn rate_limit<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
        _: Option<&'a str>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn authorize_session<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
    ) -> ContentFuture<'a, Value> {
        panic!("unexpected session access")
    }
    fn authorize_artifact_source<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a Artifact,
    ) -> ContentFuture<'a, ()> {
        panic!("unexpected artifact access")
    }
    fn require_vision_model<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
        _: Option<&'a str>,
    ) -> ContentFuture<'a, ()> {
        panic!("unexpected model access")
    }
}
fn app() -> Router {
    let fixture = Arc::new(Fixture);
    let provider = p::router(
        fixture.clone(),
        Arc::new(p::ProviderAdminApi::new(
            Some(fixture.clone()),
            None,
            fixture.clone(),
        )),
        fixture.clone(),
    );
    let dashboard = d::router(
        fixture.clone(),
        Arc::new(d::DashboardApi::new(d::DashboardDependencies {
            realtime: fixture.clone(),
            metrics: fixture.clone(),
            usage: Default::default(),
            redis: None,
            clock: Arc::new(m::SystemMetricsAdminClock),
            rbac: Rbac::new(HashMap::new()),
            denied: None,
        })),
        fixture.clone(),
    );
    let mcp = c::router(
        Arc::new(c::McpRegistryApi::new(c::McpRegistryDependencies {
            identity: fixture.clone(),
            rbac: Rbac::new(HashMap::new()),
            enabled: true,
            repository: None,
            audit: None,
            discovery: None,
            destination: Arc::new(c::NativeMcpDestinationValidator),
        })),
        fixture.clone(),
    );
    let pool = || {
        sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused")
            .unwrap()
    };
    let skills = ai_platform_gateway::migration_content::skills::router(Arc::new(SkillsApi::new(
        SkillRepository {
            database: ContentDatabase::new(pool(), pool()).unwrap(),
        },
        fixture,
    )));
    provider
        .reset_fallback()
        .merge(dashboard.reset_fallback())
        .merge(mcp.reset_fallback())
        .nest("/api/v1", skills.reset_fallback())
}
async fn assert_denied(method: &str, path: &str, content_type: &str, body: &str) {
    let response = app()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", content_type)
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 403, "{method} {path}");
}
#[tokio::test]
async fn skills_list_and_get_require_skill_read() {
    for path in ["/api/v1/skills", "/api/v1/skills/secure-skill"] {
        assert_denied("GET", path, "application/json", "").await;
    }
}
#[tokio::test]
async fn skills_upload_requires_skill_write() {
    assert_denied("POST","/api/v1/skills/upload","multipart/form-data; boundary=fixture","--fixture\r\nContent-Disposition: form-data; name=\"file\"; filename=\"SKILL.md\"\r\nContent-Type: text/markdown\r\n\r\n---\nname: secure-skill\ndescription: Test skill\n---\n# Secure Skill\n\r\n--fixture--\r\n").await;
}
#[tokio::test]
async fn mcp_inventory_requires_mcp_read() {
    for path in [
        "/api/v1/assistant/mcp/servers",
        "/api/v1/assistant/mcp/tools",
    ] {
        assert_denied("GET", path, "application/json", "").await;
    }
}
#[tokio::test]
async fn provider_inventory_template_and_get_require_provider_read() {
    for path in [
        "/api/v1/providers",
        "/api/v1/provider-templates",
        "/api/v1/providers/prov-security",
    ] {
        assert_denied("GET", path, "application/json", "").await;
    }
}
#[tokio::test]
async fn provider_create_requires_provider_write() {
    assert_denied(
        "POST",
        "/api/v1/providers",
        "application/json",
        r#"{"provider_id":"prov-security","display_name":"Provider Security"}"#,
    )
    .await;
}
#[tokio::test]
async fn dashboard_timeseries_summary_realtime_require_metrics() {
    for path in [
        "/api/v1/dashboard/timeseries/requests",
        "/api/v1/dashboard/summary",
        "/api/v1/dashboard/realtime",
    ] {
        assert_denied("GET", path, "application/json", "").await;
    }
}
struct WebSocketFixture {
    allowed: bool,
}
impl m::MetricsAdminIdentityResolver for WebSocketFixture {
    fn resolve<'a>(&'a self, _: &'a HeaderMap, _: &'a str) -> m::MetricsAdminAuthFuture<'a> {
        Box::pin(async move { Ok(websocket_identity(self.allowed)) })
    }
}
impl d::DashboardRealtimeSource for WebSocketFixture {
    fn snapshot(&self) -> UsageAdminFuture<'_, RealtimeSnapshot> {
        assert!(self.allowed, "denied websocket read metrics");
        Box::pin(async { Ok(RealtimeSnapshot::default()) })
    }
    fn user_metrics<'a>(&'a self, _: &'a str, _: &'a str) -> UsageAdminFuture<'a, Value> {
        panic!("websocket requested unrelated user metrics")
    }
}
fn websocket_identity(allowed: bool) -> m::MetricsAdminIdentity {
    m::MetricsAdminIdentity::from_resolved_identity(
        if allowed {
            "platform-admin"
        } else {
            "user-security"
        }
        .into(),
        "platform".into(),
        vec![
            if allowed {
                "platform_admin"
            } else {
                "no_dashboard"
            }
            .into(),
        ],
        if allowed {
            vec!["console:metrics:view".into()]
        } else {
            vec![]
        },
        true,
    )
}
fn websocket_api(allowed: bool) -> Arc<d::DashboardApi> {
    websocket_api_with_config(allowed, Default::default())
}
fn websocket_api_with_config(
    allowed: bool,
    config: d::DashboardWebSocketConfig,
) -> Arc<d::DashboardApi> {
    Arc::new(
        d::DashboardApi::new(d::DashboardDependencies {
            realtime: Arc::new(WebSocketFixture { allowed }),
            metrics: Arc::new(Fixture),
            usage: Default::default(),
            redis: None,
            clock: Arc::new(m::SystemMetricsAdminClock),
            rbac: Rbac::new(HashMap::new()),
            denied: None,
        })
        .with_websocket_config(config),
    )
}
#[tokio::test]
async fn dashboard_websocket_accepts_metrics_permission_and_connects() {
    use futures_util::{SinkExt, StreamExt};
    let api = websocket_api(true);
    assert!(
        api.authorize_websocket(Ok(&websocket_identity(true)), &Default::default())
            .is_ok()
    );
    let app = d::router(
        Arc::new(WebSocketFixture { allowed: true }),
        api,
        Arc::new(Fixture),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (mut socket, response) =
        tokio_tungstenite::connect_async(format!("ws://{address}/api/v1/dashboard/ws"))
            .await
            .unwrap();
    assert_eq!(response.status().as_u16(), 101);
    let message = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let payload: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
    assert_eq!(payload["type"], "metrics");
    assert_eq!(payload["tenant_id"], "platform");
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text("ping".into()))
        .await
        .unwrap();
    let pong = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(pong.to_text().unwrap()).unwrap(),
        json!({"type":"pong"})
    );
    let _ = socket.close(None).await;
    server.abort();
}
#[tokio::test]
async fn dashboard_websocket_denies_without_metrics_permission_before_upgrade() {
    let api = websocket_api(false);
    let rejection = api
        .authorize_websocket(Ok(&websocket_identity(false)), &Default::default())
        .unwrap_err();
    assert_eq!(rejection.code, 4003);
    assert_eq!(rejection.reason, "Metrics permission required");
    let app = d::router(
        Arc::new(WebSocketFixture { allowed: false }),
        api,
        Arc::new(Fixture),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let result =
        tokio_tungstenite::connect_async(format!("ws://{address}/api/v1/dashboard/ws")).await;
    server.abort();
    match result {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status().as_u16(), 403)
        }
        _ => panic!("unauthorized websocket must be denied before upgrade"),
    }
}

#[tokio::test]
async fn dashboard_websocket_session_limit_survives_upgrade_and_releases_on_disconnect() {
    use futures_util::StreamExt;
    let api = websocket_api_with_config(
        true,
        d::DashboardWebSocketConfig::new(1, 1000, 10_000).unwrap(),
    );
    let app = d::router(
        Arc::new(WebSocketFixture { allowed: true }),
        api.clone(),
        Arc::new(Fixture),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/api/v1/dashboard/ws",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket.next().await.unwrap().unwrap();
    assert_eq!(api.available_websocket_sessions(), 0);
    match tokio_tungstenite::connect_async(&url).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status().as_u16(), 503);
            assert_eq!(response.headers()["retry-after"], "1");
        }
        _ => panic!("overloaded WebSocket must be denied before upgrade"),
    }
    socket.close(None).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while api.available_websocket_sessions() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (mut replacement, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    replacement.close(None).await.unwrap();
    server.abort();
}
#[tokio::test]
async fn dashboard_websocket_idle_peer_releases_session_budget() {
    use futures_util::StreamExt;
    let api =
        websocket_api_with_config(true, d::DashboardWebSocketConfig::new(1, 1000, 30).unwrap());
    let app = d::router(
        Arc::new(WebSocketFixture { allowed: true }),
        api.clone(),
        Arc::new(Fixture),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/api/v1/dashboard/ws",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    assert!(socket.next().await.unwrap().is_ok());
    let terminal = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .unwrap();
    assert!(
        terminal.is_none()
            || terminal.is_some_and(|result| result.is_err()
                || matches!(
                    result,
                    Ok(tokio_tungstenite::tungstenite::Message::Close(_))
                ))
    );
    assert_eq!(api.available_websocket_sessions(), 1);
    server.abort();
}
