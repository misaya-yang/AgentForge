use super::*;
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
};

struct Clock {
    utc: AtomicI64,
    mono: AtomicU64,
}
impl Clock {
    fn new() -> Self {
        Self {
            utc: AtomicI64::new(0),
            mono: AtomicU64::new(0),
        }
    }
    fn advance(&self, seconds: u64) {
        self.utc.fetch_add(seconds as i64, Ordering::SeqCst);
        self.mono.fetch_add(seconds, Ordering::SeqCst);
    }
}
impl SessionClock for Clock {
    fn utc_now(&self) -> NaiveDateTime {
        DateTime::from_timestamp(1_791_763_200 + self.utc.load(Ordering::SeqCst), 0)
            .unwrap()
            .naive_utc()
    }
    fn monotonic(&self) -> Duration {
        Duration::from_secs(self.mono.load(Ordering::SeqCst))
    }
}
struct Ids;
impl SessionIds for Ids {
    fn new_id(&self) -> String {
        "new-session".into()
    }
}
#[derive(Default)]
struct Store {
    rows: StdMutex<HashMap<String, Value>>,
    calls: StdMutex<Vec<String>>,
    fail: AtomicBool,
    mutation_result: AtomicBool,
    blocked_get: AtomicBool,
}
impl Store {
    fn new() -> Self {
        Self {
            mutation_result: AtomicBool::new(true),
            ..Default::default()
        }
    }
    fn record(&self, value: impl Into<String>) -> Result<(), SessionError> {
        self.calls.lock().unwrap().push(value.into());
        if self.fail.load(Ordering::SeqCst) {
            Err(SessionError::StorageUnavailable)
        } else {
            Ok(())
        }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}
impl SessionStorage for Store {
    fn get<'a>(&'a self, id: &'a str) -> SessionFuture<'a, Option<Value>> {
        Box::pin(async move {
            self.record(format!("get:{id}"))?;
            if self.blocked_get.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(self.rows.lock().unwrap().get(id).cloned())
        })
    }
    fn save(&self, session: Value) -> SessionFuture<'_, ()> {
        Box::pin(async move {
            self.record("save")?;
            self.rows
                .lock()
                .unwrap()
                .insert(session["session_id"].as_str().unwrap().into(), session);
            Ok(())
        })
    }
    fn create_if_absent(&self, session: Value) -> SessionFuture<'_, bool> {
        Box::pin(async move {
            self.record("create")?;
            let mut rows = self.rows.lock().unwrap();
            let id = session["session_id"].as_str().unwrap().to_owned();
            if rows.contains_key(&id) {
                Ok(false)
            } else {
                rows.insert(id, session);
                Ok(true)
            }
        })
    }
    fn append_message<'a>(
        &'a self,
        id: &'a str,
        message: SessionMessage,
        metadata: Option<Map<String, Value>>,
    ) -> SessionFuture<'a, bool> {
        Box::pin(async move {
            self.record(format!("append:{id}"))?;
            if !self.mutation_result.load(Ordering::SeqCst) {
                return Ok(false);
            }
            let mut rows = self.rows.lock().unwrap();
            let Some(row) = rows.get_mut(id) else {
                return Ok(false);
            };
            row["history"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::to_value(message).unwrap());
            if let Some(metadata) = metadata {
                row["metadata"].as_object_mut().unwrap().extend(metadata);
            }
            Ok(true)
        })
    }
    fn delete<'a>(&'a self, id: &'a str) -> SessionFuture<'a, bool> {
        Box::pin(async move {
            self.record("delete")?;
            Ok(self.rows.lock().unwrap().remove(id).is_some())
        })
    }
    fn extend_expiry<'a>(&'a self, id: &'a str, expiry: SessionTimestamp) -> SessionFuture<'a, ()> {
        Box::pin(async move {
            self.record(format!("extend:{id}"))?;
            if let Some(row) = self.rows.lock().unwrap().get_mut(id) {
                row["expires_at"] = json!(expiry);
            }
            Ok(())
        })
    }
    fn update_metadata<'a>(
        &'a self,
        id: &'a str,
        patch: Map<String, Value>,
    ) -> SessionFuture<'a, bool> {
        Box::pin(async move {
            self.record("metadata")?;
            if !self.mutation_result.load(Ordering::SeqCst) {
                return Ok(false);
            }
            let mut rows = self.rows.lock().unwrap();
            let Some(row) = rows.get_mut(id) else {
                return Ok(false);
            };
            row["metadata"].as_object_mut().unwrap().extend(patch);
            Ok(true)
        })
    }
    fn update_config<'a>(
        &'a self,
        id: &'a str,
        patch: Map<String, Value>,
    ) -> SessionFuture<'a, bool> {
        Box::pin(async move {
            self.record("config")?;
            if !self.mutation_result.load(Ordering::SeqCst) {
                return Ok(false);
            }
            let mut rows = self.rows.lock().unwrap();
            let Some(row) = rows.get_mut(id) else {
                return Ok(false);
            };
            row["config"].as_object_mut().unwrap().extend(patch);
            Ok(true)
        })
    }
}
fn fixture() -> (DatabaseSessionManager, Arc<Store>, Arc<Clock>) {
    let store = Arc::new(Store::new());
    let clock = Arc::new(Clock::new());
    let manager = DatabaseSessionManager::with_runtime(
        store.clone(),
        DatabaseSessionConfig {
            cache_ttl: 10,
            memory_cache_max_entries: 2,
            default_session_ttl: 60,
            ..Default::default()
        },
        clock.clone(),
        Arc::new(Ids),
    );
    for id in ["s1", "s2", "s3"] {
        store.rows.lock().unwrap().insert(id.into(),json!({"session_id":id,"user_id":"user-a","tenant_id":"tenant-a","service_id":"service-a","created_at":"2026-10-12T00:00:00","updated_at":"2026-10-12T00:00:00","metadata":{"title":"Kept title","nested":{"x":1}},"config":{"old":1},"history":[{"role":"user","content":"one"},{"role":"assistant","content":{"text":"二"}},{"role":"user","content":["three"]}],"status":"active"}));
    }
    (manager, store, clock)
}

