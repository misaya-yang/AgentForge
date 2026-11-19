use super::*;
use crate::session_manager::{GetOrCreateSession, SessionManagerConfig};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
struct Clock(AtomicU64);
impl crate::session_manager::SessionClock for Clock {
    fn utc_now(&self) -> chrono::NaiveDateTime {
        chrono::DateTime::from_timestamp(1_791_763_200 + self.0.load(Ordering::SeqCst) as i64, 0)
            .unwrap()
            .naive_utc()
    }
    fn monotonic(&self) -> Duration {
        Duration::from_secs(self.0.load(Ordering::SeqCst))
    }
}
struct Ids;
impl crate::session_manager::SessionIds for Ids {
    fn new_id(&self) -> String {
        "new-session".into()
    }
}

struct Resolver(SessionsIdentity);
impl SessionsIdentityResolver for Resolver {
    fn resolve<'a>(
        &'a self,
        _: &'a HeaderMap,
        _: &'a str,
    ) -> SessionsApiFuture<'a, SessionsIdentity> {
        Box::pin(async { Ok(self.0.clone()) })
    }
}
struct Manager {
    inner: MemorySessionManager,
    calls: Arc<Mutex<Vec<&'static str>>>,
}
impl SessionLifecycle for Manager {
    fn get_or_create(&self, _: GetOrCreateSession) -> SessionFuture<'_, Session> {
        unreachable!()
    }
    fn add_message<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Value,
        _: Option<Map<String, Value>>,
    ) -> SessionFuture<'a, ()> {
        unreachable!()
    }
}
// Deliberately does not override get_owner: downstream implementations retain
// their full-get fallback without acquiring a new mandatory trait method.
impl SessionsApiManager for Manager {
    fn create(&self, _: CreateSession) -> SessionFuture<'_, Session> {
        unreachable!()
    }
    fn get<'a>(&'a self, id: &'a str) -> SessionFuture<'a, Option<Session>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push("get");
            self.inner.get(id).await
        })
    }
    fn list_summaries(&self, _: SessionSummaryQuery) -> SessionFuture<'_, Vec<Value>> {
        unreachable!()
    }
    fn update_metadata<'a>(&'a self, _: &'a str, _: Map<String, Value>) -> SessionFuture<'a, bool> {
        unreachable!()
    }
    fn update_config<'a>(&'a self, _: &'a str, _: Map<String, Value>) -> SessionFuture<'a, bool> {
        unreachable!()
    }
    fn delete<'a>(&'a self, _: &'a str) -> SessionFuture<'a, bool> {
        unreachable!()
    }
    fn history<'a>(&'a self, id: &'a str, limit: i64) -> SessionFuture<'a, Vec<SessionMessage>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push("history");
            Ok(self.inner.history(id, limit).await)
        })
    }
}
struct Cleanup {
    calls: Arc<Mutex<Vec<&'static str>>>,
    deleted: bool,
    fail: bool,
}
impl SessionsRuntimeCleanup for Cleanup {
    fn session_deleted<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
    ) -> SessionsApiFuture<'a, bool> {
        Box::pin(async move {
            self.calls.lock().unwrap().push("deleted");
            if self.fail {
                Err(SessionsApiError::Internal)
            } else {
                Ok(self.deleted)
            }
        })
    }
    fn cleanup_session<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
    ) -> SessionsApiFuture<'a, bool> {
        unreachable!()
    }
}
async fn fixture(
    user: &str,
    tenant: &str,
    deleted: bool,
    fail: bool,
    expired: bool,
) -> (SessionsApi, Arc<Mutex<Vec<&'static str>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let clock = Arc::new(Clock(AtomicU64::new(0)));
    let manager = MemorySessionManager::with_runtime(
        SessionManagerConfig::default(),
        clock.clone(),
        Arc::new(Ids),
    )
    .unwrap();
    manager
        .create(CreateSession {
            session_id: Some("s1".into()),
            user_id: "owner".into(),
            tenant_id: "tenant".into(),
            ttl: expired.then_some(1),
            ..Default::default()
        })
        .await
        .unwrap();
    if expired {
        clock.0.store(2, Ordering::SeqCst);
    }
    let api = SessionsApi::new(
        Arc::new(Resolver(SessionsIdentity::from_resolved_identity(
            user.into(),
            tenant.into(),
        ))),
        Arc::new(Manager {
            inner: manager,
            calls: calls.clone(),
        }),
        Some(Arc::new(Cleanup {
            calls: calls.clone(),
            deleted,
            fail,
        })),
    );
    (api, calls)
}
async fn request(api: SessionsApi, path: &str) -> (u16, Value) {
    let request = Request::builder()
        .uri(path)
        .body(axum::body::Body::empty())
        .unwrap();
    let response = dispatch(State(api), request).await;
    let status = response.status().as_u16();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}
