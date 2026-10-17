//! Production dispatch admission: local fair queues, one global-slot Lua and
//! one tenant-slot Lua (GCRA + all tenant capacity + token intent).
//! Cleanup and lease renewals are lifecycle operations, not extra admission
//! probes. PostgreSQL intents survive Valkey loss; no dispatched hold expires.
use super::{
    AdmissionCancellation, AdmissionError, AdmissionRequest, CapacityAdmissionController,
    CapacityLease, rejected,
};
use crate::{
    capacity::CapacityBudget,
    dispatcher::{
        DispatchAdmission, DispatchContext, DispatchError, DispatchFuture, DispatchLease,
        DispatchService, DispatchSettlement,
    },
    dispatcher_rate_limiter::{RateLimit, RateLimitConfig},
    protocol::UnifiedRequest,
    quota_store::reservations::{ReservationError, ReservationIdentity, ReservationStore},
};
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::task::JoinHandle;
#[path = "quota_context.rs"]
mod quota_context;
#[path = "atomic_store.rs"]
mod store;
pub use quota_context::{AdmittedQuota, decision as quota_minute_decision, scope as quota_scope};
use store::{Refusal, SlotPlan, hex, tenant_tag};
pub(super) fn encode(value: &str) -> String {
    hex(value)
}
pub(super) fn global_tag(epoch: &str) -> String {
    format!("gateway-capacity-{}", hex(epoch))
}