#[tokio::test]
async fn projected_history_preserves_python_slicing_cold_and_cached() {
    let (manager, store, _) = fixture();
    for limit in [1, 2, 50, 0, -1, -2, -3, -50, i64::MIN, i64::MAX] {
        let expected =
            history_slice(&manager.get("s1").await.unwrap().unwrap().history, limit).to_vec();
        assert_eq!(manager.history("s1", limit).await.unwrap(), expected);
        manager.invalidate("s1").await;
        assert_eq!(manager.history("s1", limit).await.unwrap(), expected);
    }
    assert!(manager.history("missing", 2).await.unwrap().is_empty());
    assert!(store.calls().iter().all(|call| call.starts_with("get:")));
}
#[tokio::test]
async fn snapshots_and_history_results_do_not_alias_cached_record() {
    let (manager, _, _) = fixture();
    let mut result = manager.history("s1", 2).await.unwrap();
    result[0].content = json!("changed");
    let mut snapshot = manager.get("s1").await.unwrap().unwrap();
    snapshot.history.clear();
    snapshot.metadata.as_mut().unwrap().clear();
    let untouched = manager.get("s1").await.unwrap().unwrap();
    assert_eq!(untouched.history.len(), 3);
    assert_eq!(untouched.history[1].content, json!({"text":"二"}));
    assert_eq!(untouched.metadata.unwrap()["title"], "Kept title");
}
#[tokio::test]
async fn cached_projection_touches_lru_without_renewing_cache_deadline() {
    let (manager, store, clock) = fixture();
    manager.get("s1").await.unwrap();
    manager.get("s2").await.unwrap();
    clock.advance(9);
    manager.history("s1", 1).await.unwrap();
    manager.get("s3").await.unwrap();
    assert_eq!(
        manager
            .cache
            .lock()
            .await
            .records
            .order
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["s1", "s3"]
    );
    clock.advance(1);
    manager.history("s1", 1).await.unwrap();
    assert_eq!(
        store
            .calls()
            .iter()
            .filter(|s| s.as_str() == "get:s1")
            .count(),
        2
    );
}
#[tokio::test]
async fn expired_durable_session_renews_only_on_cache_miss() {
    let (manager, store, clock) = fixture();
    store.rows.lock().unwrap().get_mut("s1").unwrap()["expires_at"] = json!("2020-01-01T00:00:00");
    manager.history("s1", 1).await.unwrap();
    assert_eq!(store.calls(), vec!["get:s1", "extend:s1"]);
    let expiry = manager.get("s1").await.unwrap().unwrap().expires_at;
    clock.advance(1);
    manager.history("s1", 1).await.unwrap();
    assert_eq!(store.calls().len(), 2);
    assert_eq!(manager.get("s1").await.unwrap().unwrap().expires_at, expiry);
}
#[tokio::test]
async fn storage_failure_and_malformed_record_do_not_populate_cache() {
    let (manager, store, _) = fixture();
    store.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        manager.history("s1", 1).await,
        Err(SessionError::StorageUnavailable)
    );
    assert!(manager.cache.lock().await.records.sessions.is_empty());
    store.fail.store(false, Ordering::SeqCst);
    store.rows.lock().unwrap().get_mut("s1").unwrap()["history"] = json!(42);
    assert!(matches!(
        manager.history("s1", 1).await,
        Err(SessionError::InvalidData(_))
    ));
    assert!(manager.cache.lock().await.records.sessions.is_empty());
}
#[tokio::test]
async fn cancelled_cold_projection_does_not_populate_cache_or_start_mutation() {
    let (manager, store, _) = fixture();
    store.blocked_get.store(true, Ordering::SeqCst);
    {
        let future = manager.update_config("s1", Map::from_iter([("new".into(), json!(2))]));
        tokio::pin!(future);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                future.as_mut().poll(cx).is_pending()
            ))
            .await
        );
    }
    assert!(manager.cache.lock().await.records.sessions.is_empty());
    assert_eq!(store.calls(), vec!["get:s1"]);
}
#[tokio::test]
async fn user_append_retains_existing_title_and_invalidates_on_success() {
    let (manager, store, _) = fixture();
    manager.get("s1").await.unwrap();
    assert!(
        manager
            .append_message("s1", "user", json!("new prompt"), None)
            .await
            .unwrap()
    );
    assert!(manager.cache.lock().await.records.sessions.is_empty());
    assert_eq!(store.calls(), vec!["get:s1", "append:s1"]);
    let row = manager.get("s1").await.unwrap().unwrap();
    assert_eq!(row.history.len(), 4);
    assert_eq!(row.metadata.unwrap()["title"], "Kept title");
}
#[tokio::test]
async fn user_append_generates_missing_title_and_keeps_empty_title_error() {
    let (manager, store, _) = fixture();
    store.rows.lock().unwrap().get_mut("s1").unwrap()["metadata"] = json!({"title":""});
    assert_eq!(
        manager
            .append_message("s1", "user", json!("  \n  "), None)
            .await,
        Err(SessionError::EmptyTitleLine)
    );
    assert_eq!(store.calls(), vec!["get:s1"]);
    assert!(
        manager
            .append_message("s1", "user", json!("  First line\nsecond"), None)
            .await
            .unwrap()
    );
    assert_eq!(
        manager.get("s1").await.unwrap().unwrap().metadata.unwrap()["title"],
        "First line"
    );
}
#[tokio::test]
async fn append_false_and_storage_error_keep_existing_cache() {
    let (manager, store, _) = fixture();
    manager.get("s1").await.unwrap();
    store.mutation_result.store(false, Ordering::SeqCst);
    assert!(
        !manager
            .append_message("s1", "user", json!("new"), None)
            .await
            .unwrap()
    );
    store.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        manager
            .append_message("s1", "user", json!("new"), None)
            .await,
        Err(SessionError::StorageUnavailable)
    );
    assert!(
        manager
            .cache
            .lock()
            .await
            .records
            .sessions
            .contains_key("s1")
    );
}
#[tokio::test]
async fn assistant_append_does_not_introduce_session_read() {
    let (manager, store, _) = fixture();
    assert!(
        manager
            .append_message("s1", "assistant", json!("reply"), None)
            .await
            .unwrap()
    );
    assert_eq!(store.calls(), vec!["append:s1"]);
}
#[tokio::test]
async fn map_updates_preserve_missing_false_error_size_and_invalidation() {
    let (manager, store, _) = fixture();
    let patch = Map::from_iter([("new".into(), json!(2))]);
    assert!(
        !manager
            .update_config("missing", patch.clone())
            .await
            .unwrap()
    );
    manager.get("s1").await.unwrap();
    store.mutation_result.store(false, Ordering::SeqCst);
    assert!(!manager.update_config("s1", patch.clone()).await.unwrap());
    assert!(
        manager
            .cache
            .lock()
            .await
            .records
            .sessions
            .contains_key("s1")
    );
    store.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        manager.update_metadata("s1", patch.clone()).await,
        Err(SessionError::StorageUnavailable)
    );
    store.fail.store(false, Ordering::SeqCst);
    store.mutation_result.store(true, Ordering::SeqCst);
    let before = store.calls().len();
    assert!(
        !manager
            .update_metadata(
                "s1",
                Map::from_iter([("large".into(), json!("x".repeat(1_048_576)))])
            )
            .await
            .unwrap()
    );
    assert_eq!(store.calls().len(), before);
    assert!(manager.update_metadata("s1", patch.clone()).await.unwrap());
    assert!(manager.cache.lock().await.records.sessions.is_empty());
    assert!(manager.update_config("s1", patch).await.unwrap());
    assert!(manager.cache.lock().await.records.sessions.is_empty());
    let row = manager.get("s1").await.unwrap().unwrap();
    assert_eq!(
        row.config.unwrap(),
        Map::from_iter([("old".into(), json!(1)), ("new".into(), json!(2))])
    );
    assert_eq!(row.history.len(), 3);
}
#[tokio::test]
async fn get_or_create_retains_tenant_user_and_service_fences() {
    let (manager, _, _) = fixture();
    for (user, tenant, service) in [
        ("other", "tenant-a", "service-a"),
        ("user-a", "other", "service-a"),
        ("user-a", "tenant-a", "other"),
    ] {
        assert!(matches!(
            manager
                .get_or_create(GetOrCreateSession {
                    session_id: Some("s1".into()),
                    user_id: user.into(),
                    tenant_id: tenant.into(),
                    service_id: Some(service.into()),
                    ttl: None
                })
                .await,
            Err(SessionError::PermissionDenied(_))
        ));
    }
    let session = manager
        .get_or_create(GetOrCreateSession {
            session_id: Some("s1".into()),
            user_id: "user-a".into(),
            tenant_id: "tenant-a".into(),
            service_id: Some("service-a".into()),
            ttl: None,
        })
        .await
        .unwrap();
    assert_eq!(session.session_id, "s1");
    assert_eq!(session.history.len(), 3);
}

