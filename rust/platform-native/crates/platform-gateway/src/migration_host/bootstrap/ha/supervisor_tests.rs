use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize};
#[derive(Default)]
struct Mock {
    registrations: Arc<AtomicUsize>,
    renewals: Arc<AtomicUsize>,
    releases: Arc<AtomicUsize>,
    reject_registration: bool,
    lose: AtomicBool,
    hang: bool,
}
impl Authority for Mock {
    fn register(&self) -> HostFuture {
        self.registrations.fetch_add(1, Ordering::AcqRel);
        let reject = self.reject_registration;
        Box::pin(async move {
            if reject {
                Err(HostError::Startup("conflict"))
            } else {
                Ok(())
            }
        })
    }
    fn renew(&self) -> HostFuture {
        self.renewals.fetch_add(1, Ordering::AcqRel);
        let lose = self.lose.load(Ordering::Acquire);
        let hang = self.hang;
        Box::pin(async move {
            if hang {
                std::future::pending::<()>().await;
            }
            if lose {
                Err(HostError::Startup("lost"))
            } else {
                Ok(())
            }
        })
    }
    fn release(&self) -> HostFuture {
        self.releases.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { Ok(()) })
    }
}
async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("bounded supervision");
}
#[tokio::test]
async fn conflict_never_admits_startup() {
    let authority = Arc::new(Mock {
        reject_registration: true,
        ..Default::default()
    });
    let drain = Arc::new(DrainState::default());
    let owner = Supervisor::new(authority.clone(), drain.clone(), Duration::from_millis(10));
    assert!(owner.start().await.is_err());
    assert!(!owner.health.healthy());
    assert!(!drain.draining());
    let effects = Arc::new(AtomicUsize::new(0));
    let effect = effects.clone();
    let step = owner.guard_start(LifecycleStep {
        name: "actual_worker",
        best_effort: true,
        operation: Arc::new(move || {
            effect.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { Ok(()) })
        }),
    });
    assert!(!step.best_effort);
    assert!((step.operation)().await.is_err());
    assert_eq!(effects.load(Ordering::Acquire), 0);
}
#[tokio::test]
async fn loss_drains_once_preserves_inflight_and_never_registers_again() {
    let authority = Arc::new(Mock::default());
    let drain = Arc::new(DrainState::default());
    let inflight = drain.enter().unwrap();
    let owner = Supervisor::new(authority.clone(), drain.clone(), Duration::from_millis(10));
    let stops = Arc::new(AtomicUsize::new(0));
    let stopped = stops.clone();
    assert!(
        owner
            .on_loss
            .set(Arc::new(move || {
                stopped.fetch_add(1, Ordering::AcqRel);
                Box::pin(async { Ok(()) })
            }))
            .is_ok()
    );
    owner.start().await.unwrap();
    assert!(owner.health.healthy());
    authority.lose.store(true, Ordering::Release);
    until(|| drain.draining() && stops.load(Ordering::Acquire) == 1).await;
    assert!(!owner.health.healthy());
    assert!(drain.enter().is_none());
    assert!(owner.start().await.is_err());
    assert_eq!(authority.registrations.load(Ordering::Acquire), 1);
    drop(inflight);
    assert!(drain.wait_drained(Duration::from_secs(1)).await);
    owner.stop().await.unwrap();
    assert_eq!(authority.releases.load(Ordering::Acquire), 1);
    assert!(owner.start().await.is_err());
    assert_eq!(stops.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn hung_renewal_has_bounded_loss_and_clean_stop_interrupts_wait() {
    let authority = Arc::new(Mock {
        hang: true,
        ..Default::default()
    });
    let drain = Arc::new(DrainState::default());
    let owner = Supervisor::new(authority.clone(), drain.clone(), Duration::from_millis(10));
    owner.start().await.unwrap();
    until(|| drain.draining()).await;
    assert!(!owner.health.healthy());
    owner.stop().await.unwrap();
    let authority = Arc::new(Mock::default());
    let drain = Arc::new(DrainState::default());
    let owner = Supervisor::new(authority.clone(), drain.clone(), Duration::from_secs(60));
    owner.start().await.unwrap();
    tokio::time::timeout(Duration::from_millis(100), owner.stop())
        .await
        .unwrap()
        .unwrap();
    assert!(!owner.health.healthy());
    assert!(!drain.draining());
    assert_eq!(authority.renewals.load(Ordering::Acquire), 0);
    assert_eq!(authority.releases.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn lease_lost_during_activation_cannot_be_accepted_or_start_next_worker() {
    let authority = Arc::new(Mock::default());
    let drain = Arc::new(DrainState::default());
    let owner = Supervisor::new(authority.clone(), drain.clone(), Duration::from_millis(10));
    owner.start().await.unwrap();
    let loss = authority.clone();
    let observed = drain.clone();
    let step = owner.guard_start(LifecycleStep {
        name: "delayed_activation",
        best_effort: false,
        operation: Arc::new(move || {
            let loss = loss.clone();
            let observed = observed.clone();
            Box::pin(async move {
                loss.lose.store(true, Ordering::Release);
                until(|| observed.draining()).await;
                Ok(())
            })
        }),
    });
    assert!((step.operation)().await.is_err());
    let step = owner.guard_start(LifecycleStep {
        name: "later_worker",
        best_effort: false,
        operation: Arc::new(|| Box::pin(async { panic!("must never activate") })),
    });
    assert!((step.operation)().await.is_err());
    owner.stop().await.unwrap();
}

#[tokio::test]
async fn readiness_and_http_admission_observe_real_terminal_lease_state() {
    use crate::migration_host::{
        readiness::{ReadinessConfig, ReadinessState},
        resources::ConnectionResources,
    };
    let authority = Arc::new(Mock::default());
    let drain = Arc::new(DrainState::default());
    let owner = Supervisor::new(authority.clone(), drain.clone(), Duration::from_millis(10));
    let readiness = ReadinessState::new(
        Arc::new(ConnectionResources {
            database: None,
            independent_control: None,
            redis: None,
            coordination: None,
            cache: None,
            database_enabled: false,
            redis_enabled: false,
        }),
        ReadinessConfig {
            ha_lease: Some(owner.health.clone()),
            ..Default::default()
        },
        drain.clone(),
    )
    .unwrap();
    assert_eq!(
        readiness.snapshot().await["core"]["gateway_ha_directory"],
        "unavailable"
    );
    owner.start().await.unwrap();
    assert_eq!(
        readiness.snapshot().await["core"]["gateway_ha_directory"],
        "healthy"
    );
    authority.lose.store(true, Ordering::Release);
    until(|| drain.draining()).await;
    let snapshot = readiness.snapshot().await;
    assert_eq!(snapshot["core"]["gateway_ha_directory"], "unavailable");
    assert_eq!(snapshot["core"]["traffic_acceptance"], "unavailable");
    assert_eq!(snapshot["core_ready"], false);
    owner.stop().await.unwrap();
}
