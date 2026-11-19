//! Own remote capacity only. The original model-call authority remains the sole
//! token/money reservation and completion/unknown owner.
use super::*;
use crate::capacity_admission::CapacityLease;
use redis::aio::ConnectionManager;
use std::{
    future::Future,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::task::JoinHandle;
pub struct ModelPlaneLease {
    local: Arc<CapacityLease>,
    global: SlotPlan,
    tenant: SlotPlan,
    connection: ConnectionManager,
    admission: ModelPlaneAdmission,
    proof: Proof,
    member: String,
    lost: AdmissionCancellation,
    stop: AdmissionCancellation,
    released: AtomicBool,
    post_started: AtomicBool,
    renewal: Mutex<Option<JoinHandle<()>>>,
    deadline: tokio::time::Instant,
}
impl ModelPlaneLease {
    pub(super) fn new(
        local: CapacityLease,
        global: SlotPlan,
        tenant: SlotPlan,
        connection: ConnectionManager,
        admission: ModelPlaneAdmission,
        proof: Proof,
        member: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            local: Arc::new(local),
            deadline: tokio::time::Instant::now() + Duration::from_millis(proof.deadline_ms as u64),
            global,
            tenant,
            connection,
            admission,
            proof,
            member,
            lost: Default::default(),
            stop: Default::default(),
            released: AtomicBool::new(false),
            post_started: AtomicBool::new(false),
            renewal: Mutex::new(None),
        })
    }
    pub fn tokens(&self) -> &ModelTokenReservation {
        &self.proof.units
    }
    pub(super) async fn acquire_remote(
        &self,
        logical: &str,
        ttl_ms: i64,
    ) -> Result<(), DispatchError> {
        self.global
            .acquire_logical(&self.connection, &self.member, ttl_ms, logical)
            .await
            .map_err(refusal)?;
        self.tenant
            .acquire_logical(&self.connection, &self.member, ttl_ms, logical)
            .await
            .map_err(refusal)?;
        Ok(())
    }
    pub(super) fn start_renew(self: &Arc<Self>, ttl_ms: i64) {
        // A weak owner prevents the renewal task retaining its own lease forever.
        let weak = Arc::downgrade(self);
        let stop = self.stop.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {biased;_=stop.cancelled()=>return,
                _=tokio::time::sleep(Duration::from_millis((ttl_ms/3) as u64))=>{}}
                let Some(owner) = weak.upgrade() else { return };
                if owner.released.load(Ordering::Acquire) {
                    return;
                }
                if tokio::time::Instant::now() >= owner.deadline {
                    owner.lost.cancel();
                    return;
                }
                let (global, tenant) = tokio::join!(
                    owner.global.renew(&owner.connection, &owner.member, ttl_ms),
                    owner.tenant.renew(&owner.connection, &owner.member, ttl_ms)
                );
                if global.is_err() || tenant.is_err() {
                    owner.lost.cancel();
                    return;
                }
            }
        });
        *self.renewal.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
    }
    /// The caller's existing canonical CAS must have returned one changed row.
    /// Revalidate the committed budget row and immutable binding before the sole
    /// POST. This local fence rejects a second POST through the same lease.
    pub async fn before_post(&self) -> Result<(), DispatchError> {
        if tokio::time::Instant::now() >= self.deadline
            || self.released.load(Ordering::Acquire)
            || self.lost.is_cancelled()
            || self
                .post_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(refusal(Refusal::Unavailable));
        }
        self.admission.confirm_dispatched(&self.proof).await?;
        if tokio::time::Instant::now() >= self.deadline
            || self.lost.is_cancelled()
            || self.released.load(Ordering::Acquire)
        {
            return Err(refusal(Refusal::Unavailable));
        }
        Ok(())
    }
    pub async fn wait_lost(&self) {
        tokio::select! {_=self.lost.cancelled()=>{},_=self.local.wait_lost()=>{},_=tokio::time::sleep_until(self.deadline)=>{self.lost.cancel();}}
    }
    /// Knowledge calls this after its checked source CAS, before polling the
    /// future containing POST. Runtime calls before_post explicitly before POST
    /// and uses wait_lost for the subsequently owned response stream.
    pub async fn run<F: Future>(
        &self,
        future: F,
        cancellation: AdmissionCancellation,
    ) -> Result<F::Output, DispatchError> {
        if let Err(error) = self.before_post().await {
            self.release().await;
            return Err(error);
        }
        let result = tokio::select! {biased;
            _=self.wait_lost()=>Err(refusal(Refusal::Unavailable)),
            _=cancellation.cancelled()=>Err(DispatchError::Cancelled),
            result=future=>Ok(result),
        };
        self.release().await;
        result
    }
    pub async fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stop.cancel();
        let renewal = self
            .renewal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let (global, tenant, connection, member, local) = (
            self.global.clone(),
            self.tenant.clone(),
            self.connection.clone(),
            self.member.clone(),
            self.local.clone(),
        );
        let _ = tokio::spawn(async move {
            if let Some(task) = renewal {
                task.abort();
                let _ = task.await;
            }
            let _ = tokio::join!(
                global.release(&connection, &member),
                tenant.release(&connection, &member)
            );
            local.release().await;
        })
        .await;
        // No token refund here: only the owner's canonical completion/CAS may
        // change its existing reservation. Drop/unknown retain exactly that hold.
    }
}
impl Drop for ModelPlaneLease {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(task) = self
            .renewal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            task.abort();
        }
        if !self.released.swap(true, Ordering::AcqRel) {
            let global = self.global.clone();
            let tenant = self.tenant.clone();
            let connection = self.connection.clone();
            let member = self.member.clone();
            tokio::spawn(async move {
                let _ = tokio::join!(
                    global.release(&connection, &member),
                    tenant.release(&connection, &member)
                );
            });
        }
        // CapacityLease's own cancellation-safe Drop handles local permits.
    }
}
