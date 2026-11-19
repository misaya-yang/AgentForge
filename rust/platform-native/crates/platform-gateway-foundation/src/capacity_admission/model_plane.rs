//! Shared model-plane admission over ALREADY COMMITTED canonical model-call
//! reservations. There is no caller boolean to skip budget checks, no second
//! reservation, and no price arithmetic. Proofs can only be minted by scoped PG
//! reads and are revalidated after the owner's sole-POST CAS.
use super::store::{Refusal, SlotPlan, hex, tenant_tag};
use super::{AtomicDispatchAdmission, global_tag, policies, refusal};
use crate::{
    billing,
    capacity::CapacityBudget,
    capacity_admission::{AdmissionCancellation, AdmissionRequest},
    dispatcher::{DispatchError, DispatchService},
    quota_store::reservations::TokenEnvelope,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
#[path = "model_plane_lease.rs"]
mod lease;
pub use lease::ModelPlaneLease;
#[derive(Clone)]
pub struct ModelPlaneAdmission {
    capacity: std::sync::Weak<super::super::ControllerInner>,
    connection: redis::aio::ConnectionManager,
    rates: crate::dispatcher_rate_limiter::RateLimitConfig,
    epoch: String,
    ttl_ms: i64,
    pool: PgPool,
}
/// Exact authenticated call binding; values are compared with the durable row.
/// Supplying strings never certifies that a budget was reserved.
#[derive(Clone)]
pub struct ModelCallBinding {
    pub tenant: String,
    pub user: String,
    pub reservation_id: String,
    pub provider: String,
    pub model: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelTokenReservation {
    Tokens { input: i64, output: i64 },
    NotApplicable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Assistant,
    Knowledge,
}
#[derive(Clone)]
struct Proof {
    owner: Owner,
    binding: ModelCallBinding,
    immutable: Value,
    units: ModelTokenReservation,
    envelope: TokenEnvelope,
    rpm: Option<i64>,
    deadline_ms: i64,
}
impl ModelPlaneAdmission {
    /// Explicit bootstrap installation. The back-reference is weak, so the
    /// controller can own this capability without a reference cycle.
    pub fn install(shared: &AtomicDispatchAdmission, pool: PgPool) -> bool {
        let capability = Arc::new(Self {
            capacity: Arc::downgrade(&shared.capacity.inner),
            connection: shared.connection.clone(),
            rates: shared.rates.clone(),
            epoch: shared.epoch.clone(),
            ttl_ms: shared.ttl_ms,
            pool,
        });
        shared.capacity.install_model_plane(capability)
    }
    pub async fn acquire_assistant(
        &self,
        binding: ModelCallBinding,
        budgets: &[CapacityBudget],
    ) -> Result<Arc<ModelPlaneLease>, DispatchError> {
        self.acquire(Owner::Assistant, binding, budgets).await
    }
    pub async fn acquire_knowledge(
        &self,
        binding: ModelCallBinding,
        budgets: &[CapacityBudget],
    ) -> Result<Arc<ModelPlaneLease>, DispatchError> {
        self.acquire(Owner::Knowledge, binding, budgets).await
    }
    async fn acquire(
        &self,
        owner: Owner,
        binding: ModelCallBinding,
        budgets: &[CapacityBudget],
    ) -> Result<Arc<ModelPlaneLease>, DispatchError> {
        let this = self.clone();
        let budgets = budgets.to_vec();
        // As in generic dispatch, abandoning this waiter cannot abandon an
        // uncertain remote allocation. The owned task output owns its cleanup.
        tokio::spawn(async move { this.acquire_owned(owner, binding, budgets).await })
            .await
            .map_err(|_| DispatchError::CleanupFailed)?
    }
    async fn acquire_owned(
        &self,
        owner: Owner,
        binding: ModelCallBinding,
        budgets: Vec<CapacityBudget>,
    ) -> Result<Arc<ModelPlaneLease>, DispatchError> {
        let proof = self.prove(owner, binding, "reserved").await?;
        let capacity = crate::capacity_admission::CapacityAdmissionController {
            inner: self
                .capacity
                .upgrade()
                .ok_or_else(|| refusal(Refusal::Unavailable))?,
        };
        let service = DispatchService::new(match owner {
            Owner::Assistant => "agent-runtime-provider",
            Owner::Knowledge => "knowledge-model-plane",
        });
        let request = AdmissionRequest {
            tenant_id: proof.binding.tenant.clone(),
            user_id: proof.binding.user.clone(),
            service_id: service.service_id.clone(),
            request_class: "provider".into(),
            request_id: proof.binding.reservation_id.clone(),
            ..Default::default()
        };
        let local = capacity.acquire_local_only(&budgets, &request).await?;
        let mut global = SlotPlan::default();
        let mut tenant = SlotPlan::default();
        for budget in budgets
            .iter()
            .filter(|budget| budget.enforced && budget.shared)
        {
            global
                .capacities
                .push((capacity.shared_key(&budget.key), budget.limit));
            tenant.capacities.push((
                capacity.tenant_key(&proof.binding.tenant, &budget.key),
                capacity.tenant_limit_for(budget, &proof.binding.tenant)?,
            ));
        }
        policies(
            &self.rates,
            &service,
            &proof.binding.tenant,
            &proof.binding.user,
            &self.epoch,
            &mut global,
            &mut tenant,
        )
        .map_err(refusal)?;
        if let Some(rpm) = proof.rpm {
            tenant
                .rate(
                    format!(
                        "gateway:quota-rpm:{}:{}",
                        tenant_tag(&proof.binding.tenant),
                        hex(&proof.binding.user)
                    ),
                    &crate::dispatcher_rate_limiter::RateLimit::new(rpm, 60),
                )
                .map_err(refusal)?;
        }
        // The budget owner has already reserved these tokens transactionally.
        // estimate=0 prevents charging them again; used/pending/limits come from
        // its durable counters, not from this caller or an already_reserved flag.
        tenant.tokens = Some(proof.envelope.clone());
        tenant.token_key = Some(format!(
            "gateway:dispatch-token:{}:{}:{}",
            tenant_tag(&proof.binding.tenant),
            hex(&self.epoch),
            hex(&proof.binding.user)
        ));
        let retention = proof.deadline_ms.clamp(self.ttl_ms, 86_400_000);
        global.rate_dedupe = Some((
            format!("gateway:rate-dedupe:{{{}}}:global", global_tag(&self.epoch)),
            retention,
        ));
        tenant.rate_dedupe = Some((
            format!(
                "gateway:rate-dedupe:{}:{}",
                tenant_tag(&proof.binding.tenant),
                hex(&self.epoch)
            ),
            retention,
        ));
        let member = uuid::Uuid::new_v4().to_string();
        let logical = format!("{:?}:{}", owner, proof.binding.reservation_id);
        let guard = ModelPlaneLease::new(
            local,
            global,
            tenant,
            self.connection.clone(),
            self.clone(),
            proof,
            member,
        );
        guard.acquire_remote(&logical, self.ttl_ms).await?;
        guard.start_renew(self.ttl_ms);
        Ok(guard)
    }
    async fn prove(
        &self,
        owner: Owner,
        binding: ModelCallBinding,
        state: &str,
    ) -> Result<Proof, DispatchError> {
        tokio::time::timeout(
            Duration::from_secs(3),
            self.prove_inner(owner, binding, state),
        )
        .await
        .map_err(|_| refusal(Refusal::Unavailable))?
    }
    async fn prove_inner(
        &self,
        owner: Owner,
        binding: ModelCallBinding,
        state: &str,
    ) -> Result<Proof, DispatchError> {
        let denied = || refusal(Refusal::Unavailable);
        if [
            &binding.tenant,
            &binding.user,
            &binding.provider,
            &binding.model,
        ]
        .iter()
        .any(|s| s.is_empty())
            || uuid::Uuid::parse_str(&binding.reservation_id).is_err()
        {
            return Err(denied());
        }
        let row:Option<Value>=match owner {
            Owner::Assistant=>sqlx::query_scalar("SELECT jsonb_build_object('call',to_jsonb(c),'lease',to_jsonb(l),'quota',to_jsonb(q),'remaining_ms',floor(extract(epoch FROM (l.expires_at-NOW()))*1000)::bigint) FROM assistant.assistant_runtime_model_calls c JOIN assistant.assistant_runtime_model_leases l ON l.lease_id=c.lease_id AND l.tenant_id=c.tenant_id AND l.user_id=c.user_id AND l.run_id=c.run_id AND l.snapshot_id=c.snapshot_id JOIN assistant.assistant_runs r ON r.run_id=c.run_id LEFT JOIN gateway.user_quotas q ON q.tenant_id=c.tenant_id AND q.user_id=c.user_id WHERE c.call_id=$1::text::uuid AND c.tenant_id=$2 AND c.user_id=$3 AND l.provider_id=$4 AND l.model_id=$5 AND c.status=$6 AND l.status='active' AND l.expires_at>NOW() AND r.status='running' AND NOT EXISTS(SELECT 1 FROM assistant.assistant_runtime_snapshot_revocations x WHERE x.snapshot_id=c.snapshot_id)")
                .bind(&binding.reservation_id).bind(&binding.tenant).bind(&binding.user).bind(&binding.provider).bind(&binding.model).bind(state)
                .fetch_optional(&self.pool).await.map_err(|_|denied())?,
            Owner::Knowledge=>sqlx::query_scalar("SELECT jsonb_build_object('call',to_jsonb(c),'quota',to_jsonb(q),'remaining_ms',floor(extract(epoch FROM (c.expires_at-NOW()))*1000)::bigint) FROM gateway.knowledge_model_calls c LEFT JOIN gateway.user_quotas q ON q.tenant_id=c.tenant_id AND q.user_id=c.user_id WHERE c.lease_id=$1::text::uuid AND c.tenant_id=$2 AND c.user_id=$3 AND c.call->'model'->>'provider'=$4 AND c.call->'model'->>'model'=$5 AND c.state=$6 AND c.expires_at>NOW()")
                .bind(&binding.reservation_id).bind(&binding.tenant).bind(&binding.user).bind(&binding.provider).bind(&binding.model).bind(state)
                .fetch_optional(&self.pool).await.map_err(|_|denied())?,
        };
        proof_from_row(owner, binding, row.ok_or_else(denied)?)
    }
    async fn confirm_dispatched(&self, proof: &Proof) -> Result<(), DispatchError> {
        let current = self
            .prove(proof.owner, proof.binding.clone(), "dispatched")
            .await?;
        if current.immutable != proof.immutable || current.units != proof.units {
            return Err(refusal(Refusal::Unavailable));
        }
        Ok(())
    }
}
fn number(value: &Value, key: &str) -> Result<i64, DispatchError> {
    value[key]
        .as_i64()
        .filter(|n| *n >= 0)
        .ok_or_else(|| refusal(Refusal::Unavailable))
}
fn proof_from_row(
    owner: Owner,
    binding: ModelCallBinding,
    row: Value,
) -> Result<Proof, DispatchError> {
    let denied = || refusal(Refusal::Unavailable);
    let call = &row["call"];
    let quota = &row["quota"];
    if call["tenant_id"].as_str() != Some(binding.tenant.as_str())
        || call["user_id"].as_str() != Some(binding.user.as_str())
    {
        return Err(denied());
    }
    match owner {
        Owner::Assistant
            if call["call_id"].as_str() != Some(binding.reservation_id.as_str())
                || row["lease"]["provider_id"].as_str() != Some(binding.provider.as_str())
                || row["lease"]["model_id"].as_str() != Some(binding.model.as_str()) =>
        {
            return Err(denied());
        }
        Owner::Knowledge
            if call["lease_id"].as_str() != Some(binding.reservation_id.as_str())
                || call["call"]["model"]["provider"].as_str()
                    != Some(binding.provider.as_str())
                || call["call"]["model"]["model"].as_str() != Some(binding.model.as_str()) =>
        {
            return Err(denied());
        }
        _ => {}
    }
    let mut envelope = TokenEnvelope::default();
    let (units, immutable) = match owner {
        Owner::Assistant => {
            let lease = &row["lease"];
            let input = number(call, "estimated_input_tokens")?;
            let output = number(call, "reserved_output_tokens")?;
            let _ = number(lease, "used_input_tokens")?;
            let _ = number(lease, "used_output_tokens")?;
            let ri = number(lease, "reserved_input_tokens")?;
            let ro = number(lease, "reserved_output_tokens")?;
            let mi = number(lease, "max_input_tokens")?;
            let mo = number(lease, "max_output_tokens")?;
            if ri < input
                || ro < output
                || ri > mi
                || ro > mo
                || number(lease, "reserved_cost_microusd")? > number(lease, "max_cost_microusd")?
            {
                return Err(denied());
            }
            // Runtime source reservations are cumulative lifetime counters,
            // not outstanding-plus-used counters. Never add used a second time.
            envelope.daily_used = 0;
            envelope.pending = ri.checked_add(ro).ok_or_else(denied)?;
            envelope.daily_limit = Some(mi.checked_add(mo).ok_or_else(denied)?);
            (
                ModelTokenReservation::Tokens { input, output },
                json!([
                    call["call_id"],
                    call["lease_id"],
                    call["request_sha256"],
                    call["snapshot_id"],
                    input,
                    output,
                    call["reserved_cost_microusd"]
                ]),
            )
        }
        Owner::Knowledge => {
            let input = number(call, "reserved_input")?;
            let output = number(call, "reserved_output")?;
            let request_meter = crate::request_meter::is_snapshot(&call["pricing"]);
            let units = if request_meter {
                if input != 0 || output != 0 {
                    return Err(denied());
                }
                ModelTokenReservation::NotApplicable
            } else {
                ModelTokenReservation::Tokens { input, output }
            };
            // Knowledge's source authority has precharged current quota counters.
            // Preserve temporary boosts and canonical overage policy exactly.
            if !quota.is_object() {
                return Err(denied());
            }
            let limits = billing::effective_quota_limits(
                quota["daily_token_limit"].as_i64(),
                quota["monthly_token_limit"].as_i64(),
                quota["monthly_cost_limit_cents"].as_i64(),
                number(quota, "temporary_extra_tokens")?,
                number(quota, "temporary_extra_cost_cents")?,
                quota["temporary_expires_at"]
                    .as_str()
                    .map(chrono::DateTime::parse_from_rfc3339)
                    .transpose()
                    .map_err(|_| denied())?
                    .map(|v| v.with_timezone(&chrono::Utc)),
                chrono::Utc::now(),
            )
            .map_err(|_| denied())?;
            envelope.daily_used = number(quota, "current_daily_tokens")?;
            envelope.monthly_used = number(quota, "current_monthly_tokens")?;
            if matches!(
                quota["overage_strategy"].as_str(),
                Some("hard_block" | "rate_limit")
            ) {
                envelope.daily_limit = limits.daily_tokens.filter(|n| *n != 0);
                envelope.monthly_limit = limits.monthly_tokens.filter(|n| *n != 0);
            }
            (
                units,
                json!([
                    call["lease_id"],
                    call["request_sha256"],
                    call["call_id"],
                    call["pricing"],
                    input,
                    output,
                    call["reserved_cost_nanos"]
                ]),
            )
        }
    };
    if quota["is_blocked"].as_bool() == Some(true) {
        return Err(denied());
    }
    let deadline_ms = row["remaining_ms"]
        .as_i64()
        .filter(|n| *n > 0 && *n <= 86_400_000)
        .ok_or_else(denied)?;
    Ok(Proof {
        owner,
        binding,
        immutable,
        units,
        envelope,
        rpm: billing::normalize_limit(quota["requests_per_minute"].as_i64()),
        deadline_ms,
    })
}

#[cfg(test)]
#[path = "model_plane_tests.rs"]
mod tests;