#[tokio::test]
async fn history_fallback_checks_owner_then_deletion_then_history() {
    let (api, calls) = fixture("owner", "tenant", false, false, false).await;
    assert_eq!(
        request(api, "/api/v1/sessions/s1/history?limit=1").await,
        (200, json!([]))
    );
    assert_eq!(*calls.lock().unwrap(), vec!["get", "deleted", "history"]);
}
#[tokio::test]
async fn history_wrong_user_or_tenant_stops_before_deletion_and_history() {
    for (user, tenant) in [("other", "tenant"), ("owner", "other")] {
        let (api, calls) = fixture(user, tenant, false, false, false).await;
        assert_eq!(
            request(api, "/api/v1/sessions/s1/history").await,
            (404, json!({"detail":"session not found"}))
        );
        assert_eq!(*calls.lock().unwrap(), vec!["get"]);
    }
}
#[tokio::test]
async fn history_missing_and_expired_memory_sessions_fail_closed() {
    for (id, expired) in [("missing", false), ("s1", true)] {
        let (api, calls) = fixture("owner", "tenant", false, false, expired).await;
        assert_eq!(
            request(api, &format!("/api/v1/sessions/{id}/history"))
                .await
                .0,
            404
        );
        assert_eq!(*calls.lock().unwrap(), vec!["get"]);
    }
}
#[tokio::test]
async fn history_deleted_or_failed_deletion_check_stops_history() {
    for (deleted, fail, status) in [(true, false, 404), (false, true, 500)] {
        let (api, calls) = fixture("owner", "tenant", deleted, fail, false).await;
        assert_eq!(request(api, "/api/v1/sessions/s1/history").await.0, status);
        assert_eq!(*calls.lock().unwrap(), vec!["get", "deleted"]);
    }
}
#[tokio::test]
async fn history_query_validation_remains_before_ownership() {
    for limit in ["0", "-1", "501", "bad"] {
        let (api, calls) = fixture("other", "tenant", false, false, false).await;
        assert_eq!(
            request(api, &format!("/api/v1/sessions/s1/history?limit={limit}"))
                .await
                .0,
            422
        );
        assert!(calls.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn item_get_retains_full_get_ownership_path_and_wire_shape() {
    let (api, calls) = fixture("owner", "tenant", false, false, false).await;
    let (status, body) = request(api, "/api/v1/sessions/s1").await;
    assert_eq!(status, 200);
    assert_eq!(body["session_id"], "s1");
    assert_eq!(body.as_object().unwrap().len(), 8);
    assert_eq!(body.get("service_id"), Some(&Value::Null));
    assert!(body.get("history").is_none());
    assert_eq!(*calls.lock().unwrap(), vec!["get", "deleted"]);
}

#[tokio::test]
async fn detail_returns_authoritative_nullable_service_binding() {
    for service_id in [Some("dify-service"), Some("other-service"), Some(""), None] {
        let manager = MemorySessionManager::with_runtime(
            SessionManagerConfig::default(),
            Arc::new(Clock(AtomicU64::new(0))),
            Arc::new(Ids),
        )
        .unwrap();
        manager
            .create(CreateSession {
                session_id: Some("s1".into()),
                user_id: "owner".into(),
                tenant_id: "tenant".into(),
                service_id: service_id.map(str::to_owned),
                metadata: Some(
                    serde_json::from_value(json!({"service_id":"spoofed", "title":"kept"}))
                        .unwrap(),
                ),
                ..Default::default()
            })
            .await
            .unwrap();
        let api = SessionsApi::new(
            Arc::new(Resolver(SessionsIdentity::from_resolved_identity(
                "owner".into(),
                "tenant".into(),
            ))),
            Arc::new(manager),
            None,
        );
        let (status, body) = request(api, "/api/v1/sessions/s1").await;
        assert_eq!(status, 200);
        assert_eq!(body["service_id"], json!(service_id));
        assert_eq!(body["metadata"]["service_id"], "spoofed");
        let keys: std::collections::BTreeSet<_> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "session_id",
                "service_id",
                "user_id",
                "tenant_id",
                "created_at",
                "updated_at",
                "metadata",
                "config"
            ]
            .into_iter()
            .collect()
        );
    }
}

#[tokio::test]
async fn detail_service_binding_does_not_relax_ownership_or_deleted_denial() {
    for (user, tenant, deleted) in [
        ("other", "tenant", false),
        ("owner", "other", false),
        ("owner", "tenant", true),
    ] {
        let (api, _) = fixture(user, tenant, deleted, false, false).await;
        let (status, body) = request(api, "/api/v1/sessions/s1").await;
        assert_eq!(status, 404);
        assert_eq!(body, json!({"detail":"session not found"}));
        assert!(body.get("service_id").is_none());
    }
    let (api, _) = fixture("owner", "tenant", false, false, false).await;
    let (status, body) = request(api, "/api/v1/sessions/missing").await;
    assert_eq!(status, 404);
    assert!(body.get("service_id").is_none());
}