#[tokio::test]
async fn projected_cache_hit_borrows_the_original_history_allocation() {
    let (manager, _, _) = fixture();
    manager.get("s1").await.unwrap();
    let original = manager.cache.lock().await.records.sessions["s1"]
        .history
        .as_ptr() as usize;
    let projected = manager
        .get_projected("s1", |session| session.history.as_ptr() as usize)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(projected, original);
}

#[tokio::test]
async fn owner_projection_is_small_independent_and_preserves_full_get() {
    let (manager, store, _) = fixture();
    let mut owner = manager.get_owner("s1").await.unwrap().unwrap();
    assert_eq!(
        owner,
        SessionOwner {
            user_id: "user-a".into(),
            tenant_id: "tenant-a".into()
        }
    );
    owner.user_id.clear();
    owner.tenant_id.clear();
    let full = manager.get("s1").await.unwrap().unwrap();
    assert_eq!(full.user_id, "user-a");
    assert_eq!(full.tenant_id, "tenant-a");
    assert_eq!(full.service_id.as_deref(), Some("service-a"));
    assert_eq!(full.history.len(), 3);
    assert_eq!(store.calls(), vec!["get:s1"]);
}

#[tokio::test]
async fn owner_projection_touches_lru_but_not_cache_deadline() {
    let (manager, store, clock) = fixture();
    manager.get_owner("s1").await.unwrap();
    manager.get_owner("s2").await.unwrap();
    clock.advance(9);
    manager.get_owner("s1").await.unwrap();
    manager.get_owner("s3").await.unwrap();
    assert_eq!(
        manager
            .cache
            .lock()
            .await
            .records
            .order
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["s1", "s3"]
    );
    clock.advance(1);
    manager.get_owner("s1").await.unwrap();
    assert_eq!(
        store
            .calls()
            .iter()
            .filter(|call| *call == "get:s1")
            .count(),
        2
    );
}