#[derive(Clone)]
pub struct AtomicDispatchAdmission {
    capacity: CapacityAdmissionController,
    connection: ConnectionManager,
    reservations: ReservationStore,
    rates: RateLimitConfig,
    epoch: String,
    ttl_ms: i64,
}
impl AtomicDispatchAdmission {
    pub fn new(
        capacity: CapacityAdmissionController,
        connection: ConnectionManager,
        pool: PgPool,
        rates: RateLimitConfig,
        epoch: String,
        ttl_ms: i64,
    ) -> Self {
        Self {
            capacity,
            connection,
            reservations: ReservationStore::new(pool),
            rates,
            epoch,
            ttl_ms: ttl_ms.clamp(1000, 300_000),
        }
    }
    async fn acquire_owned(
        &self,
        context: DispatchContext,
        service: DispatchService,
        class: String,
        budgets: Vec<CapacityBudget>,
        request: Option<UnifiedRequest>,
        logical_id: String,
        verified_batch: bool,
    ) -> Result<Box<dyn DispatchLease>, DispatchError> {
        let request = request.ok_or_else(|| refusal(Refusal::Unavailable))?;
        // The caller context was bound by preflight authority. Never derive the
        // ledger identity from client-supplied request-body tenant/user fields.
        let tenant = context.admission_tenant_id();
        let user = context.admission_user_id();
        let estimate = if verified_batch { 0 } else { estimate(&request).map_err(refusal)? };
        let admission = AdmissionRequest {
            tenant_id: tenant.into(),
            user_id: user.into(),
            service_id: service.service_id.clone(),
            request_class: class.clone(),
            request_id: context.request_id.clone(),
            priority: request.priority,
            ..AdmissionRequest::default()
        };
        let local = self
            .capacity
            .acquire_local_only(&budgets, &admission)
            .await?;
        let id = ReservationIdentity {
            tenant: tenant.into(),
            user: user.into(),
            request: context.request_id.clone(),
            service: service.service_id.clone(),
            request_type: if verified_batch { "provider_batch" } else if class == "sync" { "invoke" } else { "stream" }.into(),
            member: uuid::Uuid::new_v4().to_string(),
            logical_id: logical_id.clone(),
        };
        let prepared = if verified_batch {self.reservations.prepare_provider_batch(&id,self.ttl_ms).await} else {self.reservations.prepare(&id,estimate,self.ttl_ms).await}
            .map_err(|error| {
                refusal(match error {
                    ReservationError::QuotaExceeded => Refusal::Tokens,
                    ReservationError::Unavailable => Refusal::Unavailable,
                    ReservationError::Duplicate => Refusal::Duplicate,
                })
            })?;
        let mut global = SlotPlan::default();
        let mut scoped = SlotPlan::default();
        for budget in budgets
            .iter()
            .filter(|budget| budget.enforced && budget.shared)
        {
            global
                .capacities
                .push((self.capacity.shared_key(&budget.key), budget.limit));
            scoped.capacities.push((
                self.capacity.tenant_key(tenant, &budget.key),
                self.capacity.tenant_limit_for(budget, tenant)?,
            ));
        }
        policies(
            &self.rates,
            &service,
            tenant,
            user,
            &self.epoch,
            &mut global,
            &mut scoped,
        )
        .map_err(refusal)?;
        if let Some(limit) = prepared.rpm_limit {
            // Identical key and GCRA shape to standalone quota checks. Adapter
            // preparation reuses the dispatch-owned proof rather than charging twice.
            scoped
                .rate(
                    format!("gateway:quota-rpm:{}:{}", tenant_tag(tenant), hex(user)),
                    &RateLimit::new(limit, 60),
                )
                .map_err(refusal)?;
        }
        let retention = service
            .policy
            .timeout
            .as_millis()
            .checked_mul(service.policy.max_attempts() as u128)
            .and_then(|n| n.checked_add(self.ttl_ms as u128 + 1000))
            .filter(|n| *n <= 86_400_000)
            .ok_or_else(|| refusal(Refusal::Unavailable))? as i64;
        global.rate_dedupe = Some((
            format!("gateway:rate-dedupe:{{{}}}:global", global_tag(&self.epoch)),
            retention,
        ));
        scoped.rate_dedupe = Some((
            format!(
                "gateway:rate-dedupe:{}:{}",
                tenant_tag(tenant),
                hex(&self.epoch)
            ),
            retention,
        ));
        let has_rpm = prepared.rpm_limit.is_some();
        scoped.token_key = Some(format!(
            "gateway:dispatch-token:{}:{}:{}",
            tenant_tag(tenant),
            hex(&self.epoch),
            hex(user)
        ));
        scoped.tokens = Some(prepared.envelope.clone());
        let state = Arc::new(LeaseState::default());
        // Install cleanup ownership BEFORE any Redis request: an uncertain
        // network reply can still have allocated a remote capacity member.
        let resources = Resources {
            local,
            id,
            global,
            scoped,
            connection: self.connection.clone(),
            reservations: self.reservations.clone(),
            state: state.clone(),
            renew: None,
            quota_proof: None,
        };
        let mut lease = AtomicLease {
            resources: Some(resources),
        };
        let r = lease.resources.as_ref().expect("owned resources");
        r.global
            .acquire_logical(&r.connection, &r.id.member, self.ttl_ms, &logical_id)
            .await
            .map_err(refusal)?;
        let used = r
            .scoped
            .acquire_logical(&r.connection, &r.id.member, self.ttl_ms, &logical_id)
            .await
            .map_err(refusal)?;
        if has_rpm {
            lease
                .resources
                .as_mut()
                .expect("owned resources")
                .quota_proof = Some(AdmittedQuota::new(
                tenant.into(),
                user.into(),
                crate::billing::MinuteQuotaCheck {
                    allowed: true,
                    used,
                    retry_after_seconds: 0,
                },
            ));
        }
        // A successful uncertain/abandoned commit is still only PREPARED and
        // cannot dispatch without the separate acknowledged state transition.
        prepared
            .commit()
            .await
            .map_err(|_| refusal(Refusal::Unavailable))?;
        lease.start_renew(self.ttl_ms);
        Ok(Box::new(lease))
    }
}
fn estimate(request: &UnifiedRequest) -> Result<i64, Refusal> {
    let input = crate::usage_parser::estimate_tokens(&crate::usage_parser::inputs_to_text(request));
    let p = request.parameters.as_ref();
    // Missing explicit output bounds get the conservative platform envelope;
    // this is an admission estimate, never an observed/billed token count.
    let output = p
        .and_then(|p| p.get("max_output_tokens").or_else(|| p.get("max_tokens")))
        .map(|v| v.as_i64().filter(|n| *n >= 0).ok_or(Refusal::Unavailable))
        .transpose()?
        .unwrap_or(32768);
    input
        .checked_add(output)
        .filter(|n| *n <= 1_000_000_000_000_000)
        .ok_or(Refusal::Unavailable)
}
fn policies(
    config: &RateLimitConfig,
    service: &DispatchService,
    tenant: &str,
    user: &str,
    epoch: &str,
    global: &mut SlotPlan,
    scoped: &mut SlotPlan,
) -> Result<(), Refusal> {
    if let Some(rate) = &config.global_limit {
        global.rate(
            format!("gateway:rate:{{{}}}:global", global_tag(epoch)),
            rate,
        )?;
    }
    let prefix = format!("gateway:rate:{}:{}", tenant_tag(tenant), hex(epoch));
    if let Some(rate) = config.tenant_limits.get(tenant) {
        scoped.rate(format!("{prefix}:tenant"), rate)?;
    }
    if let Some(rate) = config.user_limits.get(user) {
        scoped.rate(format!("{prefix}:user:{}", hex(user)), rate)?;
    }
    if let Some(rate) = config.service_limits.get(&service.service_id) {
        scoped.rate(
            format!("{prefix}:service:{}", hex(&service.service_id)),
            rate,
        )?;
    }
    let configured = &service.capacity_config["service_config"]["rate_limit"];
    let rate = if configured["enabled"].as_bool() == Some(true) {
        Some(configured)
    } else {
        service
            .capacity_config
            .get("rate_limit")
            .filter(|v| !v.is_null() && **v != serde_json::json!({}))
    };
    if let Some(rate) = rate {
        let rate: RateLimit =
            serde_json::from_value(rate.clone()).map_err(|_| Refusal::Unavailable)?;
        scoped.rate(
            format!("{prefix}:service:{}:configured", hex(&service.service_id)),
            &rate,
        )?;
    }
    Ok(())
}
fn refusal(error: Refusal) -> DispatchError {
    let (code, status, retry) = match error {
        Refusal::Capacity => ("GATEWAY_SHARED_CAPACITY_EXHAUSTED", 429, 1),
        Refusal::Rate(retry) => ("GATEWAY_RATE_LIMIT_EXCEEDED", 429, retry),
        Refusal::Tokens => ("GATEWAY_TOKEN_BUDGET_EXHAUSTED", 429, 1),
        Refusal::Unavailable => ("GATEWAY_ADMISSION_UNAVAILABLE", 503, 1),
        Refusal::Duplicate => ("GATEWAY_REQUEST_IN_FLIGHT", 409, 1),
    };
    AdmissionError::Rejected(Box::new(rejected(
        "dispatch",
        code,
        Some("Gateway admission refused".into()),
        status,
        0.0,
        retry,
    )))
    .into()
}
impl DispatchAdmission for AtomicDispatchAdmission {
    fn acquire_provider_batch<'a>(&'a self,_marker:&'a crate::provider_batch_lane::BatchAdmission,context:&'a DispatchContext,service:&'a DispatchService,budgets:&'a [CapacityBudget],request:&'a UnifiedRequest,logical_id:&'a str)->DispatchFuture<'a,Box<dyn DispatchLease>> {
        let this=self.clone();let context=context.clone();let service=service.clone();let budgets=budgets.to_vec();let request=request.clone();let logical_id=logical_id.to_owned();
        Box::pin(async move{tokio::spawn(async move{this.acquire_owned(context,service,"provider_batch".into(),budgets,Some(request),logical_id,true).await}).await.map_err(|_|DispatchError::CleanupFailed)?})
    }
    fn acquire<'a>(
        &'a self,
        _: &'a DispatchContext,
        _: &'a str,
        _: &'a str,
        _: &'a [CapacityBudget],
    ) -> DispatchFuture<'a, Box<dyn DispatchLease>> {
        Box::pin(async { Err(refusal(Refusal::Unavailable)) })
    }
    fn acquire_for_request<'a>(
        &'a self,
        context: &'a DispatchContext,
        service: &'a DispatchService,
        class: &'a str,
        budgets: &'a [CapacityBudget],
        request: Option<&'a UnifiedRequest>,
        logical_id: &'a str,
    ) -> DispatchFuture<'a, Box<dyn DispatchLease>> {
        let this = self.clone();
        let context = context.clone();
        let service = service.clone();
        let class = class.to_owned();
        let budgets = budgets.to_vec();
        let request = request.cloned();
        let logical_id = logical_id.to_owned();
        Box::pin(async move {
            // Dropped acquisition futures cannot abandon successful allocations.
            // An unobserved JoinHandle output drops the owning lease and cleans up.
            tokio::spawn(async move {
                this.acquire_owned(context, service, class, budgets, request, logical_id, false)
                    .await
            })
            .await
            .map_err(|_| DispatchError::CleanupFailed)?
        })
    }
}
#[derive(Default)]
struct LeaseState {
    lost: AdmissionCancellation,
    stop: AdmissionCancellation,
    owner: Mutex<Option<AdmissionCancellation>>,
}
struct Resources {
    local: CapacityLease,
    id: ReservationIdentity,
    global: SlotPlan,
    scoped: SlotPlan,
    connection: ConnectionManager,
    reservations: ReservationStore,
    state: Arc<LeaseState>,
    renew: Option<JoinHandle<()>>,
    quota_proof: Option<AdmittedQuota>,
}
struct AtomicLease {
    resources: Option<Resources>,
}
impl AtomicLease {
    fn start_renew(&mut self, ttl_ms: i64) {
        let r = self.resources.as_ref().expect("live lease");
        let (global, scoped, connection, member, state) = (
            r.global.clone(),
            r.scoped.clone(),
            r.connection.clone(),
            r.id.member.clone(),
            r.state.clone(),
        );
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {biased; _=state.stop.cancelled()=>return,
                _=tokio::time::sleep(Duration::from_millis((ttl_ms/3) as u64))=>{}}
                let (a, b) = tokio::join!(
                    global.renew(&connection, &member, ttl_ms),
                    scoped.renew(&connection, &member, ttl_ms)
                );
                if a.is_err() || b.is_err() {
                    state.lost.cancel();
                    if let Some(owner) = state
                        .owner
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_ref()
                    {
                        owner.cancel();
                    }
                    return;
                }
            }
        });
        self.resources.as_mut().expect("live lease").renew = Some(handle);
    }
}
impl DispatchLease for AtomicLease {
    fn quota_admission(&self) -> Option<AdmittedQuota> {
        self.resources.as_ref().and_then(|r| r.quota_proof.clone())
    }
    fn bind_owner(&self, owner: AdmissionCancellation) -> Result<(), DispatchError> {
        let r = self
            .resources
            .as_ref()
            .ok_or(DispatchError::CleanupFailed)?;
        r.local.bind_owner(owner.clone())?;
        *r.state.owner.lock().unwrap_or_else(|e| e.into_inner()) = Some(owner.clone());
        if r.state.lost.is_cancelled() {
            owner.cancel();
            return Err(refusal(Refusal::Unavailable));
        }
        Ok(())
    }
    fn mark_dispatched(&self) -> DispatchFuture<'_, ()> {
        Box::pin(async move {
            let r = self
                .resources
                .as_ref()
                .ok_or(DispatchError::CleanupFailed)?;
            if r.state.lost.is_cancelled()
                || r.state
                    .owner
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .is_some_and(AdmissionCancellation::is_cancelled)
            {
                return Err(DispatchError::Cancelled);
            }
            r.reservations
                .mark_dispatched(&r.id)
                .await
                .map_err(|_| refusal(Refusal::Unavailable))
        })
    }
    fn mark_completion(&self, outcome: DispatchSettlement) -> DispatchFuture<'_, ()> {
        Box::pin(async move {
            let r = self
                .resources
                .as_ref()
                .ok_or(DispatchError::CleanupFailed)?;
            r.reservations
                .mark_outcome(&r.id, outcome)
                .await
                .map_err(|_| refusal(Refusal::Unavailable))
        })
    }
    fn wait_lost(&self) -> DispatchFuture<'_, ()> {
        Box::pin(async move {
            let r = self
                .resources
                .as_ref()
                .ok_or(DispatchError::CleanupFailed)?;
            tokio::select! {_=r.state.lost.cancelled()=>Err(refusal(Refusal::Unavailable)),_=r.local.wait_lost()=>Err(r.local.lost_error().into())}
        })
    }
    fn release(mut self: Box<Self>) -> DispatchFuture<'static, ()> {
        let resources = self.resources.take();
        Box::pin(async move {
            if let Some(resources) = resources {
                tokio::spawn(cleanup(resources))
                    .await
                    .map_err(|_| DispatchError::CleanupFailed)?
            } else {
                Ok(())
            }
        })
    }
}
async fn cleanup(mut r: Resources) -> Result<(), DispatchError> {
    r.state.stop.cancel();
    if let Some(handle) = r.renew.take() {
        let _ = handle.await;
    }
    let (a, b, c) = tokio::join!(
        r.global.release(&r.connection, &r.id.member),
        r.scoped.release(&r.connection, &r.id.member),
        r.reservations.finish(&r.id)
    );
    r.local.release().await;
    if a.is_err() || b.is_err() || c.is_err() {
        Err(DispatchError::CleanupFailed)
    } else {
        Ok(())
    }
}
impl Drop for AtomicLease {
    fn drop(&mut self) {
        if let Some(r) = self.resources.take() {
            r.state.stop.cancel();
            tokio::spawn(cleanup(r));
        }
    }
}
#[cfg(test)]
#[path = "atomic_tests.rs"]
mod tests;

#[path = "model_plane.rs"]
pub mod model_plane;
