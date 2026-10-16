//! Public/private readiness projection from services/health_contract.py.
use super::{middleware::DrainState, resources::ConnectionResources};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::Duration,
};
#[derive(Clone, Default)]
pub struct ReadinessConfig {
    pub auth_config_ready: bool,
    pub model_plane_ready: bool,
    pub runtime_url: Option<String>,
    pub capability_worker_url: Option<String>,
    pub knowledge_worker_url: Option<String>,
    pub qdrant_url: Option<String>,
    pub knowledge_service_url: Option<String>,
    pub image_worker_status: Option<String>,
}
pub struct ReadinessState {
    pub resources: Arc<ConnectionResources>,
    pub config: RwLock<ReadinessConfig>,
    pub drain: Arc<DrainState>,
    pub client: reqwest::Client,
    pub last_snapshot: RwLock<Value>,
}
impl ReadinessState {
    pub fn new(
        resources: Arc<ConnectionResources>,
        config: ReadinessConfig,
        drain: Arc<DrainState>,
    ) -> Result<Self, super::lifecycle::HostError> {
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| super::lifecycle::HostError::Configuration)?;
        Ok(Self {
            resources,
            config: RwLock::new(config),
            drain,
            client,
            last_snapshot: RwLock::new(Value::Null),
        })
    }
    async fn probe(
        &self,
        base: Option<&str>,
        path: &str,
        expected: Option<&str>,
        require_core: bool,
    ) -> String {
        probe_http_service(&self.client, base, path, expected, require_core).await
    }
    pub async fn snapshot(&self) -> Value {
        let config = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let database = async {
            if let Some(pool) = &self.resources.database {
                matches!(
                    tokio::time::timeout(
                        Duration::from_secs(1),
                        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(pool)
                    )
                    .await,
                    Ok(Ok(1))
                )
            } else {
                false
            }
        };
        let redis = async {
            if let Some(connection) = &self.resources.redis {
                let mut connection = connection.clone();
                matches!(
                    tokio::time::timeout(
                        Duration::from_secs(1),
                        redis::cmd("PING").query_async::<String>(&mut connection)
                    )
                    .await,
                    Ok(Ok(_))
                )
            } else {
                false
            }
        };
        let (database, redis, runtime, capability, worker, qdrant, knowledge) = tokio::join!(
            database,
            redis,
            self.probe(
                config.runtime_url.as_deref(),
                "/health/ready",
                Some("ready"),
                true
            ),
            self.probe(
                config.capability_worker_url.as_deref(),
                "/health/ready",
                Some("ready"),
                true
            ),
            self.probe(
                config.knowledge_worker_url.as_deref(),
                "/health/ready",
                Some("ready"),
                false
            ),
            self.probe(config.qdrant_url.as_deref(), "/healthz", None, false),
            self.probe(
                config.knowledge_service_url.as_deref(),
                "/health/ready",
                Some("ready"),
                false
            )
        );
        let mut core = BTreeMap::<String, String>::new();
        core.insert(
            "auth_config".into(),
            if config.auth_config_ready {
                "healthy"
            } else {
                "misconfigured"
            }
            .into(),
        );
        core.insert(
            "database".into(),
            if database {
                "healthy"
            } else if self.resources.database_enabled {
                "unavailable"
            } else {
                "disabled"
            }
            .into(),
        );
        core.insert(
            "redis".into(),
            if redis {
                "healthy"
            } else if self.resources.redis_enabled {
                "unavailable"
            } else {
                "disabled"
            }
            .into(),
        );
        core.insert("agent_runtime".into(), runtime);
        core.insert(
            "model_plane".into(),
            if config.model_plane_ready {
                "healthy"
            } else {
                "unavailable"
            }
            .into(),
        );
        core.insert(
            "traffic_acceptance".into(),
            if self.drain.draining() {
                "unavailable"
            } else {
                "healthy"
            }
            .into(),
        );
        let capabilities = BTreeMap::from([
            ("knowledge_service", knowledge),
            ("knowledge_worker", worker),
            ("qdrant", qdrant),
            ("capability_worker", capability),
            (
                "image_worker",
                config
                    .image_worker_status
                    .unwrap_or_else(|| "unavailable".into()),
            ),
        ]);
        let ready = core.values().all(|v| v == "healthy");
        let degraded = capabilities
            .values()
            .any(|v| !matches!(v.as_str(), "healthy" | "not_configured" | "disabled"));
        let snapshot = json!({"status":if ready{"ready"}else{"not_ready"},"core_ready":ready,"degraded":degraded,"core":core,"capabilities":capabilities});
        *self
            .last_snapshot
            .write()
            .unwrap_or_else(|e| e.into_inner()) = snapshot.clone();
        snapshot
    }
}
async fn ready(State(state): State<Arc<ReadinessState>>) -> Response {
    let snapshot = state.snapshot().await;
    let ready = snapshot["core_ready"] == true;
    (if ready{StatusCode::OK}else{StatusCode::SERVICE_UNAVAILABLE},Json(json!({"status":if ready{"ready"}else{"not_ready"},"checks":{"core":if ready{"healthy"}else{"unavailable"}}}))).into_response()
}
pub fn router(state: Arc<ReadinessState>) -> Router {
    Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"status":"healthy","version":"2.0.0"})) }),
        )
        .route(
            "/health/live",
            get(|| async { Json(json!({"status":"alive"})) }),
        )
        .route("/health/ready", get(ready))
        .with_state(state)
}

/// Probe the same explicit health contract used by live Gateway readiness.
pub async fn probe_http_service(
    client: &reqwest::Client,
    base: Option<&str>,
    path: &str,
    expected: Option<&str>,
    require_core: bool,
) -> String {
    let Some(base) = base.filter(|v| !v.is_empty()) else {
        return "not_configured".into();
    };
    let url = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let response = match client
        .get(url)
        .header("x-request-id", format!("svc-{}", uuid::Uuid::new_v4()))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return if e.is_timeout() {
                "error: TimeoutException".into()
            } else {
                "error: RequestError".into()
            };
        }
    };
    if !response.status().is_success() {
        return format!("status_{}", response.status().as_u16());
    }
    if let Some(expected) = expected {
        match response.json::<Value>().await {
            Ok(v)
                if v.is_object()
                    && v["status"] == expected
                    && (!require_core || v["core_ready"] == true) => {}
            _ => return "schema_mismatch".into(),
        }
    }
    "healthy".into()
}
