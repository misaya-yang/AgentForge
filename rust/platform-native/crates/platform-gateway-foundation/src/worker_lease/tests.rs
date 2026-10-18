use super::*;
fn guard(deadline: Instant) -> Arc<LeaseGuard> {
    Arc::new(LeaseGuard {
        pool: PgPool::connect_lazy("postgresql://localhost/unopened").unwrap(),
        job: "usage_scheduler",
        owner: Uuid::new_v4(),
        fence: 7,
        live: AtomicBool::new(true),
        deadline: Mutex::new(deadline),
        permits: AtomicU64::new(0),
        drained: tokio::sync::Notify::new(),
        changed: tokio::sync::Notify::new(),
    })
}
#[tokio::test]
async fn expired_or_revoked_owner_cannot_poll_a_side_effect() {
    for expired in [true, false] {
        let guard = guard(if expired {
            Instant::now() - Duration::from_secs(1)
        } else {
            Instant::now() + TTL
        });
        if !expired {
            guard.revoke();
        }
        let polled = AtomicBool::new(false);
        assert_eq!(
            until_invalidated(&guard, async { polled.store(true, Ordering::Release) }).await,
            Err(LeaseError::Lost)
        );
        assert!(!polled.load(Ordering::Acquire));
        assert_eq!(guard.check_local(), Err(LeaseError::Lost));
    }
}
#[tokio::test]
async fn revocation_cancels_inflight_work_and_drops_its_future() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let guard = guard(Instant::now() + TTL);
    let dropped = Arc::new(AtomicBool::new(false));
    let did_start = Arc::new(tokio::sync::Notify::new());
    let worker = {
        let guard = guard.clone();
        let dropped = dropped.clone();
        let did_start = did_start.clone();
        tokio::spawn(async move {
            until_invalidated(&guard, async move {
                let _dropped = Dropped(dropped);
                did_start.notify_one();
                std::future::pending::<()>().await;
            })
            .await
        })
    };
    did_start.notified().await;
    guard.revoke();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap(),
        Err(LeaseError::Lost)
    );
    assert!(dropped.load(Ordering::Acquire));
}
#[tokio::test]
async fn lease_deadline_cancels_an_inflight_effect_without_a_heartbeat() {
    let guard = guard(Instant::now() + Duration::from_millis(20));
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            until_invalidated(&guard, std::future::pending::<()>())
        )
        .await
        .unwrap(),
        Err(LeaseError::Lost)
    );
}
#[tokio::test]
async fn renewal_wakes_waiter_and_extends_deadline_without_resurrection() {
    let guard = guard(Instant::now() + Duration::from_millis(20));
    let worker = {
        let guard = guard.clone();
        tokio::spawn(async move {
            until_invalidated(&guard, async {
                tokio::time::sleep(Duration::from_millis(60)).await;
                42
            })
            .await
        })
    };
    *guard.deadline.lock().unwrap() = Instant::now() + TTL;
    guard.changed.notify_waiters();
    assert_eq!(worker.await.unwrap(), Ok(42));
    guard.revoke();
    *guard.deadline.lock().unwrap() = Instant::now() + TTL;
    assert_eq!(guard.check_local(), Err(LeaseError::Lost));
}
#[tokio::test]
async fn drain_waits_for_retained_work_and_has_no_lost_wakeup() {
    let guard = guard(Instant::now() + TTL);
    guard.permits.store(1, Ordering::Release);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), guard.drain())
            .await
            .is_err()
    );
    guard.permits.store(0, Ordering::Release);
    guard.drained.notify_waiters();
    tokio::time::timeout(Duration::from_secs(1), guard.drain())
        .await
        .unwrap();
}
#[test]
fn only_known_singleton_names_are_authorized() {
    for name in [
        "usage_scheduler",
        "image_task_worker",
        "scheduled_task_dispatcher",
        "eval_outbox_worker",
    ] {
        assert!(valid_job(name));
    }
    for name in [
        "",
        "usage_recorder",
        "task_worker",
        "database",
        "../job",
        "USAGE_SCHEDULER",
    ] {
        assert!(!valid_job(name));
    }
}
#[tokio::test]
async fn unknown_jobs_fail_before_connecting_to_postgres() {
    let pool = PgPool::connect_lazy("postgresql://localhost/unopened").unwrap();
    let store = LeaseStore::new(pool.clone(), pool);
    assert!(matches!(
        store.acquire("unknown").await,
        Err(LeaseError::InvalidJob)
    ));
}

