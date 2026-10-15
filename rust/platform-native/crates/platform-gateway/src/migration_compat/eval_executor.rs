//! Evaluation execution and lease-owned background processing.
use super::{
    common::{CompatError, CompatUser, object, text},
    eval::EvalService,
    eval_clients::LlmCompleteContext,
    eval_queue::{EvalOutbox, is_runnable_dataset_example},
    eval_scoring::EvalScorer,
};
use crate::{
    capacity::{CapacityBudget, CapacityRequest, CapacityResolver},
    capacity_admission::{AdmissionRequest, CapacityAdmissionController},
    migration_trace::TraceRepository,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, watch};
fn trace_error(error: crate::migration_trace::TraceError) -> CompatError {
    CompatError::new(error.status, error.code)
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
pub fn trace_target(detail: &Value) -> Option<Value> {
    let trace = &detail["trace"];
    if text(trace, "trace_id").trim().is_empty() {
        return None;
    }
    let mut target = json!(object(trace));
    target["metadata"] = json!(object(&trace["metadata"]));
    target["metrics"] = json!(object(&trace["metrics"]));
    target["spans"] = json!(array(&detail["spans"]));
    target["events"] = json!(array(&detail["events"]));
    for key in ["input_preview", "output_preview"] {
        if target[key].is_null() {
            target[key] = json!("");
        }
    }
    Some(target)
}
fn family(payload: &Value) -> &str {
    let nested = text(&payload["target_snapshot"], "trace_family").trim();
    if matches!(nested, "assistant" | "langgraph_proxy" | "rag") {
        nested
    } else {
        match text(payload, "trace_family").trim() {
            "langgraph_proxy" => "langgraph_proxy",
            "rag" => "rag",
            _ => "assistant",
        }
    }
}
#[derive(Clone, Debug)]
pub struct EvaluatorRunResult {
    pub run_id: Option<String>,
    pub status: String,
    pub score_summary: Value,
    pub metrics: Value,
    pub error_message: Option<String>,
    pub scores_written: usize,
}
impl EvaluatorRunResult {
    pub(crate) fn terminal(
        run: &str,
        status: &str,
        summary: Value,
        metrics: Value,
        error: Option<String>,
        written: usize,
    ) -> Self {
        Self {
            run_id: if run.is_empty() {
                None
            } else {
                Some(run.into())
            },
            status: status.into(),
            score_summary: summary,
            metrics,
            error_message: error,
            scores_written: written,
        }
    }
}
pub struct EvaluatorExecutor {
    pub service: Arc<EvalService>,
    pub traces: Arc<TraceRepository>,
    pub scorer: Arc<EvalScorer>,
    pub outbox: EvalOutbox,
    pub created_by: String,
    pub candidate: Option<Arc<super::eval_live::EvalCandidateRunner>>,
}
impl EvaluatorExecutor {
    pub async fn update_run(
        &self,
        job: &Value,
        run: &str,
        status: &str,
        summary: Option<&Value>,
        metrics: Option<&Value>,
        error: Option<&str>,
        started: bool,
        finished: bool,
    ) -> Result<(), CompatError> {
        let mut tx = self.service.pool.begin().await?;
        self.outbox.fence(&mut tx, job).await?;
        sqlx::query("UPDATE eval_experiment_runs SET status=$3,score_summary=COALESCE($4::jsonb,score_summary),metrics=COALESCE($5::jsonb,metrics),error_message=$6,started_at=CASE WHEN $7 THEN COALESCE(started_at,NOW()) ELSE started_at END,finished_at=CASE WHEN $8 THEN NOW() ELSE finished_at END,updated_at=NOW() WHERE tenant_id=$1 AND run_id=$2::text::uuid").bind(text(job,"tenant_id")).bind(run).bind(status).bind(summary).bind(metrics).bind(error).bind(started).bind(finished).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub(crate) async fn fail(
        &self,
        job: &Value,
        run: &str,
        error: &str,
    ) -> Result<EvaluatorRunResult, CompatError> {
        self.update_run(job, run, "failed", None, None, Some(error), false, true)
            .await?;
        Ok(EvaluatorRunResult::terminal(
            run,
            "failed",
            json!({}),
            json!({}),
            Some(error.into()),
            0,
        ))
    }
    pub async fn run_job(&self, job: &Value) -> Result<EvaluatorRunResult, CompatError> {
        let tenant = text(job, "tenant_id");
        let payload = &job["payload"];
        let run = text(payload, "run_id");
        let evaluator_id = text(payload, "evaluator_id");
        if run.is_empty() || evaluator_id.is_empty() {
            return Ok(EvaluatorRunResult::terminal(
                run,
                "failed",
                json!({}),
                json!({}),
                Some("Missing run_id or evaluator_id in outbox payload".into()),
                0,
            ));
        }
        self.update_run(job, run, "running", None, None, None, true, false)
            .await?;
        if text(payload, "run_mode") == "live_candidate" {
            return self.run_live_job(job).await;
        }
        if !text(payload, "dataset_id").is_empty() && !text(payload, "trace_id").is_empty() {
            return self
                .fail(job, run, "Dataset and trace targets are mutually exclusive")
                .await;
        }
        let evaluator = if payload["evaluator_snapshot"].is_object() {
            payload["evaluator_snapshot"].clone()
        } else {
            let Some(evaluator) = self.service.get(tenant, "evaluators", evaluator_id).await?
            else {
                return self.fail(job, run, "Evaluator not found").await;
            };
            let evaluator = self.service.freeze_eval_judge(tenant, evaluator).await?;
            let mut tx = self.service.pool.begin().await?;
            self.outbox.fence(&mut tx, job).await?;
            sqlx::query("UPDATE agent_trace_outbox SET payload=jsonb_set(payload,'{evaluator_snapshot}',$2::jsonb) WHERE job_id=$1::text::uuid").bind(text(job,"job_id")).bind(&evaluator).execute(&mut *tx).await?;
            tx.commit().await?;
            evaluator
        };
        let family = family(payload);
        let (targets, expected, unresolved) = self.resolve_targets(job, payload, family).await?;
        if unresolved > 0 {
            return self
                .fail(
                    job,
                    run,
                    &format!("{unresolved} dataset example(s) unresolved"),
                )
                .await;
        }
        if targets.is_empty() {
            return self.fail(job, run, "No evaluation targets resolved").await;
        }
        let kind = if text(&evaluator, "evaluator_type").is_empty() {
            "human"
        } else {
            text(&evaluator, "evaluator_type")
        };
        if kind == "human" {
            let summary = json!({"pending_human":true,"message":"Human evaluator runs require manual scoring.","expected_count":expected,"resolved_count":targets.len(),"executed_count":targets.len(),"scored_count":0,"skipped_count":0});
            self.update_run(
                job,
                run,
                "succeeded",
                Some(&summary),
                Some(&json!({"targets":targets.len()})),
                None,
                false,
                true,
            )
            .await?;
            return Ok(EvaluatorRunResult::terminal(
                run,
                "succeeded",
                summary,
                json!({}),
                None,
                0,
            ));
        }
        let (mut scores, mut labels, mut written, mut skipped, mut infrastructure) =
            (vec![], vec![], 0, 0, false);
        for target in &targets {
            let context = LlmCompleteContext {
                tenant_id: tenant.into(),
                trace_family: family.into(),
                trace_id: target["trace_id"].as_str().map(str::to_owned),
                ..Default::default()
            };
            let payloads = self.scorer.score_target(&evaluator, target, &context).await;
            let mut persisted = 0;
            for mut score in payloads.iter().cloned() {
                let mut metadata = object(&score["metadata"]);
                metadata.insert("experiment_run_id".into(), json!(run));
                metadata.insert("example_id".into(), target["example_id"].clone());
                metadata.insert("case_id".into(), target["case_id"].clone());
                metadata.insert("candidate_trace_id".into(), target["trace_id"].clone());
                score["metadata"] = json!(metadata);
                if let Some(created) = self
                    .traces
                    .create_eval_score_claimed(
                        tenant,
                        text(target, "trace_id"),
                        &self.created_by,
                        &score,
                        family,
                        job,
                    )
                    .await
                    .map_err(trace_error)?
                {
                    persisted += 1;
                    written += 1;
                    infrastructure |= score["metadata"]["failure_kind"] == "infrastructure";
                    let label = text(&created, "label").to_owned();
                    if matches!(label.as_str(), "pass" | "fail") {
                        if let Some(number) = created["numeric_value"]
                            .as_f64()
                            .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
                        {
                            scores.push(number);
                        }
                    }
                    labels.push(label);
                }
            }
            if payloads.is_empty() || persisted != payloads.len() {
                skipped += 1;
            }
        }
        let average = if scores.is_empty() {
            None
        } else {
            Some(
                ((scores.iter().sum::<f64>() / scores.len() as f64) * 10000.0).round_ties_even()
                    / 10000.0,
            )
        };
        let reviews = labels.iter().filter(|v| v.as_str() == "review").count();
        let summary = json!({"average_score":average,"scored_count":scores.len(),"review_count":reviews,"target_count":targets.len(),"expected_count":expected,"resolved_count":targets.len(),"executed_count":targets.len(),"skipped_count":skipped,"evaluator_type":kind,"evaluator_name":evaluator["name"]});
        let metrics = json!({"targets":targets.len(),"scores_written":written,"pass_count":labels.iter().filter(|v|v.as_str()=="pass").count(),"review_count":reviews,"expected_count":expected,"resolved_count":targets.len(),"executed_count":targets.len(),"scored_count":scores.len(),"skipped_count":skipped});
        let error = if infrastructure {
            Some(
                if kind == "ragas" {
                    "KB RAGAS infrastructure failure requires retry"
                } else {
                    "Evaluator infrastructure failure requires retry"
                }
                .into(),
            )
        } else if skipped > 0 {
            Some(format!(
                "{skipped} evaluation target(s) had incomplete score persistence"
            ))
        } else {
            None
        };
        let status = if error.is_some() {
            "failed"
        } else {
            "succeeded"
        };
        self.update_run(
            job,
            run,
            status,
            Some(&summary),
            Some(&metrics),
            error.as_deref(),
            false,
            true,
        )
        .await?;
        Ok(EvaluatorRunResult::terminal(
            run, status, summary, metrics, error, written,
        ))
    }
    async fn resolve_targets(
        &self,
        job: &Value,
        payload: &Value,
        family: &str,
    ) -> Result<(Vec<Value>, usize, usize), CompatError> {
        let tenant = text(job, "tenant_id");
        let repository = self.traces.with_outbox_claim(job).map_err(trace_error)?;
        if !text(payload, "trace_id").is_empty() {
            let target = repository
                .get_trace_detail(tenant, text(payload, "trace_id"), None, family)
                .await
                .map_err(trace_error)?
                .and_then(|v| trace_target(&v));
            return Ok((target.into_iter().collect(), 1, 0));
        }
        let dataset = text(payload, "dataset_id");
        if dataset.is_empty() {
            return Ok((vec![], 0, 0));
        }
        let manifest = if payload["dataset_manifest"].is_array() {
            array(&payload["dataset_manifest"]).to_vec()
        } else if payload["target_snapshot"]["dataset_manifest"].is_array() {
            array(&payload["target_snapshot"]["dataset_manifest"]).to_vec()
        } else {
            self.service.list_example_manifest(tenant, dataset).await?
        };
        let manifest = manifest
            .into_iter()
            .filter(is_runnable_dataset_example)
            .collect::<Vec<_>>();
        let expected = manifest.len();
        let ids = manifest
            .iter()
            .filter(|v| !text(v, "example_id").is_empty())
            .filter_map(|v| v["source_trace_id"].as_str())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let details = repository
            .get_trace_details(tenant, &ids, None, family)
            .await
            .map_err(trace_error)?;
        let (mut targets, mut unresolved) = (vec![], 0);
        for example in manifest {
            let id = text(&example, "example_id");
            let source = text(&example, "source_trace_id");
            if id.is_empty() || source.is_empty() {
                unresolved += 1;
                continue;
            }
            let Some(mut target) = details.get(source).and_then(trace_target) else {
                unresolved += 1;
                continue;
            };
            target["expected_output"] = example
                .get("expected_output")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(json!({}));
            target["expected_trajectory"] = example["metadata"]
                .get("expected_trajectory")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(json!({}));
            target["assertions"] = example["metadata"]
                .get("assertions")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(json!([]));
            target["example_id"] = json!(id);
            target["case_id"] = json!(if text(&example["metadata"], "case_id").is_empty() {
                id
            } else {
                text(&example["metadata"], "case_id")
            });
            target["target_type"] = json!("example");
            target["target_id"] = json!(id);
            targets.push(target);
        }
        Ok((targets, expected, unresolved))
    }
    pub async fn schedule_online(&self, job: &Value) -> Result<Value, CompatError> {
        let tenant = text(job, "tenant_id");
        let payload = &job["payload"];
        let trace_id = text(payload, "trace_id").trim();
        if trace_id.is_empty() {
            return Ok(json!({"scheduled":0,"reason":"missing_trace_id"}));
        }
        let family = family(payload);
        let detail = self
            .traces
            .with_outbox_claim(job)
            .map_err(trace_error)?
            .get_trace_detail(tenant, trace_id, None, family)
            .await
            .map_err(trace_error)?
            .unwrap_or(json!({}));
        let origin = [
            text(payload, "eval_origin"),
            text(&detail["trace"]["metadata"], "eval_origin"),
            text(&detail["trace"], "user_id"),
        ]
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or("");
        if matches!(
            origin,
            "candidate" | "judge" | "eval-candidate" | "eval-worker"
        ) {
            return Ok(
                json!({"scheduled":0,"matched":0,"trace_id":trace_id,"reason":"internal_eval_trace"}),
            );
        }
        let pending=sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM eval_experiment_runs WHERE tenant_id=$1 AND status IN ('queued','running') AND target_snapshot->>'source'='online_sampling'").bind(tenant).fetch_one(&self.service.pool).await?;
        if pending >= 200 {
            return Ok(
                json!({"scheduled":0,"matched":0,"trace_id":trace_id,"reason":"online_eval_queue_full","pending_online_runs":pending}),
            );
        }
        let evaluators = sqlx::query_scalar::<_, Value>(
            "SELECT to_jsonb(e) FROM eval_evaluators e WHERE tenant_id=$1 ORDER BY created_at DESC",
        )
        .bind(tenant)
        .fetch_all(&self.service.pool)
        .await?;
        let status = if text(payload, "status").is_empty() {
            "succeeded".into()
        } else {
            text(payload, "status").trim().to_lowercase()
        };
        let (mut scheduled, mut matched) = (0, 0);
        for evaluator in evaluators {
            if !matches_online(&evaluator, family, &status) {
                continue;
            }
            matched += 1;
            let config = &evaluator["sampling_config"]["online"];
            let rate = config
                .get("rate")
                .map(|v| {
                    v.as_f64()
                        .or_else(|| v.as_str().and_then(|v| v.parse().ok()))
                        .unwrap_or(0.05)
                })
                .unwrap_or(0.05);
            if !should_sample_trace_id(trace_id, rate) {
                continue;
            }
            let id = text(&evaluator, "evaluator_id");
            if id.is_empty() {
                continue;
            }
            let active=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM eval_experiment_runs WHERE tenant_id=$1 AND evaluator_id=$2::text::uuid AND status IN ('queued','running') AND target_snapshot->>'trace_id'=$3)").bind(tenant).bind(id).bind(trace_id).fetch_one(&self.service.pool).await?;
            let cap = config["max_pending_runs"]
                .as_i64()
                .filter(|n| *n != 0)
                .unwrap_or(200)
                .max(1);
            if active || pending + scheduled >= cap {
                continue;
            }
            self.service.enqueue_evaluator_run_claimed(&CompatUser{tenant_id:tenant.into(),user_id:"eval-online-sampler".into(),..Default::default()},&json!({"evaluator_id":id,"trace_id":trace_id,"target_snapshot":{"trace_id":trace_id,"trace_family":family,"source":"online_sampling","status":status,"source_adapter":payload["source_adapter"]},"metadata":{"online_sampling":true}}),Some(job)).await?;
            scheduled += 1;
        }
        Ok(json!({"scheduled":scheduled,"matched":matched,"trace_id":trace_id}))
    }
}
pub fn should_sample_trace_id(id: &str, rate: f64) -> bool {
    if !rate.is_finite() || rate <= 0.0 {
        return false;
    }
    if rate >= 1.0 {
        return true;
    }
    let bucket = if let Ok(uuid) = uuid::Uuid::parse_str(id) {
        uuid.as_u128() % 10000
    } else {
        let digest = Sha256::digest(id.as_bytes());
        u64::from_be_bytes(digest[..8].try_into().expect("digest length")) as u128 % 10000
    };
    bucket < (rate.clamp(0.0, 1.0) * 10000.0) as u128
}
pub fn matches_online(evaluator: &Value, family: &str, status: &str) -> bool {
    let config = &evaluator["sampling_config"]["online"];
    if config["enabled"] != true || matches!(text(evaluator, "evaluator_type"), "" | "human") {
        return false;
    }
    let families = array(&config["trace_families"]);
    if !families.is_empty() && !families.iter().any(|v| v.as_str() == Some(family)) {
        return false;
    }
    if config["only_failed"] == true
        && !matches!(status, "failed" | "cancelled" | "timeout" | "error")
    {
        return false;
    }
    config["only_succeeded"] != true || status == "succeeded"
}

pub struct EvalOutboxWorker {
    pub executor: Arc<EvaluatorExecutor>,
    pub outbox: EvalOutbox,
    pub poll_interval: Duration,
    pub max_attempts: i32,
    pub lease_seconds: i32,
    pub capacity: Option<Arc<CapacityAdmissionController>>,
    /// Shared live resolver used by the canonical worker admission hook.
    /// None retains explicitly supplied legacy static budgets.
    pub capacity_resolver: Option<Arc<CapacityResolver>>,
    pub budgets: Vec<CapacityBudget>,
    owner: String,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    stop: watch::Sender<bool>,
}
impl EvalOutboxWorker {
    pub fn new(executor: Arc<EvaluatorExecutor>, outbox: EvalOutbox) -> Self {
        let (stop, _) = watch::channel(false);
        Self {
            executor,
            outbox,
            poll_interval: Duration::from_secs(2),
            max_attempts: 5,
            lease_seconds: 60,
            capacity: None,
            capacity_resolver: None,
            budgets: vec![],
            owner: format!("eval-worker-{}", uuid::Uuid::new_v4()),
            tasks: Mutex::new(vec![]),
            stop,
        }
    }
    pub async fn start(self: &Arc<Self>, concurrency: usize) {
        let mut tasks = self.tasks.lock().await;
        if !tasks.is_empty() {
            return;
        }
        self.stop.send_replace(false);
        for index in 0..concurrency.max(1) {
            let worker = self.clone();
            tasks.push(tokio::spawn(async move {
                worker.poll(index).await;
            }));
        }
    }
    pub async fn stop(&self) {
        self.stop.send_replace(true);
        let tasks = std::mem::take(&mut *self.tasks.lock().await);
        for task in tasks {
            let _ = task.await;
        }
    }
    pub async fn running(&self) -> bool {
        !self.tasks.lock().await.is_empty()
    }
    async fn poll(self: Arc<Self>, index: usize) {
        let mut stop = self.stop.subscribe();
        loop {
            if *stop.borrow() {
                break;
            }
            let claim_owner = format!("{}:{index}", self.owner);
            let jobs = tokio::select! {_=stop.changed()=>break,jobs=self.outbox.claim(&claim_owner,1,self.max_attempts,self.lease_seconds)=>jobs};
            match jobs {
                Ok(jobs) if !jobs.is_empty() => {
                    for job in jobs {
                        if *stop.borrow() {
                            let _ = self.outbox.abandon(&job).await;
                            break;
                        }
                        self.handle(&job, &mut stop).await;
                    }
                }
                _ => {
                    tokio::select! {_=stop.changed()=>break,_=tokio::time::sleep(self.poll_interval)=>{}}
                }
            }
        }
    }
    async fn handle(&self, job: &Value, stop: &mut watch::Receiver<bool>) {
        let interval = Duration::from_secs_f64(self.lease_seconds.max(3) as f64 / 3.0);
        let execution = self.execute_admitted(job);
        tokio::pin!(execution);
        let heartbeat = async {
            loop {
                tokio::time::sleep(interval).await;
                let valid =
                    tokio::time::timeout(interval, self.outbox.renew(job, self.lease_seconds))
                        .await;
                if !matches!(valid, Ok(Ok(true))) {
                    return;
                }
            }
        };
        tokio::pin!(heartbeat);
        tokio::select! {_=stop.changed()=>{let _=self.outbox.abandon(job).await;},_=heartbeat=>{},result=execution=>{if let Err(error)=result{if !is_lost(&error){let _=self.outbox.failed(job,"eval_job_admission_unavailable",Some(5),self.max_attempts).await;}}}}
    }
    async fn execute_admitted(&self, job: &Value) -> Result<(), CompatError> {
        let lease = if let Some(controller) = &self.capacity {
            let resolved = self
                .capacity_resolver
                .as_ref()
                .map(|resolver| {
                    resolver.resolve(&CapacityRequest {
                        tenant_id: text(job, "tenant_id").into(),
                        service_id: "eval-worker".into(),
                        request_class: "job".into(),
                        upstream_group: Value::Null,
                        provider_id: Value::Null,
                        is_admin_read: false,
                        service_config: Value::Null,
                        resource_kind: "job".into(),
                    })
                })
                .transpose()
                .map_err(|_| CompatError::new(503, "eval_job_admission_unavailable"))?;
            let (service_id, request_class) = if resolved.is_some() {
                ("eval-worker", "job")
            } else {
                ("gateway", "eval")
            };
            Some(
                controller
                    .acquire(
                        resolved.as_deref().unwrap_or(&self.budgets),
                        &AdmissionRequest {
                            tenant_id: text(job, "tenant_id").into(),
                            user_id: "eval-worker".into(),
                            service_id: service_id.into(),
                            request_class: request_class.into(),
                            request_id: text(job, "job_id").into(),
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|_| CompatError::new(503, "eval_job_admission_unavailable"))?,
            )
        } else {
            None
        };
        let result = self.execute(job).await;
        if let Some(lease) = lease {
            lease.release().await;
        }
        result
    }
    async fn execute(&self, job: &Value) -> Result<(), CompatError> {
        let result = async {
            match text(job, "job_type") {
                "eval.evaluator.run" => {
                    let result = self.executor.run_job(job).await?;
                    if result.status == "failed" {
                        return Err(CompatError::new(
                            503,
                            result
                                .error_message
                                .unwrap_or_else(|| "evaluator run failed".into()),
                        ));
                    }
                }
                "trace.ingested" => {
                    self.executor.schedule_online(job).await?;
                }
                _ => {}
            }
            self.outbox.succeeded(job).await
        }
        .await;
        if let Err(error) = result {
            if is_lost(&error) {
                return Err(error);
            }
            let attempts = job["attempts"].as_i64().unwrap_or(1) as i32;
            let terminal = attempts >= self.max_attempts;
            let run = text(&job["payload"], "run_id");
            let code = "eval_job_failed:RuntimeError";
            if text(job, "job_type") == "eval.evaluator.run" && !run.is_empty() {
                self.executor
                    .update_run(
                        job,
                        run,
                        if terminal { "failed" } else { "queued" },
                        None,
                        None,
                        Some(code),
                        false,
                        terminal,
                    )
                    .await?;
            }
            self.outbox
                .failed(job, code, Some((5 * attempts).min(300)), self.max_attempts)
                .await?;
        }
        Ok(())
    }
}
fn is_lost(error: &CompatError) -> bool {
    error.status == 409
        && error
            .detail
            .as_str()
            .is_some_and(|v| matches!(v, "eval_outbox_claim_lost" | "eval_outbox_lease_lost"))
}
