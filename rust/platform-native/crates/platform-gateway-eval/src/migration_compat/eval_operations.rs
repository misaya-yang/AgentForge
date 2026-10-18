//! Explicit retries preserve frozen inputs and refuse uncertain external effects.
use super::{
    common::{CompatError, CompatUser, hex, object, text},
    eval::EvalService,
    eval_queue::canonical_hash,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn retry_error(message: impl Into<String>) -> CompatError {
    let message = message.into();
    let conflict = [
        "side_effect_unconfirmed",
        "idempotency_conflict",
        "kb_source_requires_fresh_authorization",
        "dataset_provenance_unverified",
        "dataset_unavailable",
    ]
    .iter()
    .any(|s| message.contains(s));
    CompatError::new(if conflict { 409 } else { 422 }, message)
}
fn retry_source_case<'a>(group: &[&'a Value], id: &str) -> Result<&'a Value, CompatError> {
    if group
        .iter()
        .any(|row| !matches!(text(row, "status"), "succeeded" | "failed"))
    {
        return Err(retry_error(format!("eval_retry_case_not_terminal:{id}")));
    }
    let source = group
        .iter()
        .copied()
        .find(|row| {
            row["status"] == "failed"
                || row["status"] == "succeeded" && row["observed_metrics"]["behavior_pass"] == false
        })
        .ok_or_else(|| retry_error(format!("eval_retry_case_not_failed:{id}")))?;
    for row in group {
        let meta = &row["metadata"];
        if text(meta, "source_kind").starts_with("kb")
            || text(meta, "source_kind").starts_with("knowledge")
            || ["kb_dataset_id", "kb_source_versions", "kb_trace_id"]
                .iter()
                .any(|k| meta.get(*k).is_some())
        {
            return Err(retry_error(format!(
                "eval_retry_kb_source_requires_fresh_authorization:{id}"
            )));
        }
        let observed = &row["observed_metrics"];
        let unknown = matches!(
            text(row, "dispatch_state"),
            "dispatching" | "reconcile_required"
        ) || observed["side_effect_unknown"] == true
            || observed["execution_outcome"] == "side_effect_unknown"
            || observed["exit_reason"] == "side_effect_unknown";
        let model_only = observed["execution_succeeded"] == true
            && (!text(row, "candidate_trace_id").is_empty()
                || !text(observed, "trace_id").is_empty())
            && observed["tool_trajectory"] == json!([])
            && matches!(text(observed, "exit_reason"), "completed" | "succeeded");
        let not_started =
            row["dispatch_state"] == "not_started" && arr(&observed["tool_trajectory"]).is_empty();
        if unknown || (!not_started && !model_only) {
            return Err(retry_error(format!(
                "eval_retry_side_effect_unconfirmed:{id}"
            )));
        }
    }
    Ok(source)
}
/// Advisory projection of the same all-attempt proof used at retry submission.
/// Current dataset authorization and durable effect evidence are still checked by the POST.
pub fn retry_available(rows: &[&Value]) -> bool {
    let Some(first) = rows.first() else {
        return false;
    };
    let id = text(first, "case_id");
    !id.trim().is_empty()
        && rows.iter().all(|row| text(row, "case_id") == id)
        && retry_source_case(rows, id).is_ok()
}
pub fn retry_source_cases(rows: &[Value], ids: &[String]) -> Result<Vec<Value>, CompatError> {
    if ids.is_empty()
        || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
        || ids.iter().any(|s| s.trim().is_empty())
    {
        return Err(retry_error("eval_retry_case_ids_invalid"));
    }
    let mut groups: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for row in rows {
        groups.entry(text(row, "case_id")).or_default().push(row);
    }
    ids.iter()
        .map(|id| {
            let group = groups
                .get(id.as_str())
                .ok_or_else(|| retry_error(format!("eval_retry_case_not_found:{id}")))?;
            retry_source_case(group, id).cloned()
        })
        .collect()
}
/// Include every selected attempt, not just the failed row copied into the new run.
pub fn retry_effect_run_ids(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .flat_map(|case| {
            [
                text(&case["runtime_handle"], "turn_id"),
                text(case, "candidate_trace_id"),
                text(&case["observed_metrics"], "trace_id"),
            ]
        })
        .filter(|id| !id.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_owned)
        .collect()
}
impl EvalService {
    pub async fn retry_failed_experiment_cases(
        &self,
        user: &CompatUser,
        run: &str,
        ids: &[String],
        key: &str,
    ) -> Result<Option<Value>, CompatError> {
        if user.user_id.is_empty() {
            return Err(CompatError::new(
                403,
                "Authenticated user is required for retry",
            ));
        }
        if !(8..=128).contains(&key.chars().count()) || key.trim() != key {
            return Err(CompatError::new(422, "Idempotency-Key header is required"));
        }
        let hash = hex(&Sha256::digest(
            format!("{}:{}:{run}:{key}", user.tenant_id, user.user_id).as_bytes(),
        ));
        let mut tx = self.pool.begin().await?;
        let source=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(r) FROM eval_experiment_runs r WHERE tenant_id=$1 AND run_id=$2::text::uuid FOR UPDATE").bind(&user.tenant_id).bind(run).fetch_optional(&mut *tx).await?;
        let Some(source) = source else {
            tx.commit().await?;
            return Ok(None);
        };
        if source["run_mode"] != "live_candidate"
            || !matches!(
                text(&source, "status"),
                "succeeded" | "failed" | "cancelled"
            )
        {
            return Err(retry_error("eval_retry_requires_terminal_live_run"));
        }
        if let Some(existing)=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(r) FROM eval_experiment_runs r WHERE tenant_id=$1 AND target_snapshot->>'retry_of_run_id'=$2 AND target_snapshot->>'retry_request_key_hash'=$3 LIMIT 1").bind(&user.tenant_id).bind(run).bind(&hash).fetch_optional(&mut *tx).await?{let mut previous=arr(&existing["target_snapshot"]["retry_case_ids"]).iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>();let mut requested=ids.to_vec();previous.sort();requested.sort();if previous!=requested{return Err(retry_error("eval_retry_idempotency_conflict"));}tx.commit().await?;return Ok(Some(json!({"job_id":existing["target_snapshot"]["retry_job_id"],"run_id":existing["run_id"],"status":existing["status"]})));}
        if source["target_snapshot"]["dataset_kb_linked"] != false {
            return Err(retry_error("eval_retry_dataset_provenance_unverified"));
        }
        let dataset = sqlx::query_scalar::<_, Value>(
            "SELECT metadata FROM eval_datasets WHERE tenant_id=$1 AND dataset_id=$2::text::uuid",
        )
        .bind(&user.tenant_id)
        .bind(source["dataset_id"].as_str())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| retry_error("eval_retry_dataset_unavailable"))?;
        if !text(&dataset, "kb_dataset_id").is_empty() {
            return Err(retry_error(
                "eval_retry_kb_source_requires_fresh_authorization",
            ));
        }
        let rows=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(c) FROM eval_experiment_run_cases c WHERE tenant_id=$1 AND run_id=$2::text::uuid AND case_id=ANY($3::varchar[]) ORDER BY case_id,trial_index FOR UPDATE").bind(&user.tenant_id).bind(run).bind(ids).fetch_all(&mut *tx).await?;
        let mut cases = retry_source_cases(&rows, ids)?;
        let effect_runs = retry_effect_run_ids(&rows);
        if !effect_runs.is_empty() {
            let effects = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM assistant_capability_executions WHERE tenant_id=$1 AND run_id=ANY($2::text[]::uuid[])) OR EXISTS(SELECT 1 FROM assistant_runtime_items WHERE tenant_id=$1 AND turn_id=ANY($2::text[]) AND event_type IN ('compat/v1/tool_call_start','compat/v1/tool_call_end','compat/v1/tool_result'))",
            )
            .bind(&user.tenant_id)
            .bind(&effect_runs)
            .fetch_one(&mut *tx)
            .await?;
            if effects {
                return Err(retry_error("eval_retry_side_effect_unconfirmed"));
            }
        }
        cases.sort_by(|a, b| text(a, "case_id").cmp(text(b, "case_id")));
        let manifest = cases
            .iter()
            .map(|row| {
                let mut m = serde_json::Map::new();
                for k in [
                    "case_id",
                    "example_id",
                    "input",
                    "expected_output",
                    "expected_trajectory",
                    "assertions",
                    "metadata",
                ] {
                    m.insert(k.into(), row[k].clone());
                }
                json!(m)
            })
            .collect::<Vec<_>>();
        let manifest_hash = canonical_hash(&json!(manifest))?;
        let config = &source["execution_config"];
        let evaluators = arr(&config["evaluators"])
            .iter()
            .filter_map(|e| e["evaluator_id"].as_str().filter(|v| !v.is_empty()))
            .collect::<Vec<_>>();
        if evaluators.is_empty() {
            return Err(retry_error("eval_retry_frozen_evaluator_missing"));
        }
        let mut requested = ids.to_vec();
        requested.sort();
        let mut snapshot = json!(object(&source["target_snapshot"]));
        for (k, v) in [
            ("run_mode", json!("live_candidate")),
            ("repetitions", json!(1)),
            ("dataset_manifest_hash", json!(manifest_hash)),
            ("retry_of_run_id", json!(run)),
            ("retry_request_key_hash", json!(hash)),
            ("retry_case_ids", json!(requested)),
            (
                "retry_source_run_case_ids",
                json!(
                    cases
                        .iter()
                        .map(|v| v["run_case_id"].clone())
                        .collect::<Vec<_>>()
                ),
            ),
        ] {
            snapshot[k] = v;
        }
        let mut fingerprint = json!(object(&source["candidate_fingerprint"]));
        fingerprint["verification"] = json!("pending");
        let new_run=sqlx::query_scalar::<_,String>("INSERT INTO eval_experiment_runs(experiment_id,tenant_id,evaluator_id,dataset_id,status,run_mode,repetitions,baseline_run_id,dataset_manifest_hash,evaluator_suite_hash,candidate_fingerprint,execution_config,target_snapshot,metrics,created_by) VALUES($1::text::uuid,$2,$3::text::uuid,$4::text::uuid,'queued','live_candidate',1,$5::text::uuid,$6,$7,$8,$9,$10,'{}'::jsonb,$11) RETURNING run_id::text").bind(source["experiment_id"].as_str()).bind(&user.tenant_id).bind(source["evaluator_id"].as_str()).bind(source["dataset_id"].as_str()).bind(source["baseline_run_id"].as_str()).bind(&manifest_hash).bind(source["evaluator_suite_hash"].as_str()).bind(fingerprint).bind(config).bind(snapshot).bind(&user.user_id).fetch_one(&mut *tx).await?;
        for row in &cases {
            sqlx::query("INSERT INTO eval_experiment_run_cases(run_id,tenant_id,case_id,example_id,trial_index,input,expected_output,expected_trajectory,assertions,metadata) VALUES($1::text::uuid,$2,$3,$4::text::uuid,1,$5,$6,$7,$8,$9)").bind(&new_run).bind(&user.tenant_id).bind(text(row,"case_id")).bind(row["example_id"].as_str()).bind(&row["input"]).bind(&row["expected_output"]).bind(&row["expected_trajectory"]).bind(&row["assertions"]).bind(&row["metadata"]).execute(&mut *tx).await?;
        }
        let payload = json!({"run_id":new_run,"experiment_id":source["experiment_id"],"dataset_id":source["dataset_id"],"evaluator_id":source["evaluator_id"],"evaluator_ids":evaluators,"run_mode":"live_candidate","trace_family":"assistant"});
        let job=sqlx::query_scalar::<_,String>("INSERT INTO agent_trace_outbox(tenant_id,job_type,payload) VALUES($1,'eval.evaluator.run',$2) RETURNING job_id::text").bind(&user.tenant_id).bind(payload).fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE eval_experiment_runs SET target_snapshot=jsonb_set(target_snapshot,'{retry_job_id}',to_jsonb($3::text)) WHERE tenant_id=$1 AND run_id=$2::text::uuid").bind(&user.tenant_id).bind(&new_run).bind(&job).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(
            json!({"job_id":job,"run_id":new_run,"status":"queued"}),
        ))
    }
    pub async fn list_run_cases(&self, tenant: &str, run: &str) -> Result<Vec<Value>, CompatError> {
        Ok(sqlx::query_scalar("SELECT to_jsonb(c) FROM eval_experiment_run_cases c WHERE tenant_id=$1 AND run_id=$2::text::uuid ORDER BY case_id,trial_index").bind(tenant).bind(run).fetch_all(&self.pool).await?)
    }
    pub async fn promote_baseline_storage(
        &self,
        user: &CompatUser,
        experiment: &str,
        run: &str,
        previous: Option<&str>,
    ) -> Result<Option<Value>, CompatError> {
        let mut tx = self.pool.begin().await?;
        let current=sqlx::query_scalar::<_,Option<String>>("SELECT baseline_run_id::text FROM eval_experiments WHERE tenant_id=$1 AND experiment_id=$2::text::uuid FOR UPDATE").bind(&user.tenant_id).bind(experiment).fetch_optional(&mut *tx).await?;
        let Some(current) = current else {
            return Ok(None);
        };
        if current.as_deref() != previous {
            return Ok(None);
        }
        let eligible=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM eval_experiment_runs WHERE tenant_id=$1 AND experiment_id=$2::text::uuid AND run_id=$3::text::uuid AND run_mode='live_candidate' AND status='succeeded')").bind(&user.tenant_id).bind(experiment).bind(run).fetch_one(&mut *tx).await?;
        if !eligible {
            return Ok(None);
        }
        let result=sqlx::query_scalar::<_,Value>("WITH changed AS (UPDATE eval_experiments SET baseline_run_id=$3::text::uuid,baseline_promoted_by=$4,baseline_promoted_at=NOW(),updated_at=NOW() WHERE tenant_id=$1 AND experiment_id=$2::text::uuid RETURNING experiment_id,baseline_run_id,baseline_promoted_by,baseline_promoted_at) SELECT jsonb_build_object('experiment_id',experiment_id,'baseline_run_id',baseline_run_id,'promoted_by',baseline_promoted_by,'promoted_at',baseline_promoted_at,'previous_baseline_run_id',$5::text) FROM changed").bind(&user.tenant_id).bind(experiment).bind(run).bind(&user.user_id).bind(previous).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO eval_baseline_promotions(tenant_id,experiment_id,previous_baseline_run_id,baseline_run_id,promoted_by) VALUES($1,$2::text::uuid,$3::text::uuid,$4::text::uuid,$5)").bind(&user.tenant_id).bind(experiment).bind(previous).bind(run).bind(&user.user_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(result))
    }
}
