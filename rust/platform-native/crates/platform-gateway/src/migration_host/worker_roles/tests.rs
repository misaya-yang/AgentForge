use super::*;
use std::sync::{
    Mutex as SyncMutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
fn pool() -> sqlx::PgPool {
    sqlx::PgPool::connect_lazy("postgresql://localhost/unopened").unwrap()
}
fn operation(counter: Arc<AtomicUsize>, fail: bool) -> HostOperation {
    Arc::new(move || {
        let counter = counter.clone();
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            if fail {
                Err(HostError::Database)
            } else {
                Ok(())
            }
        })
    })
}
#[test]
fn cli_defaults_and_every_config_mode_preserve_compatibility() {
    assert_eq!(ProcessRole::default(), ProcessRole::All);
    for mode in [
        "--config-env",
        "--config-stdin",
        "--check-config-env",
        "--check-config-stdin",
    ] {
        let parsed = Arguments::parse(&[mode.into()]).unwrap();
        assert_eq!(parsed.role, None);
        assert_eq!(parsed.check_only, mode.starts_with("--check-"));
        for role in ["api", "worker", "all"] {
            let parsed = Arguments::parse(&["--role".into(), role.into(), mode.into()]).unwrap();
            assert_eq!(parsed.role, Some(ProcessRole::parse(role).unwrap()));
        }
    }
    for args in [
        vec![],
        vec!["--role", "worker"],
        vec!["--config-env", "--role"],
        vec!["--config-env", "--role", "API"],
        vec!["--config-env", "--role", "all", "--role", "api"],
        vec!["--config-env", "--config-stdin"],
        vec!["--config-env", "--unknown"],
    ] {
        assert!(
            Arguments::parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err()
        );
    }
}
#[tokio::test]
async fn api_filters_real_worker_steps_and_keeps_local_usage_flush() {
    let worker = Arc::new(AtomicUsize::new(0));
    let local = Arc::new(AtomicUsize::new(0));
    let names = [
        "eval_outbox_worker",
        "usage_scheduler",
        "scheduled_task_dispatcher",
        "image_task_worker",
        "file_cleanup_service",
        "sdk_session_cleanup",
        "trace_retention_scheduler",
        "assistant_trace_reconciler_task",
        "usage_event_consumer",
        "task_worker",
        "usage_recorder",
        "usage_event_publisher",
        "health_monitor",
    ];
    let mut starts: Vec<_> = names
        .iter()
        .map(|name| LifecycleStep {
            name,
            best_effort: false,
            operation: operation(
                if worker_job(name) {
                    worker.clone()
                } else {
                    local.clone()
                },
                false,
            ),
        })
        .collect();
    let mut stops = starts.clone();
    let health = apply(ProcessRole::Api, pool(), pool(), &mut starts, &mut stops).unwrap();
    assert!(health.ready());
    assert_eq!(health.image_status(), "disabled");
    let lifecycle = super::super::lifecycle::Lifecycle::new(starts, stops);
    lifecycle.start().await.unwrap();
    lifecycle
        .stop(&super::super::middleware::DrainState::default())
        .await
        .unwrap();
    assert_eq!(worker.load(Ordering::SeqCst), 0);
    assert_eq!(local.load(Ordering::SeqCst), 6);
}
#[tokio::test]
async fn worker_routes_never_enter_business_handlers() {
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        routing::get,
    };
    use tower::ServiceExt;
    let router = Router::new()
        .fallback(get(|| async { StatusCode::OK }))
        .layer(axum::middleware::from_fn(worker_only));
    for path in ["/health", "/health/live", "/health/ready", "/metrics"] {
        assert_eq!(
            router
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    for path in [
        "/v1/chat/completions",
        "/api/v1/threads",
        "/api/v1/auth/login",
        "/health/ready/",
        "/v1/responses",
        "/api/v1/sdk/agent-events",
        "/health/../v1/chat/completions",
    ] {
        assert_eq!(
            router
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
}
#[derive(Default)]
struct Shared {
    owner: SyncMutex<Option<(u64, u64)>>,
    next: AtomicU64,
    starts: AtomicUsize,
    stops: AtomicUsize,
    active: AtomicUsize,
    maximum: AtomicUsize,
    releases: AtomicUsize,
}
struct FakeAuthority {
    shared: Arc<Shared>,
    id: u64,
    lose: Arc<AtomicBool>,
    schema: bool,
}
struct FakeLease {
    shared: Arc<Shared>,
    id: u64,
    fence: u64,
    lose: Arc<AtomicBool>,
}
impl Authority for FakeAuthority {
    fn verify_schema(&self) -> AuthorityFuture<'_, ()> {
        Box::pin(async move {
            if self.schema {
                Ok(())
            } else {
                Err(HostError::Database)
            }
        })
    }
    fn acquire(&self, _: &'static str) -> AuthorityFuture<'_, Option<Box<dyn ActiveLease>>> {
        Box::pin(async move {
            let mut owner = self.shared.owner.lock().unwrap();
            if owner.is_some() {
                return Ok(None);
            }
            let fence = self.shared.next.fetch_add(1, Ordering::SeqCst) + 1;
            *owner = Some((self.id, fence));
            Ok(Some(Box::new(FakeLease {
                shared: self.shared.clone(),
                id: self.id,
                fence,
                lose: self.lose.clone(),
            }) as Box<dyn ActiveLease>))
        })
    }
}
impl ActiveLease for FakeLease {
    fn revoke(&self) {
        self.lose.store(true, Ordering::SeqCst);
    }
    fn renew(&mut self) -> AuthorityFuture<'_, ()> {
        Box::pin(async move {
            if self.lose.load(Ordering::SeqCst) {
                Err(HostError::Database)
            } else {
                Ok(())
            }
        })
    }
    fn release(self: Box<Self>) -> AuthorityFuture<'static, ()> {
        Box::pin(async move {
            assert_eq!(
                self.shared.active.load(Ordering::SeqCst),
                0,
                "release must follow actual drain"
            );
            let mut owner = self.shared.owner.lock().unwrap();
            if *owner == Some((self.id, self.fence)) {
                *owner = None;
            }
            self.shared.releases.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
fn singleton(
    shared: Arc<Shared>,
    id: u64,
    lose: Arc<AtomicBool>,
    schema: bool,
    fail_start: bool,
    fail_stop: bool,
) -> Arc<Singleton> {
    let start = {
        let shared = shared.clone();
        Arc::new(move || {
            let shared = shared.clone();
            Box::pin(async move {
                shared.starts.fetch_add(1, Ordering::SeqCst);
                let active = shared.active.fetch_add(1, Ordering::SeqCst) + 1;
                shared.maximum.fetch_max(active, Ordering::SeqCst);
                if fail_start {
                    Err(HostError::Database)
                } else {
                    Ok(())
                }
            }) as super::super::lifecycle::HostFuture
        }) as HostOperation
    };
    let stop = {
        let shared = shared.clone();
        Arc::new(move || {
            let shared = shared.clone();
            Box::pin(async move {
                shared.stops.fetch_add(1, Ordering::SeqCst);
                shared.active.fetch_sub(1, Ordering::SeqCst);
                if fail_stop {
                    Err(HostError::Shutdown("fake_worker"))
                } else {
                    Ok(())
                }
            }) as super::super::lifecycle::HostFuture
        }) as HostOperation
    };
    Arc::new(Singleton {
        store: Arc::new(FakeAuthority {
            shared,
            id,
            lose,
            schema,
        }),
        name: "usage_scheduler",
        start,
        stop,
        health: Arc::new(WorkerHealth::default()),
        running: Mutex::new(None),
    })
}
async fn wait_count(counter: &AtomicUsize, count: usize) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while counter.load(Ordering::SeqCst) < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("supervisor did not reach expected state");
}
#[tokio::test]
async fn two_replicas_start_only_one_job_and_standby_takes_over_after_stop() {
    let shared = Arc::new(Shared::default());
    let first = singleton(
        shared.clone(),
        1,
        Arc::new(AtomicBool::new(false)),
        true,
        false,
        false,
    );
    let second = singleton(
        shared.clone(),
        2,
        Arc::new(AtomicBool::new(false)),
        true,
        false,
        false,
    );
    first.start().await.unwrap();
    wait_count(&shared.starts, 1).await;
    second.start().await.unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(shared.starts.load(Ordering::SeqCst), 1);
    assert!(second.health.ready());
    tokio::time::timeout(Duration::from_secs(5), first.stop())
        .await
        .unwrap()
        .unwrap();
    wait_count(&shared.starts, 2).await;
    tokio::time::timeout(Duration::from_secs(5), second.stop())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(shared.maximum.load(Ordering::SeqCst), 1);
    assert_eq!(shared.active.load(Ordering::SeqCst), 0);
    assert_eq!(shared.releases.load(Ordering::SeqCst), 2);
    assert_eq!(shared.next.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn renewal_failure_drains_and_releases_before_reacquiring_a_new_fence() {
    let shared = Arc::new(Shared::default());
    let lost = Arc::new(AtomicBool::new(false));
    let first = singleton(shared.clone(), 1, lost.clone(), true, false, false);
    first.start().await.unwrap();
    wait_count(&shared.starts, 1).await;
    lost.store(true, Ordering::SeqCst);
    wait_count(&shared.releases, 1).await;
    tokio::time::timeout(Duration::from_secs(5), first.stop())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(shared.maximum.load(Ordering::SeqCst), 1);
    assert_eq!(
        shared.starts.load(Ordering::SeqCst),
        shared.stops.load(Ordering::SeqCst)
    );
}
#[tokio::test]
async fn failed_start_cleans_partial_worker_and_surfaces_failure() {
    let shared = Arc::new(Shared::default());
    let job = singleton(
        shared.clone(),
        1,
        Arc::new(AtomicBool::new(false)),
        true,
        true,
        false,
    );
    job.start().await.unwrap();
    wait_count(&shared.stops, 1).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), job.stop())
            .await
            .unwrap()
            .is_err()
    );
    assert!(!job.health.ready());
    assert_eq!(shared.releases.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn failed_drain_never_releases_or_claims_success() {
    let shared = Arc::new(Shared::default());
    let job = singleton(
        shared.clone(),
        1,
        Arc::new(AtomicBool::new(false)),
        true,
        false,
        true,
    );
    job.start().await.unwrap();
    wait_count(&shared.starts, 1).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), job.stop())
            .await
            .unwrap()
            .is_err()
    );
    assert!(!job.health.ready());
    assert_eq!(shared.releases.load(Ordering::SeqCst), 0);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), job.stop())
            .await
            .unwrap()
            .is_err(),
        "a repeated cleanup must retain terminal failure"
    );
}
#[tokio::test]
async fn missing_schema_never_starts_a_worker() {
    let shared = Arc::new(Shared::default());
    let job = singleton(
        shared.clone(),
        1,
        Arc::new(AtomicBool::new(false)),
        false,
        false,
        false,
    );
    assert!(job.start().await.is_err());
    assert_eq!(shared.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn partial_lifecycle_startup_drains_all_owners_and_shutdown_continues_after_error() {
    use super::super::{lifecycle::Lifecycle, middleware::DrainState};
    let starts = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let lifecycle = Lifecycle::new(
        vec![
            LifecycleStep {
                name: "first",
                operation: operation(starts.clone(), false),
                best_effort: false,
            },
            LifecycleStep {
                name: "partial",
                operation: operation(starts.clone(), true),
                best_effort: false,
            },
        ],
        vec![
            LifecycleStep {
                name: "partial",
                operation: operation(stops.clone(), false),
                best_effort: false,
            },
            LifecycleStep {
                name: "first",
                operation: operation(stops.clone(), false),
                best_effort: false,
            },
        ],
    );
    assert!(lifecycle.start().await.is_err());
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert_eq!(stops.load(Ordering::SeqCst), 2);
    lifecycle.stop(&DrainState::default()).await.unwrap();
    let lifecycle = Lifecycle::new(
        vec![],
        vec![
            LifecycleStep {
                name: "bad",
                operation: operation(stops.clone(), true),
                best_effort: false,
            },
            LifecycleStep {
                name: "later_drain",
                operation: operation(stops.clone(), false),
                best_effort: false,
            },
        ],
    );
    lifecycle.start().await.unwrap();
    assert!(lifecycle.stop(&DrainState::default()).await.is_err());
    assert_eq!(stops.load(Ordering::SeqCst), 4);
}

#[test]
fn split_roles_reject_process_local_task_delivery() {
    assert!(ProcessRole::All.validate_delivery("memory").is_ok());
    for role in [ProcessRole::Api, ProcessRole::Worker] {
        assert!(role.validate_delivery("memory").is_err());
        assert!(role.validate_delivery("durable").is_ok());
        assert!(role.validate_delivery("schema_admitted").is_ok());
    }
}

#[tokio::test]
async fn dropping_the_handle_owner_signals_stop_instead_of_detaching_the_singleton() {
    let shared = Arc::new(Shared::default());
    let job = singleton(
        shared.clone(),
        1,
        Arc::new(AtomicBool::new(false)),
        true,
        false,
        false,
    );
    job.start().await.unwrap();
    wait_count(&shared.starts, 1).await;
    drop(job);
    wait_count(&shared.stops, 1).await;
    wait_count(&shared.releases, 1).await;
    assert_eq!(shared.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn schema_admission_precedes_even_the_distributed_task_worker() {
    let started = Arc::new(AtomicUsize::new(0));
    let mut starts = vec![LifecycleStep {
        name: "task_worker",
        operation: operation(started.clone(), false),
        best_effort: false,
    }];
    let mut stops = vec![];
    apply(ProcessRole::Worker, pool(), pool(), &mut starts, &mut stops).unwrap();
    assert_eq!(starts[0].name, "worker_lease_schema");
    let lifecycle = super::super::lifecycle::Lifecycle::new(starts, stops);
    assert!(lifecycle.start().await.is_err());
    assert_eq!(started.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn eval_role_keeps_only_the_durable_judge_and_shared_local_services() {
    let local = Arc::new(AtomicUsize::new(0));
    let names = [
        "eval_outbox_worker",
        "task_worker",
        "usage_scheduler",
        "trace_retention_scheduler",
        "usage_recorder",
        "authorization_decision_log",
    ];
    let mut starts = names
        .iter()
        .map(|name| LifecycleStep {
            name,
            best_effort: false,
            operation: operation(local.clone(), false),
        })
        .collect::<Vec<_>>();
    let mut stops = starts.clone();
    apply(ProcessRole::Eval, pool(), pool(), &mut starts, &mut stops).unwrap();
    let names = starts.iter().map(|s| s.name).collect::<Vec<_>>();
    assert!(names.contains(&"eval_outbox_worker"));
    assert!(names.contains(&"worker_lease_schema"));
    assert!(names.contains(&"usage_recorder"));
    assert!(names.contains(&"authorization_decision_log"));
    assert!(!names.contains(&"task_worker"));
    assert!(!names.contains(&"usage_scheduler"));
    assert!(!names.contains(&"trace_retention_scheduler"));
    assert_eq!(ProcessRole::parse("eval"), Ok(ProcessRole::Eval));
    assert!(ProcessRole::Eval.validate_delivery("memory").is_err());
}
