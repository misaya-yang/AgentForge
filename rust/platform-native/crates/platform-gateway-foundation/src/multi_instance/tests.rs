use super::*;
use serde_json::json;
#[test]
fn incarnation_hints_never_replace_full_provenance_tokens() {
    let a = affinity::InstanceIdentity::new("gateway-a").unwrap();
    let b = affinity::InstanceIdentity::new("gateway-a").unwrap();
    let first = a.token();
    assert!(affinity::valid_token(&first));
    assert!(a.owns(&first));
    assert!(!b.owns(&first));
    assert_ne!(a.prefix(), b.prefix());
    assert_ne!(a.token(), a.token());
    assert!(affinity::InstanceIdentity::new("../untrusted").is_err());
    assert!(!affinity::valid_token(&"A".repeat(64)));
}
#[test]
fn state_inventory_and_unit_tests_cannot_retire_single_instance_release() {
    let mut proof = readiness::RetirementEvidence {
        source_sha256: "a".repeat(64),
        instances: 2,
        runtime_executed: false,
        kill9_failover_passed: true,
        lease_fencing_passed: true,
        duplicate_executions: 0,
        all_state_contracts_passed: true,
        measured_rps: 100.0,
        capacity_contract_passed: true,
        adr_approved: true,
    };
    assert!(!proof.permits_retirement());
    proof.runtime_executed = true;
    assert!(proof.permits_retirement());
    proof.duplicate_executions = 1;
    assert!(!proof.permits_retirement());
    proof.duplicate_executions = 0;
    proof.adr_approved = false;
    assert!(!proof.permits_retirement());
    proof.adr_approved = true;
    proof.measured_rps = f64::NAN;
    assert!(!proof.permits_retirement());
}
#[test]
fn legacy_guest_storage_cannot_be_selected_for_ha() {
    let storage = std::sync::Arc::new(
        crate::guest_session_redis::RedisGuestSessionStorage::disconnected_canonical_storage(),
    );
    assert!(
        crate::guest_session_manager::GuestSessionManager::shared(Default::default(), storage)
            .is_err()
    );
}
async fn connection() -> redis::aio::ConnectionManager {
    let file = std::env::var("AGENTFORGE_HA_TEST_REDIS_URL_FILE")
        .expect("explicit disposable Redis URL file required");
    let bytes = ai_platform_service_primitives::workload_identity::secrets::read_file(
        std::path::Path::new(&file),
        true,
    )
    .expect("private fixture URL file");
    let url = std::str::from_utf8(bytes.expose())
        .expect("fixture URL encoding")
        .trim();
    let client = redis::Client::open(url)
        .map_err(|_| Error::Invalid)
        .unwrap();
    redis::aio::ConnectionManager::new(client)
        .await
        .map_err(|_| Error::Unavailable)
        .unwrap()
}
#[tokio::test]
#[ignore = "requires disposable AGENTFORGE_HA_TEST_REDIS_URL_FILE; never use application coordination Redis"]
async fn cross_replica_context_deduplication_and_bounded_shared_window() {
    let redis = connection().await;
    let a = context::ContextStore::new(redis.clone());
    let b = context::ContextStore::new(redis);
    let tenant = uuid::Uuid::new_v4().to_string();
    let value = json!({"tenant":tenant,"tokens":3});
    assert!(
        a.record(&tenant, "session", "request", &value)
            .await
            .unwrap()
    );
    assert!(
        !b.record(&tenant, "session", "request", &value)
            .await
            .unwrap()
    );
    assert_eq!(
        b.record(&tenant, "session", "request", &json!({"different":true}))
            .await
            .unwrap_err(),
        Error::Conflict
    );
    let rows: Vec<serde_json::Value> = b.read().await.unwrap();
    assert_eq!(rows.iter().filter(|row| row["tenant"] == tenant).count(), 1);
    for i in 0..101 {
        a.record(&tenant, "session", &format!("request-{i}"), &json!({"i":i}))
            .await
            .unwrap();
    }
    assert_eq!(b.read::<serde_json::Value>().await.unwrap().len(), 100);
}
#[tokio::test]
#[ignore = "requires disposable AGENTFORGE_HA_TEST_REDIS_URL_FILE"]
async fn cross_replica_rpm_cannot_multiply_by_replica_count() {
    let redis = connection().await;
    let a = crate::rate_limits::QuotaRpmLimiter::shared(redis.clone());
    let b = crate::rate_limits::QuotaRpmLimiter::shared(redis);
    let tenant = uuid::Uuid::new_v4().to_string();
    let mut admitted = 0;
    for i in 0..20 {
        if (if i % 2 == 0 { &a } else { &b })
            .check_shared(&tenant, "user", Some(10))
            .await
            .unwrap()
            .allowed
        {
            admitted += 1
        }
    }
    assert_eq!(admitted, 10);
    assert!(
        b.check_shared(&format!("{tenant}-other"), "user", Some(10))
            .await
            .unwrap()
            .allowed
    );
}
#[tokio::test]
#[ignore = "requires disposable AGENTFORGE_HA_TEST_REDIS_URL_FILE"]
async fn affinity_owner_expiry_replacement_and_old_owner_fence() {
    let redis = connection().await;
    let name = uuid::Uuid::new_v4().to_string();
    let a_id = affinity::InstanceIdentity::new(&name).unwrap();
    let b_id = affinity::InstanceIdentity::new(&name).unwrap();
    let token = a_id.token();
    let a = affinity::Directory::new(
        redis.clone(),
        a_id,
        "https://gateway-a:8443",
        std::time::Duration::from_secs(15),
    )
    .unwrap();
    let b = affinity::Directory::new(
        redis,
        b_id,
        "https://gateway-b:8443",
        std::time::Duration::from_secs(15),
    )
    .unwrap();
    a.register().await.unwrap();
    assert_eq!(b.register().await.unwrap_err(), Error::Conflict);
    assert_eq!(b.resolve(&token).await.unwrap().prefix, a.local_prefix());
    a.release().await.unwrap();
    b.register().await.unwrap();
    assert_eq!(a.resolve(&token).await.err(), Some(Error::LeaseLost));
    assert_eq!(a.heartbeat().await.unwrap_err(), Error::LeaseLost);
    a.release().await.unwrap();
    b.heartbeat().await.unwrap();
    b.release().await.unwrap();
}
#[tokio::test]
#[ignore = "requires disposable Redis; includes a real 16-second TTL expiry"]
async fn dashboard_limits_are_shared_and_lost_leases_never_recreate_capacity() {
    let redis = connection().await;
    let a = dashboard::SocketLeases::new(redis.clone(), 2, 1, std::time::Duration::from_secs(15))
        .unwrap();
    let b = dashboard::SocketLeases::new(redis, 2, 1, std::time::Duration::from_secs(15)).unwrap();
    let tenant = uuid::Uuid::new_v4().to_string();
    let first = a.acquire(&tenant, "u").await.unwrap();
    assert_eq!(b.acquire(&tenant, "u").await.err(), Some(Error::Limit));
    let second = b.acquire(&format!("{tenant}-other"), "u").await.unwrap();
    assert_eq!(
        b.acquire(&format!("{tenant}-third"), "u").await.err(),
        Some(Error::Limit)
    );
    first.release().await.unwrap();
    first.release().await.unwrap();
    assert_eq!(first.renew().await.unwrap_err(), Error::LeaseLost);
    let reclaimed = b.acquire(&tenant, "u").await.unwrap();
    second.release().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(16)).await;
    assert_eq!(reclaimed.renew().await.unwrap_err(), Error::LeaseLost);
    let next = a.acquire(&tenant, "u").await.unwrap();
    next.release().await.unwrap();
}
#[tokio::test]
#[ignore = "requires disposable AGENTFORGE_HA_TEST_REDIS_URL_FILE"]
async fn guest_concurrent_mutation_is_cas_and_deleted_records_cannot_resurrect() {
    use crate::guest_session_manager::{
        GuestSessionConfig, GuestSessionManager, GuestSessionStorage,
    };
    let redis = connection().await;
    let storage = std::sync::Arc::new(guest::AtomicGuestStorage::new(redis));
    let config = GuestSessionConfig {
        key_prefix: format!("guest_session:ha-test:{}", uuid::Uuid::new_v4()),
        max_threads_per_session: 3,
        ..Default::default()
    };
    let a = GuestSessionManager::shared(config.clone(), storage.clone()).unwrap();
    let b = GuestSessionManager::shared(config, storage.clone()).unwrap();
    let session = a.create_session(None).await.unwrap();
    let id = &session.session_id;
    let (x, y) = tokio::join!(
        a.add_thread_to_session(id, "one"),
        b.add_thread_to_session(id, "two")
    );
    assert!(x.unwrap() && y.unwrap());
    let (x, y) = tokio::join!(
        a.add_thread_to_session(id, "same"),
        b.add_thread_to_session(id, "same")
    );
    assert!(x.unwrap() || y.unwrap());
    let threads = b.get_session_threads(id).await.unwrap();
    assert_eq!(threads.len(), 3);
    assert!(threads.contains(&"one".into()) && threads.contains(&"two".into()));
    let key = a.session_key(id);
    let old = storage.raw_get(&key).await.unwrap().unwrap();
    a.delete_session(id).await.unwrap();
    assert!(
        !storage
            .compare_exchange(&key, old.clone(), Some(old), 30)
            .await
            .unwrap()
    );
    assert!(!b.refresh_session(id).await.unwrap());
    assert!(b.get_session(id).await.unwrap().is_none());
}
#[tokio::test]
#[ignore = "requires disposable AGENTFORGE_HA_TEST_REDIS_URL_FILE"]
async fn guest_conversion_cancellation_leaves_reconciliation_marker_without_replay() {
    use crate::guest_session_manager::{
        GuestMigrationContext, GuestMigrationFuture, GuestSessionConfig, GuestSessionManager,
        GuestThreadMigrator,
    };
    struct Migrator {
        started: tokio::sync::Notify,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl GuestThreadMigrator for Migrator {
        fn update_thread<'a>(
            &'a self,
            _user: GuestMigrationContext<'a>,
            _thread: &'a str,
            _metadata: serde_json::Map<String, serde_json::Value>,
        ) -> GuestMigrationFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.started.notify_one();
                std::future::pending().await
            })
        }
    }
    let redis = connection().await;
    let storage = std::sync::Arc::new(guest::AtomicGuestStorage::new(redis));
    let config = GuestSessionConfig {
        key_prefix: format!("guest_session:ha-test:{}", uuid::Uuid::new_v4()),
        ..Default::default()
    };
    let a =
        std::sync::Arc::new(GuestSessionManager::shared(config.clone(), storage.clone()).unwrap());
    let b = GuestSessionManager::shared(config, storage).unwrap();
    let id = a.create_session(None).await.unwrap().session_id;
    a.add_thread_to_session(&id, "one").await.unwrap();
    let migrator = std::sync::Arc::new(Migrator {
        started: tokio::sync::Notify::new(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let (worker, m, id2) = (a.clone(), migrator.clone(), id.clone());
    let task =
        tokio::spawn(async move { worker.convert_to_user(&id2, "user", Some(m.as_ref())).await });
    migrator.started.notified().await;
    task.abort();
    let _ = task.await;
    assert!(b.get_session(&id).await.is_err());
    assert!(b.convert_to_user(&id, "other", None).await.is_err());
    assert_eq!(migrator.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    b.delete_session(&id).await.unwrap();
}
// Model only the storage CAS seam; exercise the real manager with forced read
// races, clock expiry and backend failure without requiring a running service.
#[derive(Default)]
struct CasFixture {
    rows: tokio::sync::Mutex<std::collections::BTreeMap<String, (String, u64)>>,
    now: std::sync::atomic::AtomicU64,
    failed: std::sync::atomic::AtomicBool,
    conflicts: std::sync::atomic::AtomicUsize,
}
impl crate::guest_session_manager::GuestSessionStorage for CasFixture {
    fn atomic_capable(&self) -> bool {
        true
    }
    fn get<'a>(
        &'a self,
        key: &'a str,
    ) -> crate::session_manager::SessionFuture<
        'a,
        Option<crate::guest_session_manager::GuestSessionData>,
    > {
        Box::pin(async move {
            Ok(self
                .raw_get(key)
                .await?
                .map(|s| crate::guest_session_manager::GuestSessionData::Json(s.into_bytes())))
        })
    }
    fn raw_get<'a>(
        &'a self,
        key: &'a str,
    ) -> crate::session_manager::SessionFuture<'a, Option<String>> {
        Box::pin(async move {
            if self.failed.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::session_manager::SessionError::StorageUnavailable);
            }
            let now = self.now.load(std::sync::atomic::Ordering::SeqCst);
            let value = self
                .rows
                .lock()
                .await
                .get(key)
                .filter(|(_, expiry)| *expiry > now)
                .map(|(value, _)| value.clone());
            tokio::task::yield_now().await;
            Ok(value)
        })
    }
    fn save<'a>(
        &'a self,
        key: &'a str,
        value: String,
        ttl: i64,
    ) -> crate::session_manager::SessionFuture<'a, ()> {
        Box::pin(async move {
            let mut rows = self.rows.lock().await;
            let now = self.now.load(std::sync::atomic::Ordering::SeqCst);
            if rows.get(key).is_some_and(|(_, expiry)| *expiry > now) {
                return Err(crate::session_manager::SessionError::AlreadyExists);
            }
            rows.insert(key.into(), (value, now + ttl as u64));
            Ok(())
        })
    }
    fn compare_exchange<'a>(
        &'a self,
        key: &'a str,
        expected: String,
        replacement: Option<String>,
        ttl: i64,
    ) -> crate::session_manager::SessionFuture<'a, bool> {
        Box::pin(async move {
            if self.failed.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::session_manager::SessionError::StorageUnavailable);
            }
            let mut rows = self.rows.lock().await;
            let now = self.now.load(std::sync::atomic::Ordering::SeqCst);
            if !rows
                .get(key)
                .is_some_and(|(value, expiry)| value == &expected && *expiry > now)
            {
                self.conflicts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Ok(false);
            }
            if let Some(value) = replacement {
                rows.insert(key.into(), (value, now + ttl as u64));
            } else {
                rows.remove(key);
            }
            Ok(true)
        })
    }
    fn delete<'a>(&'a self, key: &'a str) -> crate::session_manager::SessionFuture<'a, i64> {
        Box::pin(async move { Ok(i64::from(self.rows.lock().await.remove(key).is_some())) })
    }
}
#[tokio::test]
async fn real_guest_manager_handles_cross_replica_conflicts_expiry_and_backend_failure_without_fallback()
 {
    use crate::guest_session_manager::{GuestSessionConfig, GuestSessionManager};
    use std::sync::{Arc, atomic::Ordering};
    let store = Arc::new(CasFixture::default());
    let config = GuestSessionConfig {
        session_ttl: 60,
        max_threads_per_session: 3,
        ..Default::default()
    };
    let a = GuestSessionManager::shared(config.clone(), store.clone()).unwrap();
    let b = GuestSessionManager::shared(config, store.clone()).unwrap();
    let id = a.create_session(None).await.unwrap().session_id;
    let (x, y) = tokio::join!(
        a.add_thread_to_session(&id, "a"),
        b.add_thread_to_session(&id, "b")
    );
    assert!(x.unwrap() && y.unwrap());
    assert!(store.conflicts.load(Ordering::SeqCst) > 0);
    assert_eq!(b.get_session_threads(&id).await.unwrap().len(), 2);
    store.failed.store(true, Ordering::SeqCst);
    assert!(a.refresh_session(&id).await.is_err());
    store.failed.store(false, Ordering::SeqCst);
    assert_eq!(a.get_session_threads(&id).await.unwrap().len(), 2);
    store.now.store(61, Ordering::SeqCst);
    assert!(b.get_session(&id).await.unwrap().is_none());
    assert!(!a.refresh_session(&id).await.unwrap());
}
#[tokio::test]
async fn two_guest_conversions_cannot_both_consume_the_same_snapshot() {
    use crate::guest_session_manager::GuestSessionManager;
    use std::sync::Arc;
    let store = Arc::new(CasFixture::default());
    let a = GuestSessionManager::shared(Default::default(), store.clone()).unwrap();
    let b = GuestSessionManager::shared(Default::default(), store).unwrap();
    let id = a.create_session(None).await.unwrap().session_id;
    a.add_thread_to_session(&id, "thread").await.unwrap();
    let (x, y) = tokio::join!(
        a.convert_to_user(&id, "authorized-a", None),
        b.convert_to_user(&id, "authorized-b", None)
    );
    let count = [x, y]
        .into_iter()
        .filter(|result| {
            result
                .as_ref()
                .ok()
                .is_some_and(|value| value["success"] == true)
        })
        .count();
    assert_eq!(count, 1);
    assert!(a.get_session(&id).await.unwrap().is_none());
}

#[test]
fn legacy_process_queue_cannot_enter_the_ha_task_profile() {
    let manager = crate::task_manager::TaskManager::new(
        std::sync::Arc::new(crate::task_manager::MemoryTaskStorage::new()),
        std::sync::Arc::new(crate::task_manager::MemoryTaskQueue::new()),
    );
    assert_eq!(
        readiness::require_durable_tasks(&manager),
        Err(Error::Invalid)
    );
}
