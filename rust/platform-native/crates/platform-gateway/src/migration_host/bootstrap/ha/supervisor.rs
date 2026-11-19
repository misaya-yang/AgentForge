use super::*;
use crate::migration_host::lifecycle::{HostError, HostFuture, HostOperation, LifecycleStep};
use std::sync::{
    OnceLock,
    atomic::{AtomicU8, Ordering},
};
use tokio::sync::{Mutex, watch};
const STARTING: u8 = 0;
const HEALTHY: u8 = 1;
const LOST: u8 = 2;
const STOPPED: u8 = 3;
#[derive(Default)]
pub struct LeaseHealth(AtomicU8);
impl LeaseHealth {
    pub fn healthy(&self) -> bool {
        self.0.load(Ordering::Acquire) == HEALTHY
    }
    fn set(&self, state: u8) {
        self.0.store(state, Ordering::Release)
    }
}
pub(super) trait Authority: Send + Sync {
    fn register(&self) -> HostFuture;
    fn renew(&self) -> HostFuture;
    fn release(&self) -> HostFuture;
}
pub(super) struct DirectoryAuthority(pub Directory);
impl Authority for DirectoryAuthority {
    fn register(&self) -> HostFuture {
        let d = self.0.clone();
        Box::pin(async move {
            d.register()
                .await
                .map_err(|_| HostError::Startup("ha_directory_register"))
        })
    }
    fn renew(&self) -> HostFuture {
        let d = self.0.clone();
        Box::pin(async move {
            d.heartbeat()
                .await
                .map_err(|_| HostError::Startup("ha_directory_lost"))
        })
    }
    fn release(&self) -> HostFuture {
        let d = self.0.clone();
        Box::pin(async move {
            d.release()
                .await
                .map_err(|_| HostError::Shutdown("ha_directory_release"))
        })
    }
}
struct Running {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
pub(super) struct Supervisor {
    authority: Arc<dyn Authority>,
    drain: Arc<DrainState>,
    period: Duration,
    pub health: Arc<LeaseHealth>,
    running: Mutex<Option<Running>>,
    pub on_loss: OnceLock<HostOperation>,
}
impl Supervisor {
    pub fn new(
        authority: Arc<dyn Authority>,
        drain: Arc<DrainState>,
        period: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            authority,
            drain,
            period,
            health: Arc::new(LeaseHealth(AtomicU8::new(STARTING))),
            running: Mutex::new(None),
            on_loss: OnceLock::new(),
        })
    }
    async fn start(self: &Arc<Self>) -> Result<(), HostError> {
        let mut running = self.running.lock().await;
        if running.is_some() {
            return if self.health.healthy() {
                Ok(())
            } else {
                Err(HostError::Startup("ha_directory_lost"))
            };
        }
        if self.health.0.load(Ordering::Acquire) == LOST || self.drain.draining() {
            return Err(HostError::Startup("ha_directory_terminal"));
        }
        tokio::time::timeout(self.period, self.authority.register())
            .await
            .map_err(|_| HostError::Startup("ha_directory_register_timeout"))??;
        self.health.set(HEALTHY);
        let (stop, mut receiver) = watch::channel(false);
        let owner = self.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {biased;_=receiver.changed()=>break,_=tokio::time::sleep(owner.period)=>{}}
                let renewal = tokio::select! {biased;_=receiver.changed()=>break,result=tokio::time::timeout(owner.period,owner.authority.renew())=>result};
                if !matches!(renewal, Ok(Ok(()))) {
                    owner.health.set(LOST);
                    owner.drain.begin_drain();
                    if let Some(stop) = owner.on_loss.get() {
                        let _ = stop().await;
                    }
                    break;
                }
            }
        });
        *running = Some(Running { stop, task });
        Ok(())
    }
    async fn stop(&self) -> Result<(), HostError> {
        let running = self.running.lock().await.take();
        if let Some(mut r) = running {
            let _ = r.stop.send(true);
            if tokio::time::timeout(self.period, &mut r.task)
                .await
                .is_err()
            {
                r.task.abort();
                let _ = r.task.await;
            }
        }
        self.health.set(STOPPED);
        tokio::time::timeout(self.period, self.authority.release())
            .await
            .map_err(|_| HostError::Shutdown("ha_directory_release_timeout"))?
    }
    pub fn guard_start(self: &Arc<Self>, step: LifecycleStep) -> LifecycleStep {
        let owner = self.clone();
        let operation = step.operation;
        LifecycleStep {
            name: step.name,
            best_effort: false,
            operation: Arc::new(move || {
                let owner = owner.clone();
                let operation = operation.clone();
                Box::pin(async move {
                    if !owner.health.healthy() || owner.drain.draining() {
                        return Err(HostError::Startup("ha_directory_not_ready"));
                    }
                    operation().await?;
                    if !owner.health.healthy() || owner.drain.draining() {
                        return Err(HostError::Startup("ha_directory_lost_during_activation"));
                    }
                    Ok(())
                })
            }),
        }
    }
    pub fn lifecycle_steps(self: &Arc<Self>) -> (LifecycleStep, LifecycleStep) {
        let start = self.clone();
        let stop = self.clone();
        (
            LifecycleStep {
                name: "gateway_ha_directory",
                best_effort: false,
                operation: Arc::new(move || {
                    let owner = start.clone();
                    Box::pin(async move { owner.start().await })
                }),
            },
            LifecycleStep {
                name: "gateway_ha_directory",
                best_effort: false,
                operation: Arc::new(move || {
                    let owner = stop.clone();
                    Box::pin(async move { owner.stop().await })
                }),
            },
        )
    }
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
