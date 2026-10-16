//! Immutable evaluation inputs and transactional durable enqueue/claim operations.
use super::{
    capability::canonical_json,
    common::{CompatError, CompatUser, hex, object, text},
    eval::EvalService,
};
use crate::pricing_receipt::{DEFAULT_PRICING_STATUS, build_pricing_snapshot};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use std::collections::BTreeSet;
fn mapping(value: &Value) -> Value {
    if value.is_object() {
        value.clone()
    } else if let Some(value) = value.as_str() {
        serde_json::from_str::<Value>(value)
            .ok()
            .filter(Value::is_object)
            .unwrap_or(json!({}))
    } else {
        json!({})
    }
}
pub fn canonical_hash(value: &Value) -> Result<String, CompatError> {
    Ok(hex(&Sha256::digest(canonical_json(value).as_bytes())))
}
pub fn is_runnable_dataset_example(example: &Value) -> bool {
    if !example.is_object() {
        return true;
    }
    if text(example, "split") == "review"
        || text(&example["metadata"], "source_kind") == "kb_failure"
    {
        return false;
    }
    example["metadata"]
        .get("review_status")
        .is_none_or(|v| v.is_null() || v == "approved")
}
pub fn build_eval_dataset_manifest(dataset: &Value, examples: &[Value]) -> Value {
    let mut cases = vec![];
    for row in examples {
        let mut metadata = object(&mapping(&row["metadata"]));
        let trajectory = metadata
            .remove("expected_trajectory")
            .filter(|v| !v.is_null())
            .unwrap_or(json!({}));
        let assertions = metadata
            .remove("assertions")
            .filter(|v| !v.is_null())
            .unwrap_or(json!([]));
        let case = metadata
            .get("case_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .unwrap_or(text(row, "example_id"));
        cases.push(json!({"case_id":case,"example_id":row["example_id"],"split":if text(row,"split").is_empty(){"regression"}else{text(row,"split")},"input":mapping(&row["input"]),"expected_output":mapping(&row["expected_output"]),"expected_trajectory":trajectory,"assertions":assertions,"metadata":metadata,"source_trace_id":row["source_trace_id"],"source_span_id":row["source_span_id"]}));
    }
    cases.sort_by(|a, b| {
        (text(a, "case_id"), text(a, "example_id"))
            .cmp(&(text(b, "case_id"), text(b, "example_id")))
    });
    json!({"dataset":{"dataset_id":dataset["dataset_id"],"tenant_id":dataset["tenant_id"],"name":text(dataset,"name"),"description":text(dataset,"description"),"version":if text(dataset,"version").is_empty(){"v1"}else{text(dataset,"version")},"schema":mapping(&dataset["schema"]),"metadata":mapping(&dataset["metadata"])},"examples":cases})
}
fn projection(value: &Value, keys: &[&str]) -> Value {
    let mut result = serde_json::Map::new();
    for key in keys {
        result.insert((*key).into(), value[*key].clone());
    }
    json!(result)
}
impl EvalService {
    pub async fn list_example_manifest(
        &self,
        tenant: &str,
        dataset: &str,
    ) -> Result<Vec<Value>, CompatError> {
        Ok(sqlx::query_scalar("SELECT to_jsonb(e) FROM eval_examples e WHERE tenant_id=$1 AND dataset_id=$2::text::uuid ORDER BY created_at,example_id").bind(tenant).bind(dataset).fetch_all(&self.pool).await?)
    }
    pub async fn freeze_eval_model_ref(
        &self,
        tenant: &str,
        model: &str,
        provider: Option<&str>,
    ) -> Result<Value, CompatError> {
        let rows=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('model_id',model_id,'provider_id',provider_id,'capability_revision',capability_revision,'input_price_per_1k',input_price_per_1k::text,'output_price_per_1k',output_price_per_1k::text) FROM llm_models WHERE tenant_id=$1 AND model_id=$2 AND is_enabled=TRUE AND ($3::text IS NULL OR provider_id=$3) LIMIT 2").bind(tenant).bind(model).bind(provider).fetch_all(&self.pool).await?;
        if rows.len() != 1 {
            return Err(CompatError::new(
                422,
                if rows.len() > 1 {
                    "eval_model_provider_required"
                } else {
                    "eval_model_unavailable"
                },
            ));
        }
        let row = &rows[0];
        let snapshot = build_pricing_snapshot(
            tenant,
            text(row, "provider_id"),
            text(row, "model_id"),
            text(row, "input_price_per_1k"),
            text(row, "output_price_per_1k"),
            DEFAULT_PRICING_STATUS,
        )
        .map_err(|e| CompatError::new(422, e.to_string()))?;
        Ok(
            json!({"tenant_id":tenant,"model_id":row["model_id"],"provider_id":row["provider_id"],"capability_revision":row["capability_revision"].as_i64().filter(|v|*v!=0).unwrap_or(1),"price_version":snapshot.version,"pricing_snapshot":snapshot.to_value()}),
        )
    }
    pub async fn freeze_eval_judge(
        &self,
        tenant: &str,
        mut evaluator: Value,
    ) -> Result<Value, CompatError> {
        if !matches!(
            text(&evaluator, "evaluator_type"),
            "llm" | "llm_judge" | "composite"
        ) {
            return Ok(evaluator);
        }
        let mut metadata = object(&evaluator["metadata"]);
        let model = metadata
            .get("judge_model_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("qwen3.7-plus");
        let provider = metadata.get("judge_provider_id").and_then(Value::as_str);
        let reference = self.freeze_eval_model_ref(tenant, model, provider).await?;
        metadata.insert("judge_model_id".into(), reference["model_id"].clone());
        metadata.insert("judge_provider_id".into(), reference["provider_id"].clone());
        metadata.insert("judge_model_ref".into(), reference);
        evaluator["metadata"] = json!(metadata);
        Ok(evaluator)
    }
    pub async fn enqueue_evaluator_run(
        &self,
        user: &CompatUser,
        request: &Value,
    ) -> Result<Value, CompatError> {
        self.enqueue_evaluator_run_claimed(user, request, None)
            .await
    }
    pub async fn enqueue_evaluator_run_claimed(
        &self,
        user: &CompatUser,
        request: &Value,
        claim: Option<&Value>,
    ) -> Result<Value, CompatError> {
        let evaluator_id = text(request, "evaluator_id");
        let evaluator = self
            .get(&user.tenant_id, "evaluators", evaluator_id)
            .await?
            .ok_or_else(|| CompatError::new(404, "eval_evaluator_not_found"))?;
        let evaluator = self.freeze_eval_judge(&user.tenant_id, evaluator).await?;
        let mut target = object(&request["target_snapshot"]);
        target.remove("dataset_manifest");
        let dataset = request
            .get("dataset_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty());
        let mut manifest = None;
        let mut dataset_hash = None;
        if let Some(dataset_id) = dataset {
            let info = self
                .get(&user.tenant_id, "datasets", dataset_id)
                .await?
                .ok_or_else(|| CompatError::new(404, "eval_dataset_not_found"))?;
            let examples = self
                .list_example_manifest(&user.tenant_id, dataset_id)
                .await?
                .into_iter()
                .filter(is_runnable_dataset_example)
                .collect::<Vec<_>>();
            dataset_hash = Some(canonical_hash(&build_eval_dataset_manifest(
                &info, &examples,
            ))?);
            let mut transport = examples
                .iter()
                .map(|row| {
                    projection(
                        row,
                        &[
                            "example_id",
                            "split",
                            "input",
                            "expected_output",
                            "metadata",
                            "source_trace_id",
                            "source_span_id",
                        ],
                    )
                })
                .collect::<Vec<_>>();
            transport.sort_by(|a, b| text(a, "example_id").cmp(text(b, "example_id")));
            target.insert("dataset_version".into(), info["version"].clone());
            target.insert("dataset_manifest_hash".into(), json!(dataset_hash));
            target.insert("dataset_case_count".into(), json!(transport.len()));
            manifest = Some(transport);
        }
        let snapshot = projection(
            &evaluator,
            &[
                "evaluator_id",
                "name",
                "evaluator_type",
                "rubric",
                "version",
                "sampling_config",
                "filter_config",
            ],
        );
        let mut suite = snapshot.clone();
        suite["metadata"] = evaluator.get("metadata").cloned().unwrap_or(json!({}));
        let suite_hash = canonical_hash(&suite)?;
        target.insert("evaluator_snapshot".into(), snapshot);
        if let Some(trace) = request.get("trace_id").filter(|v| !v.is_null()) {
            target.entry("trace_id").or_insert(trace.clone());
        }
        if request["metadata"]
            .as_object()
            .is_some_and(|m| !m.is_empty())
        {
            target
                .entry("metadata")
                .or_insert(request["metadata"].clone());
        }
        let target = json!(target);
        let mut tx = self.pool.begin().await?;
        if let Some(claim) = claim {
            EvalOutbox {
                pool: self.pool.clone(),
                enabled: true,
            }
            .fence(&mut tx, claim)
            .await?;
        }
        let run=sqlx::query_scalar::<_,String>("INSERT INTO eval_experiment_runs(experiment_id,tenant_id,evaluator_id,dataset_id,status,dataset_manifest_hash,evaluator_suite_hash,target_snapshot,metrics,created_by) VALUES($1::text::uuid,$2,$3::text::uuid,$4::text::uuid,'queued',$5,$6,$7,'{}'::jsonb,$8) RETURNING run_id::text").bind(request.get("experiment_id").and_then(Value::as_str)).bind(&user.tenant_id).bind(evaluator_id).bind(dataset).bind(dataset_hash).bind(suite_hash).bind(&target).bind(&user.user_id).fetch_one(&mut *tx).await?;
        let family = match text(&target, "trace_family") {
            "langgraph_proxy" => "langgraph_proxy",
            "rag" => "rag",
            _ => "assistant",
        };
        let payload = json!({"run_id":run,"evaluator_id":evaluator_id,"evaluator_snapshot":evaluator,"experiment_id":request["experiment_id"],"dataset_id":dataset,"trace_id":request["trace_id"],"trace_family":family,"target_snapshot":target,"dataset_manifest":manifest});
        let job=sqlx::query_scalar::<_,String>("INSERT INTO agent_trace_outbox(tenant_id,job_type,payload) VALUES($1,'eval.evaluator.run',$2) RETURNING job_id::text").bind(&user.tenant_id).bind(payload).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(json!({"job_id":job,"status":"queued","run_id":run}))
    }
    /// Inputs must have passed current caller/model/dataset/capability authorization.
    /// This transaction freezes them; it does not infer caller authority from a JSON snapshot.
    pub async fn enqueue_live_experiment_run(
        &self,
        user: &CompatUser,
        experiment: &str,
        dataset: &str,
        evaluators: &[Value],
        examples: &[Value],
        repetitions: i32,
        target: Value,
        execution: Value,
        fingerprint: Value,
        baseline: Option<&str>,
    ) -> Result<Value, CompatError> {
        if evaluators.is_empty() || repetitions < 1 {
            return Err(CompatError::new(422, "eval_live_run_invalid"));
        }
        let info = self
            .get(&user.tenant_id, "datasets", dataset)
            .await?
            .ok_or_else(|| CompatError::new(404, "eval_dataset_not_found"))?;
        let frozen = build_eval_dataset_manifest(&info, examples);
        let cases = frozen["examples"].as_array().unwrap();
        let mut seen = BTreeSet::new();
        for case in cases {
            let id = text(case, "case_id").trim();
            if id.is_empty() || !seen.insert(id.to_owned()) {
                return Err(CompatError::new(
                    422,
                    format!(
                        "Dataset contains missing or duplicate case_id: {}",
                        if id.is_empty() { "<empty>" } else { id }
                    ),
                ));
            }
        }
        let mut suite = evaluators
            .iter()
            .map(|e| {
                projection(
                    e,
                    &[
                        "evaluator_id",
                        "name",
                        "evaluator_type",
                        "rubric",
                        "version",
                        "sampling_config",
                        "filter_config",
                        "metadata",
                    ],
                )
            })
            .collect::<Vec<_>>();
        suite.sort_by(|a, b| text(a, "evaluator_id").cmp(text(b, "evaluator_id")));
        let reference = self
            .freeze_eval_model_ref(
                &user.tenant_id,
                text(&execution, "model_id"),
                execution.get("provider_id").and_then(Value::as_str),
            )
            .await?;
        for evaluator in &mut suite {
            *evaluator = self
                .freeze_eval_judge(&user.tenant_id, evaluator.clone())
                .await?;
        }
        let dataset_hash = canonical_hash(&frozen)?;
        let suite_hash = canonical_hash(&json!(suite))?;
        let ids = suite
            .iter()
            .map(|e| e["evaluator_id"].clone())
            .collect::<Vec<_>>();
        let mut target = json!(object(&target));
        for (key, value) in [
            ("run_mode", json!("live_candidate")),
            ("repetitions", json!(repetitions)),
            ("evaluator_ids", json!(ids)),
            ("dataset_manifest_hash", json!(dataset_hash)),
            ("evaluator_suite_hash", json!(suite_hash)),
        ] {
            target[key] = value;
        }
        let mut execution = json!(object(&execution));
        execution["model_id"] = reference["model_id"].clone();
        execution["provider_id"] = reference["provider_id"].clone();
        execution["model_ref"] = reference.clone();
        execution["evaluators"] = json!(suite);
        let mut fingerprint = json!(object(&fingerprint));
        fingerprint["model_ref"] = reference;
        let mut tx = self.pool.begin().await?;
        let run=sqlx::query_scalar::<_,String>("INSERT INTO eval_experiment_runs(experiment_id,tenant_id,evaluator_id,dataset_id,status,run_mode,repetitions,baseline_run_id,dataset_manifest_hash,evaluator_suite_hash,candidate_fingerprint,execution_config,target_snapshot,metrics,created_by) VALUES($1::text::uuid,$2,$3::text::uuid,$4::text::uuid,'queued','live_candidate',$5,$6::text::uuid,$7,$8,$9,$10,$11,'{}'::jsonb,$12) RETURNING run_id::text").bind(experiment).bind(&user.tenant_id).bind(ids[0].as_str()).bind(dataset).bind(repetitions).bind(baseline).bind(dataset_hash).bind(suite_hash).bind(fingerprint).bind(execution).bind(target).bind(&user.user_id).fetch_one(&mut *tx).await?;
        for case in cases {
            for trial in 1..=repetitions {
                sqlx::query("INSERT INTO eval_experiment_run_cases(run_id,tenant_id,case_id,example_id,trial_index,input,expected_output,expected_trajectory,assertions,metadata) VALUES($1::text::uuid,$2,$3,$4::text::uuid,$5,$6,$7,$8,$9,$10)").bind(&run).bind(&user.tenant_id).bind(text(case,"case_id")).bind(case.get("example_id").and_then(Value::as_str)).bind(trial).bind(&case["input"]).bind(&case["expected_output"]).bind(&case["expected_trajectory"]).bind(&case["assertions"]).bind(&case["metadata"]).execute(&mut *tx).await?;
            }
        }
        let payload = json!({"run_id":run,"experiment_id":experiment,"dataset_id":dataset,"evaluator_id":ids[0],"evaluator_ids":ids,"run_mode":"live_candidate","trace_family":"assistant"});
        let job=sqlx::query_scalar::<_,String>("INSERT INTO agent_trace_outbox(tenant_id,job_type,payload) VALUES($1,'eval.evaluator.run',$2) RETURNING job_id::text").bind(&user.tenant_id).bind(payload).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(json!({"job_id":job,"status":"queued","run_id":run}))
    }
}

#[derive(Clone)]
pub struct EvalOutbox {
    pub pool: sqlx::PgPool,
    pub enabled: bool,
}
impl EvalOutbox {
    pub async fn claim(
        &self,
        owner: &str,
        limit: i64,
        max_attempts: i32,
        lease_seconds: i32,
    ) -> Result<Vec<Value>, CompatError> {
        if !self.enabled {
            return Ok(vec![]);
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("WITH expired AS (UPDATE agent_trace_outbox SET status='failed',last_error='eval_outbox_attempts_exhausted',updated_at=NOW() WHERE status='running' AND lease_until<=NOW() AND attempts >= $1 RETURNING tenant_id,payload,job_type) UPDATE eval_experiment_runs r SET status='failed',finished_at=NOW(),error_message='eval_outbox_attempts_exhausted',updated_at=NOW() FROM expired e WHERE e.job_type='eval.evaluator.run' AND r.tenant_id=e.tenant_id AND r.run_id::text=e.payload->>'run_id'").bind(max_attempts).execute(&mut *tx).await?;
        let plan = claim_plan(owner, limit, max_attempts, lease_seconds);
        let rows = sqlx::query_scalar::<_, Value>(CLAIM_OUTBOX_SQL)
            .bind(plan.limit)
            .bind(plan.max_attempts)
            .bind(plan.owner)
            .bind(plan.lease_seconds)
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(rows)
    }
    pub async fn renew(&self, job: &Value, lease_seconds: i32) -> Result<bool, CompatError> {
        if !self.enabled {
            return Ok(false);
        }
        Ok(sqlx::query("UPDATE agent_trace_outbox SET lease_until=NOW()+($5::int*INTERVAL '1 second'),heartbeat_at=NOW() WHERE job_id=$1::text::uuid AND tenant_id=$2 AND owner_id=$3 AND claim_token=$4::text::uuid AND status='running' AND lease_until>clock_timestamp()").bind(text(job,"job_id")).bind(text(job,"tenant_id")).bind(text(job,"owner_id")).bind(text(job,"claim_token")).bind(lease_seconds.max(3)).execute(&self.pool).await?.rows_affected()==1)
    }
    pub async fn fence(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        job: &Value,
    ) -> Result<(), CompatError> {
        let found=sqlx::query_scalar::<_,String>("SELECT job_id::text FROM agent_trace_outbox WHERE job_id=$1::text::uuid AND tenant_id=$2 AND owner_id=$3 AND claim_token=$4::text::uuid AND status='running' AND lease_until>clock_timestamp() FOR UPDATE").bind(text(job,"job_id")).bind(text(job,"tenant_id")).bind(text(job,"owner_id")).bind(text(job,"claim_token")).fetch_optional(&mut **tx).await?;
        if found.is_none() {
            return Err(CompatError::new(409, "eval_outbox_claim_lost"));
        }
        Ok(())
    }
    pub async fn succeeded(&self, job: &Value) -> Result<(), CompatError> {
        let mut tx = self.pool.begin().await?;
        self.fence(&mut tx, job).await?;
        sqlx::query("UPDATE agent_trace_outbox SET status='succeeded',last_error=NULL,updated_at=NOW() WHERE job_id=$1::text::uuid").bind(text(job,"job_id")).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn failed(
        &self,
        job: &Value,
        error: &str,
        retry_seconds: Option<i32>,
        max_attempts: i32,
    ) -> Result<(), CompatError> {
        let mut tx = self.pool.begin().await?;
        self.fence(&mut tx, job).await?;
        let error = error.chars().take(4000).collect::<String>();
        sqlx::query("WITH changed AS (UPDATE agent_trace_outbox SET status=CASE WHEN $3::int IS NULL OR attempts >= $4 THEN 'failed' ELSE 'queued' END,last_error=$2,available_at=CASE WHEN $3::int IS NULL OR attempts >= $4 THEN available_at ELSE NOW()+($3::int*INTERVAL '1 second') END,updated_at=NOW() WHERE job_id=$1::text::uuid RETURNING tenant_id,job_type,payload,status) UPDATE eval_experiment_runs r SET status='failed',error_message=$2,finished_at=NOW(),updated_at=NOW() FROM changed j WHERE j.status='failed' AND j.job_type='eval.evaluator.run' AND r.tenant_id=j.tenant_id AND r.run_id=NULLIF(j.payload->>'run_id','')::uuid").bind(text(job,"job_id")).bind(error).bind(retry_seconds).bind(max_attempts).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn abandon(&self, job: &Value) -> Result<(), CompatError> {
        let mut tx = self.pool.begin().await?;
        self.fence(&mut tx, job).await?;
        sqlx::query("UPDATE agent_trace_outbox SET status='queued',available_at=NOW(),lease_until=NULL,updated_at=NOW() WHERE job_id=$1::text::uuid").bind(text(job,"job_id")).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn resolve_actor(&self, job: &Value) -> Result<Value, CompatError> {
        let mut tx = self.pool.begin().await?;
        self.fence(&mut tx, job).await?;
        let actor=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('actor','eval-worker','subject',u.user_id,'tenant_id',u.tenant_id,'run_id',r.run_id,'job_id',o.job_id) FROM agent_trace_outbox o JOIN eval_experiment_runs r ON r.run_id::text=o.payload->>'run_id' AND r.tenant_id=o.tenant_id JOIN users u ON u.user_id=r.created_by AND u.tenant_id=r.tenant_id WHERE o.job_id=$1::text::uuid AND o.tenant_id=$2 AND u.status='active'").bind(text(job,"job_id")).bind(text(job,"tenant_id")).fetch_optional(&mut *tx).await?.ok_or_else(||CompatError::new(403,"AGENT_EVAL_DELEGATION_SUBJECT_UNAVAILABLE"))?;
        tx.commit().await?;
        Ok(actor)
    }
}

pub const CLAIM_OUTBOX_SQL: &str = "WITH picked AS (SELECT job_id FROM agent_trace_outbox WHERE ((status='queued' AND available_at<=NOW()) OR (status='running' AND lease_until<=NOW())) AND attempts<$2 ORDER BY available_at,created_at LIMIT $1 FOR UPDATE SKIP LOCKED),claimed AS (UPDATE agent_trace_outbox o SET status='running',attempts=o.attempts+1,owner_id=$3,claim_token=gen_random_uuid(),lease_until=NOW()+($4::int*INTERVAL '1 second'),heartbeat_at=NOW(),updated_at=NOW() FROM picked WHERE o.job_id=picked.job_id RETURNING o.*) SELECT to_jsonb(claimed) FROM claimed";
#[derive(Debug, PartialEq)]
pub struct ClaimPlan<'a> {
    pub owner: &'a str,
    pub limit: i64,
    pub max_attempts: i32,
    pub lease_seconds: i32,
}
pub fn claim_plan(owner: &str, limit: i64, max_attempts: i32, lease_seconds: i32) -> ClaimPlan<'_> {
    ClaimPlan {
        owner,
        limit,
        max_attempts,
        lease_seconds: lease_seconds.max(3),
    }
}