#[tokio::test]
async fn owner_projection_renews_expired_durable_row_only_on_cache_miss() {
    let (manager, store, clock) = fixture();
    store.rows.lock().unwrap().get_mut("s1").unwrap()["expires_at"] = json!("2020-01-01T00:00:00");
    manager.get_owner("s1").await.unwrap();
    assert_eq!(store.calls(), vec!["get:s1", "extend:s1"]);
    clock.advance(1);
    manager.get_owner("s1").await.unwrap();
    assert_eq!(store.calls().len(), 2);
}

#[tokio::test]
async fn owner_projection_missing_malformed_and_storage_error_fail_closed() {
    let (manager, store, _) = fixture();
    assert_eq!(manager.get_owner("missing").await.unwrap(), None);
    store.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        manager.get_owner("s1").await,
        Err(SessionError::StorageUnavailable)
    );
    store.fail.store(false, Ordering::SeqCst);
    store.rows.lock().unwrap().get_mut("s1").unwrap()["history"] = json!(42);
    assert!(matches!(
        manager.get_owner("s1").await,
        Err(SessionError::InvalidData(_))
    ));
    assert!(manager.cache.lock().await.records.sessions.is_empty());
}

#[tokio::test]
async fn cancelling_owner_projection_does_not_populate_cache() {
    let (manager, store, _) = fixture();
    store.blocked_get.store(true, Ordering::SeqCst);
    let mut future = Box::pin(manager.get_owner("s1"));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    drop(future);
    assert!(manager.cache.lock().await.records.sessions.is_empty());
    assert_eq!(store.calls(), vec!["get:s1"]);
}