/// Run only against a disposable database whose name begins `test_`. The native
/// authority must have installed the U-GW-12 proposal first. No application
/// startup migrations or production credentials are used by ordinary unit tests.
#[tokio::test]
#[ignore = "requires disposable PostgreSQL and native-authority U-GW-12 schema"]
async fn postgres_two_owners_expiry_effect_lock_and_stale_real_write() {
    use sqlx::Connection;
    let dsn = std::env::var("AI_PLATFORM_TEST_WORKER_LEASE_DSN")
        .expect("disposable worker-lease test DSN required");
    let pool = PgPool::connect(&dsn)
        .await
        .expect("test database unavailable");
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(database.starts_with("test_"), "refusing non-test database");
    let control = PgPool::connect(&dsn).await.unwrap();
    let first = LeaseStore::new(pool.clone(), control.clone());
    let second = LeaseStore::new(pool.clone(), control.clone());
    first.verify_schema().await.unwrap();
    let mut held = first
        .acquire("usage_scheduler")
        .await
        .unwrap()
        .expect("test job must be unused");
    let fence = held.guard.fence;
    assert!(second.acquire("usage_scheduler").await.unwrap().is_none());
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("CREATE TEMP TABLE worker_fence_probe(value integer NOT NULL)")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO worker_fence_probe VALUES (0)")
        .execute(&mut *connection)
        .await
        .unwrap();
    let mut effect = connection.begin().await.unwrap();
    held.guard.assert_on(&mut effect).await.unwrap();
    sqlx::query("UPDATE worker_fence_probe SET value=value+1")
        .execute(&mut *effect)
        .await
        .unwrap();
    // A live effect KEY SHARE must not block the heartbeat's NO KEY UPDATE.
    held.renew().await.unwrap();
    // Test-owner mutation simulates server expiry without a 30-second sleep.
    sqlx::query("UPDATE gateway.singleton_worker_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE job='usage_scheduler'")
        .execute(&control).await.unwrap();
    // Expiry alone cannot take over a transaction that still owns a real effect.
    assert!(second.acquire("usage_scheduler").await.unwrap().is_none());
    effect.commit().await.unwrap();
    let successor = second.acquire("usage_scheduler").await.unwrap().unwrap();
    assert!(successor.guard.fence > fence);
    assert!(held.renew().await.is_err());
    // An old owner cannot make a real write on the effect's own transaction.
    let mut stale = connection.begin().await.unwrap();
    assert_eq!(
        held.guard.assert_on(&mut stale).await,
        Err(LeaseError::Lost)
    );
    stale.rollback().await.unwrap();
    let value: i32 = sqlx::query_scalar("SELECT value FROM worker_fence_probe")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(value, 1);
    let old_valid: bool = sqlx::query_scalar("SELECT gateway.assert_worker_lease($1,$2::uuid,$3)")
        .bind("usage_scheduler")
        .bind(first.owner.to_string())
        .bind(fence)
        .fetch_one(&control)
        .await
        .unwrap();
    assert!(!old_valid);
    held.release().await.unwrap();
    // A stale release must not expire the successor.
    let mut successor = successor;
    successor.renew().await.unwrap();
    successor.release().await.unwrap();
    pool.close().await;
    control.close().await;
}

#[tokio::test]
async fn a_hidden_blocking_effect_keeps_its_owned_waiter_after_caller_cancellation() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let completed = Arc::new(tokio::sync::Notify::new());
    let waiter = {
        let entered = entered.clone();
        let release = release.clone();
        let completed = completed.clone();
        tokio::spawn(finish_effect(async move {
            entered.notify_one();
            let _released = release.acquire().await.unwrap();
            completed.notify_one();
        }))
    };
    entered.notified().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), completed.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn effect_locks_never_consume_the_data_or_heartbeat_connection_budget() {
    let data = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy("postgresql://localhost/unopened")
        .unwrap();
    let control = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy("postgresql://localhost/unopened")
        .unwrap();
    let store = LeaseStore::new(data.clone(), control.clone());
    assert!(!std::ptr::eq(store.effects.options(), data.options()));
    assert!(!std::ptr::eq(store.effects.options(), control.options()));
    assert!(std::ptr::eq(store.control.options(), control.options()));
    assert_eq!(
        store.effects.options().get_max_connections(),
        EFFECT_CONNECTIONS
    );
}
