//! Explicit tenant guardrail policy over current tenant metadata and audit logs.
//! Local detectors report bounded evidence, never raw prompts, matches or secrets.
// SPDX-License-Identifier: Apache-2.0
pub mod contract;
mod detect;
mod policy;
mod routes;
#[cfg(test)]
mod tests;
use super::{LlmError, LlmResult};
use policy::CompiledPolicy;
pub use policy::{Detector, Direction, Mode, Policy, Rule, Stage, StagePolicy};
pub use routes::router;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Block,
    BudgetExceeded,
    Unavailable,
}
#[derive(Clone, Serialize)]
pub struct Finding {
    pub rule: String,
    pub location: String,
}
#[derive(Clone, Serialize)]
pub struct StageReport {
    pub stage: Stage,
    pub mode: Mode,
    pub score: u32,
    pub triggered: bool,
    pub findings: Vec<Finding>,
    pub elapsed_us: u64,
}
#[derive(Clone, Serialize)]
pub struct Report {
    pub schema: String,
    pub policy_pin: String,
    pub policy_version: String,
    pub direction: Direction,
    pub decision: Decision,
    pub reason: String,
    pub stages: Vec<StageReport>,
    pub elapsed_us: u64,
    pub audit_state: String,
    pub audit_us: Option<u64>,
    pub holdback_us: u64,
}
#[derive(Clone)]
pub struct GuardrailService {
    pool: PgPool,
    #[cfg(test)]
    test_audit: Option<Arc<Mutex<Vec<Report>>>>,
}
#[derive(Clone)]
pub(crate) struct Context {
    service: GuardrailService,
    policy: Arc<CompiledPolicy>,
    tenant: String,
    actor: String,
    request: String,
    pub input: Report,
    output: Arc<Mutex<Option<Report>>>,
}
impl Context {
    pub fn output_enabled(&self) -> bool {
        self.policy
            .stages
            .iter()
            .any(|s| s.policy.directions.contains(&Direction::Output))
    }
    pub async fn buffer_permit(&self) -> LlmResult<tokio::sync::OwnedSemaphorePermit> {
        static BUFFERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        tokio::time::timeout(
            Duration::from_millis(500),
            BUFFERS
                .get_or_init(|| Arc::new(Semaphore::new(4)))
                .clone()
                .acquire_owned(),
        )
        .await
        .map_err(|_| LlmError::new(503, "llm_guardrails_buffer_busy"))?
        .map_err(|_| policy::invalid())
    }
    pub async fn reject_output_limit(&self, holdback_us: u64) -> LlmResult<()> {
        let mut report = unavailable(&self.policy, Direction::Output);
        report.decision = Decision::Block;
        report.reason = "payload_limit".into();
        report.holdback_us = holdback_us;
        report.audit_state = "confirmed".into();
        self.service
            .audit(&self.tenant, &self.actor, &self.request, &report)
            .await?;
        if let Ok(mut slot) = self.output.lock() {
            *slot = Some(report)
        }
        Ok(())
    }
    pub fn output_limit(&self) -> usize {
        self.policy.policy.max_output_bytes as usize
    }
    pub fn output_report(&self) -> Option<Report> {
        self.output.lock().ok().and_then(|r| r.clone())
    }
    pub async fn inspect_output(&self, bytes: &[u8], sse: bool, holdback_us: u64) -> LlmResult<()> {
        let report = self
            .service
            .check(
                self.policy.clone(),
                Direction::Output,
                bytes.to_vec(),
                sse,
                &self.tenant,
                &self.actor,
                &self.request,
                Instant::now(),
                holdback_us,
            )
            .await;
        match report {
            Ok(r) => {
                let result = accepted(&r);
                if let Ok(mut slot) = self.output.lock() {
                    *slot = Some(r)
                }
                result
            }
            Err(e) => {
                if let Ok(mut slot) = self.output.lock() {
                    *slot = Some(unavailable(&self.policy, Direction::Output))
                }
                Err(e)
            }
        }
    }
}
fn accepted(r: &Report) -> LlmResult<()> {
    match r.decision {
        Decision::Allow => Ok(()),
        Decision::Block => Err(LlmError::new(403, "llm_guardrails_rejected")),
        Decision::BudgetExceeded => Err(LlmError::new(503, "llm_guardrails_budget_exceeded")),
        Decision::Unavailable => Err(LlmError::new(503, "llm_guardrails_unavailable")),
    }
}
fn unavailable(p: &CompiledPolicy, direction: Direction) -> Report {
    Report {
        schema: "agentforge.guardrail_decision/v1".into(),
        policy_pin: p.pin.clone(),
        policy_version: p.policy.version.clone(),
        direction,
        decision: Decision::Unavailable,
        reason: "inspection_unavailable".into(),
        stages: vec![],
        elapsed_us: 0,
        audit_state: "unconfirmed".into(),
        audit_us: None,
        holdback_us: 0,
    }
}
fn slots() -> Arc<Semaphore> {
    static S: OnceLock<Arc<Semaphore>> = OnceLock::new();
    S.get_or_init(|| Arc::new(Semaphore::new(4))).clone()
}
impl GuardrailService {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            #[cfg(test)]
            test_audit: None,
        }
    }
    pub(crate) async fn inspect_input(
        &self,
        tenant: &str,
        actor: &str,
        request: &str,
        bytes: &[u8],
    ) -> LlmResult<Option<Context>> {
        let start = Instant::now();
        let raw:Option<Value>=sqlx::query_scalar("SELECT metadata->'guardrails_v1' FROM gateway.tenants WHERE tenant_id=$1 AND status='active'").bind(tenant).fetch_optional(&self.pool).await.map_err(|_|LlmError::new(503,"llm_guardrails_unavailable"))?.ok_or(LlmError::new(403,"llm_guardrails_tenant_inactive"))?;
        let Some(raw) = raw.filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let policy = match serde_json::from_value::<Policy>(raw) {
            Ok(p) => p,
            Err(_) => {
                self.audit_fault(tenant, actor, request, "invalid_policy")
                    .await?;
                return Err(policy::invalid());
            }
        };
        let compiled = match compile_policy(policy).await {
            Ok(p) => p,
            Err(error) => {
                self.audit_fault(tenant, actor, request, "invalid_or_timed_out_policy")
                    .await?;
                return Err(error);
            }
        };
        if !compiled.policy.enabled {
            return Ok(None);
        }
        let policy = Arc::new(compiled);
        let report = self
            .check(
                policy.clone(),
                Direction::Input,
                bytes.to_vec(),
                false,
                tenant,
                actor,
                request,
                start,
                0,
            )
            .await?;
        accepted(&report)?;
        Ok(Some(Context {
            service: self.clone(),
            policy,
            tenant: tenant.into(),
            actor: actor.into(),
            request: request.into(),
            input: report,
            output: Arc::new(Mutex::new(None)),
        }))
    }
    async fn check(
        &self,
        p: Arc<CompiledPolicy>,
        direction: Direction,
        bytes: Vec<u8>,
        sse: bool,
        tenant: &str,
        actor: &str,
        request: &str,
        start: Instant,
        holdback_us: u64,
    ) -> LlmResult<Report> {
        let limit = match direction {
            Direction::Input => p.policy.max_input_bytes,
            Direction::Output => p.policy.max_output_bytes,
        };
        let mut report = if bytes.len() > limit as usize {
            let mut r = unavailable(&p, direction);
            r.decision = Decision::Block;
            r.reason = "payload_limit".into();
            r.audit_state = "pending".into();
            r
        } else {
            let budget = Duration::from_millis(u64::from(p.policy.budget_ms));
            let remaining = budget.saturating_sub(start.elapsed());
            let job = async {
                let permit = slots()
                    .acquire_owned()
                    .await
                    .map_err(|_| policy::invalid())?;
                let p = p.clone();
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let f = detect::output(&bytes, sse)?;
                    Ok::<_, LlmError>(detect::evaluate(&p, direction, f, start))
                })
                .await
                .map_err(|_| policy::invalid())?
            };
            match tokio::time::timeout(remaining, job).await {
                Ok(Ok(r)) => r,
                Ok(Err(_)) => unavailable(&p, direction),
                Err(_) => {
                    let mut r = unavailable(&p, direction);
                    r.decision = Decision::BudgetExceeded;
                    r.reason = "latency_budget".into();
                    r.audit_state = "pending".into();
                    r
                }
            }
        };
        report.elapsed_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
        report.holdback_us = holdback_us;
        report.audit_state = "confirmed".into();
        let audit_start = Instant::now();
        self.audit(tenant, actor, request, &report).await?;
        report.audit_state = "confirmed".into();
        report.audit_us =
            Some(u64::try_from(audit_start.elapsed().as_micros()).unwrap_or(u64::MAX));
        Ok(report)
    }
    async fn audit_fault(
        &self,
        tenant: &str,
        actor: &str,
        request: &str,
        reason: &str,
    ) -> LlmResult<()> {
        let report = Report {
            schema: "agentforge.guardrail_decision/v1".into(),
            policy_pin: "unknown".into(),
            policy_version: "unknown".into(),
            direction: Direction::Input,
            decision: Decision::Unavailable,
            reason: reason.into(),
            stages: vec![],
            elapsed_us: 0,
            audit_state: "confirmed".into(),
            audit_us: None,
            holdback_us: 0,
        };
        self.audit(tenant, actor, request, &report).await
    }
    async fn audit(
        &self,
        tenant: &str,
        actor: &str,
        request: &str,
        report: &Report,
    ) -> LlmResult<()> {
        #[cfg(test)]
        if let Some(reports) = &self.test_audit {
            reports
                .lock()
                .map_err(|_| policy::invalid())?
                .push(report.clone());
            return Ok(());
        }
        // Audit confirmation is a separate fixed bound, measured separately from
        // policy's detection+queue budget. No best-effort success fallback.
        let details = serde_json::to_value(report).map_err(|_| policy::invalid())?;
        tokio::time::timeout(Duration::from_millis(250),sqlx::query("INSERT INTO gateway.audit_logs(event_type,user_id,tenant_id,resource_type,resource_id,action,request_summary,response_summary,status,duration_ms) VALUES('guardrail_decision',$1,$2,'llm_request',$3,'evaluate',$4,$5,$6,$7)").bind(actor).bind(tenant).bind(request).bind(json!({"schema":"agentforge.guardrail_audit/v1","direction":report.direction,"policy_pin":report.policy_pin})).bind(details).bind(if report.decision==Decision::Allow{"success"}else{"blocked"}).bind(i32::try_from(report.elapsed_us/1000).unwrap_or(i32::MAX)).execute(&self.pool)).await.map_err(|_|LlmError::new(503,"llm_guardrails_audit_unconfirmed"))?.map_err(|_|LlmError::new(503,"llm_guardrails_audit_unconfirmed"))?;
        Ok(())
    }
}

async fn compile_policy(policy: Policy) -> LlmResult<CompiledPolicy> {
    let timeout = Duration::from_millis(u64::from(policy.budget_ms.clamp(5, 500)));
    tokio::time::timeout(timeout, async move {
        let permit = slots()
            .acquire_owned()
            .await
            .map_err(|_| policy::invalid())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            policy.compile()
        })
        .await
        .map_err(|_| policy::invalid())?
    })
    .await
    .map_err(|_| LlmError::new(503, "llm_guardrails_budget_exceeded"))?
}

#[cfg(test)]
pub(crate) fn test_context() -> Context {
    let policy = Arc::new(tests::fixture().compile().unwrap());
    let input = detect::evaluate(&policy, Direction::Input, vec![], Instant::now());
    Context {
        service: GuardrailService {
            pool: sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
                .unwrap(),
            test_audit: Some(Arc::new(Mutex::new(vec![]))),
        },
        policy,
        tenant: "t".into(),
        actor: "u".into(),
        request: "guardrails-test".into(),
        input,
        output: Arc::new(Mutex::new(None)),
    }
}
