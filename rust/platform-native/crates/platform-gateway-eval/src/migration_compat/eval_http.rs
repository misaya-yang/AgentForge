//! Evaluation command admission using canonical Agent version and Runtime authority.
use super::{
    common::{CompatError, CompatUser, object, text},
    eval::EvalService,
    eval_queue::is_runnable_dataset_example,
};
use crate::ports::AgentPort;
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Clone, Default)]
pub struct EvalHttpOptions {
    pub agent: Option<Arc<dyn AgentPort>>,
    pub default_model: String,
    pub traces: Option<Arc<crate::migration_trace::TraceRepository>>,
    pub ragas: Option<Arc<super::eval_clients::KbRagasClient>>,
    pub admission: Option<Arc<ai_platform_auth::admission::AdmissionService>>,
    pub rbac: Option<Arc<ai_platform_auth::rbac::Rbac>>,
}
/// The Gateway composition root injects the configured model into these options.
/// Evaluation has no concrete AgentState dependency and must never discover a
/// model from ambient environment or a fabricated Runtime/Agent default.
fn apply_default_model(
    execution: &mut Value,
    options: &EvalHttpOptions,
) -> Result<(), CompatError> {
    if matches!(text(execution, "model_id").trim(), "" | "current") {
        if options.default_model.is_empty() {
            return Err(CompatError::new(503, "Default model unavailable"));
        }
        execution["model_id"] = json!(options.default_model);
    }
    Ok(())
}
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
impl EvalService {
    pub async fn resolve_agent_version_candidate(
        &self,
        user: &CompatUser,
        options: &EvalHttpOptions,
        agent: &str,
        version: &str,
    ) -> Result<Value, CompatError> {
        if !user.authenticated || user.user_id.is_empty() {
            return Err(CompatError::new(403, "Authenticated Agent viewer required"));
        }
        let state = options
            .agent
            .as_ref()
            .ok_or_else(|| CompatError::new(503, "Agent runtime unavailable"))?;
        state
            .resolve_agent_version_candidate(user, agent, version)
            .await
    }

    pub async fn run_experiment(
        &self,
        user: &CompatUser,
        experiment_id: &str,
        body: &Value,
        options: &EvalHttpOptions,
    ) -> Result<Value, CompatError> {
        let experiment = self
            .get(&user.tenant_id, "experiments", experiment_id)
            .await?
            .ok_or_else(|| CompatError::new(404, "Experiment not found"))?;
        let evaluators = body["evaluator_ids"]
            .as_array()
            .filter(|v| (1..=10).contains(&v.len()) && v.iter().all(Value::is_string))
            .ok_or_else(|| CompatError::new(422, "evaluator_ids must contain 1 to 10 strings"))?;
        let candidate = body
            .get("candidate_config")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(json!({}));
        if !candidate.is_object()
            || object(&candidate).keys().any(|k| {
                !matches!(
                    k.as_str(),
                    "system_prompt_override" | "agent_id" | "agent_version_id"
                )
            })
        {
            return Err(CompatError::new(422, "Invalid candidate_config"));
        }
        let agent = candidate["agent_id"].as_str();
        let version = candidate["agent_version_id"].as_str();
        if agent.is_some() != version.is_some() {
            return Err(CompatError::new(
                422,
                "agent_id and agent_version_id must be provided together",
            ));
        }
        if agent.is_some() && !candidate["system_prompt_override"].is_null() {
            return Err(CompatError::new(
                422,
                "Agent Version candidates do not accept prompt overrides",
            ));
        }
        if text(&candidate, "system_prompt_override").chars().count() > 16000 {
            return Err(CompatError::new(
                422,
                "system_prompt_override exceeds 16000 characters",
            ));
        }
        let repetitions = body
            .get("repetitions")
            .filter(|v| !v.is_null())
            .map(|v| {
                v.as_i64()
                    .filter(|n| (1..=10).contains(n))
                    .ok_or_else(|| CompatError::new(422, "Invalid repetitions"))
            })
            .transpose()?
            .unwrap_or(3) as i32;
        let mode = if text(body, "run_mode").is_empty() {
            "rescore_trace"
        } else {
            text(body, "run_mode")
        };
        if !matches!(mode, "rescore_trace" | "live_candidate") {
            return Err(CompatError::new(422, "Invalid run_mode"));
        }
        for key in ["target_snapshot", "metadata"] {
            if body.get(key).is_some_and(|v| !v.is_object()) {
                return Err(CompatError::new(422, format!("{key} must be an object")));
            }
        }
        for key in ["candidate_label", "baseline_label"] {
            if text(body, key).chars().count() > 96 {
                return Err(CompatError::new(422, "Eval label too long"));
            }
        }
        let dataset = body["dataset_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| experiment["dataset_id"].as_str());
        let trace = body["target_snapshot"]["trace_id"]
            .as_str()
            .filter(|s| !s.is_empty());
        if dataset.is_some() && trace.is_some() {
            return Err(CompatError::new(
                422,
                "Dataset and trace targets are mutually exclusive",
            ));
        }
        if mode == "live_candidate" {
            let identity_keys = [
                "candidate_type",
                "agent_id",
                "agent_version_id",
                "agent_spec_hash",
                "agent_runtime_snapshot_hash",
            ];
            if agent.is_none()
                && [&experiment["target_config"], &body["target_snapshot"]]
                    .iter()
                    .any(|v| identity_keys.iter().any(|key| v.get(*key).is_some()))
            {
                return Err(CompatError::new(
                    422,
                    "Agent Version identity requires typed candidate_config",
                ));
            }
            let dataset_id = dataset
                .ok_or_else(|| CompatError::new(422, "live_candidate requires a dataset"))?;
            let info = self
                .get(&user.tenant_id, "datasets", dataset_id)
                .await?
                .ok_or_else(|| CompatError::new(404, "Dataset not found"))?;
            let examples = self
                .list_example_manifest(&user.tenant_id, dataset_id)
                .await?
                .into_iter()
                .filter(is_runnable_dataset_example)
                .collect::<Vec<_>>();
            if examples.is_empty() {
                return Err(CompatError::new(422, "Dataset has no approved examples"));
            }
            let mut invalid = vec![];
            let frozen = super::eval_queue::build_eval_dataset_manifest(&info, &examples);
            for case in arr(&frozen["examples"]) {
                let mut errors = super::eval_behavior::validate_case(case);
                if text(&case["input"], "message").trim().is_empty() {
                    errors.push("input.message must be a non-empty executable message".into());
                }
                if case["metadata"]["behavior_confirmed"] == false {
                    errors.push("expected behavior must be confirmed before live execution".into());
                }
                if !errors.is_empty() {
                    invalid.push(json!({"case_id":case["case_id"],"errors":errors}));
                }
            }
            if !invalid.is_empty() {
                return Err(CompatError::value(
                    422,
                    json!({"message":"live_candidate dataset contains invalid behavior contracts","cases":invalid.into_iter().take(20).collect::<Vec<_>>()}),
                ));
            }
            let mut suite = vec![];
            for evaluator in evaluators {
                let id = evaluator.as_str().unwrap();
                suite.push(
                    self.get(&user.tenant_id, "evaluators", id)
                        .await?
                        .ok_or_else(|| {
                            CompatError::new(404, format!("Evaluator not found: {id}"))
                        })?,
                );
            }
            let (execution, mut public, fingerprint) = if let Some(agent) = agent {
                if !object(&body["target_snapshot"]).is_empty() {
                    return Err(CompatError::new(
                        422,
                        "Agent Version candidates do not accept target_snapshot overrides",
                    ));
                }
                let resolved = self
                    .resolve_agent_version_candidate(user, options, agent, version.unwrap())
                    .await?;
                let mut fingerprint = json!({"verification":"pending"});
                for key in [
                    "candidate_type",
                    "agent_id",
                    "agent_version_id",
                    "agent_spec_hash",
                    "agent_runtime_snapshot_hash",
                    "model_id",
                    "provider_id",
                ] {
                    fingerprint[key] = resolved[key].clone();
                }
                (resolved.clone(), resolved, fingerprint)
            } else {
                let prompt = candidate
                    .get("system_prompt_override")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(Value::Null);
                let prompt_hash = prompt.as_str().filter(|s| !s.is_empty()).map(|s| {
                    use sha2::{Digest, Sha256};
                    super::common::hex(&Sha256::digest(s.as_bytes()))
                });
                let mut execution = object(&experiment["target_config"]);
                execution.extend(object(&body["target_snapshot"]));
                execution.insert("system_prompt_override".into(), prompt);
                let mut execution = json!(execution);
                apply_default_model(&mut execution, options)?;
                let mut public = object(&body["target_snapshot"]);
                for key in [
                    "system_prompt",
                    "system_prompt_override",
                    "eval_system_prompt_override",
                ] {
                    public.remove(key);
                }
                public.insert("prompt_override_hash".into(), json!(prompt_hash));
                let fingerprint = json!({"prompt_override_hash":prompt_hash,"requested_model_id":execution["model_id"],"requested_temperature":execution["temperature"],"requested_execution_profile":execution["execution_profile"],"verification":"pending"});
                (execution, json!(public), fingerprint)
            };
            for (key, value) in [
                ("dataset_id", json!(dataset_id)),
                ("dataset_name", info["name"].clone()),
                ("dataset_version", info["version"].clone()),
                (
                    "dataset_kb_linked",
                    json!(!text(&info["metadata"], "kb_dataset_id").is_empty()),
                ),
                ("approved_case_count", json!(examples.len())),
                (
                    "candidate_label",
                    body.get("candidate_label")
                        .cloned()
                        .unwrap_or(json!("candidate")),
                ),
                ("baseline_label", body["baseline_label"].clone()),
            ] {
                public[key] = value;
            }
            let job = self
                .enqueue_live_experiment_run(
                    user,
                    experiment_id,
                    dataset_id,
                    &suite,
                    &examples,
                    repetitions,
                    public,
                    execution,
                    fingerprint,
                    body["baseline_run_id"]
                        .as_str()
                        .or_else(|| experiment["baseline_run_id"].as_str()),
                )
                .await?;
            return Ok(json!({"jobs":[job]}));
        }
        if agent.is_some() {
            return Err(CompatError::new(
                422,
                "Agent Version candidates require live_candidate",
            ));
        }
        let mut jobs = vec![];
        for evaluator in evaluators {
            let mut target = json!(object(&body["target_snapshot"]));
            target["run_mode"] = json!("rescore_trace");
            target["candidate_label"] = body
                .get("candidate_label")
                .cloned()
                .unwrap_or(json!("candidate"));
            target["baseline_label"] = body["baseline_label"].clone();
            jobs.push(self.enqueue_evaluator_run(user,&json!({"evaluator_id":evaluator,"experiment_id":experiment_id,"dataset_id":dataset,"trace_id":trace,"target_snapshot":target,"metadata":body["metadata"]})).await?);
        }
        Ok(json!({"jobs":jobs}))
    }
    pub async fn cancel_and_interrupt(
        &self,
        user: &CompatUser,
        run: &str,
        options: &EvalHttpOptions,
    ) -> Result<Value, CompatError> {
        let mut result = self.cancel_experiment_run(user, run).await?;
        let cases = result
            .as_object_mut()
            .unwrap()
            .remove("cases")
            .unwrap_or(json!([]));
        let mut pending = 0;
        for case in arr(&cases) {
            let mut handle = json!(object(&case["runtime_handle"]));
            if text(&handle, "turn_id").is_empty() && !text(&handle, "thread_id").is_empty() {
                let rows=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('run_id',run_id,'runtime_thread_id',runtime_thread_id) FROM assistant_runtime_snapshots WHERE tenant_id=$1 AND session_id=$2 AND user_id=$3 AND runtime_thread_id=$4::text::uuid ORDER BY created_at LIMIT 2").bind(&user.tenant_id).bind(text(case,"run_case_id")).bind(text(&handle,"user_id")).bind(text(&handle,"thread_id")).fetch_all(&self.pool).await?;
                if rows.len() == 1 {
                    handle["turn_id"] = rows[0]["run_id"].clone();
                    handle["thread_id"] = rows[0]["runtime_thread_id"].clone();
                }
            }
            let Some(agent) = &options.agent else {
                pending += 1;
                continue;
            };
            if ["turn_id", "thread_id", "user_id"]
                .iter()
                .any(|key| text(&handle, key).is_empty())
            {
                pending += 1;
                continue;
            }
            if agent
                .interrupt(
                    &user.tenant_id,
                    text(&handle, "user_id"),
                    text(case, "run_case_id"),
                    text(&handle, "thread_id"),
                    text(&handle, "turn_id"),
                )
                .await
                .is_err()
            {
                pending += 1;
            }
        }
        result["runtime_interrupt_pending"] = json!(pending);
        Ok(result)
    }
}

impl EvalService {
    pub async fn dry_run_gate(
        &self,
        user: &CompatUser,
        body: &Value,
    ) -> Result<Value, CompatError> {
        let baseline = body["baseline_run_id"].as_str().filter(|s| !s.is_empty());
        let candidate = body["candidate_run_id"].as_str().filter(|s| !s.is_empty());
        if baseline.is_some() != candidate.is_some() {
            return Err(CompatError::new(
                422,
                "baseline_run_id and candidate_run_id must be supplied together",
            ));
        }
        let mut metrics = body["result_payload"]
            .get("metrics")
            .filter(|v| !v.is_null() && v.as_object().is_none_or(|v| !v.is_empty()))
            .cloned()
            .unwrap_or(body["result_payload"].clone());
        let (mut baseline_metrics, mut compatibility, mut authoritative) =
            (None, json!({}), json!({}));
        if let Some(baseline) = baseline {
            let comparison = self
                .compare_experiment_runs(&user.tenant_id, baseline, candidate.unwrap())
                .await?
                .ok_or_else(|| CompatError::new(404, "Experiment run not found"))?;
            metrics = comparison["candidate_summary"].clone();
            baseline_metrics = Some(comparison["baseline_summary"].clone());
            compatibility = json!(object(&comparison["compatibility"]));
            authoritative = json!(object(&comparison["gate"]));
        }
        let mut errors = vec![];
        for value in std::iter::once(&metrics).chain(baseline_metrics.iter()) {
            if text(value, "schema_version") != "eval-gate-metrics/v2" {
                errors.push("schema_version must be eval-gate-metrics/v2");
            }
            for key in [
                "case_count",
                "failed_case_count",
                "trajectory_case_count",
                "trajectory_failed_count",
                "critical_case_count",
                "critical_failed_count",
                "stateful_case_count",
                "stateful_failed_count",
            ] {
                if value[key].as_u64().is_none() {
                    errors.push(key);
                }
            }
            for key in [
                "score_sum",
                "overall_score",
                "pass_rate",
                "trajectory_pass_rate",
            ] {
                if value[key]
                    .as_f64()
                    .is_none_or(|n| !n.is_finite() || n < 0.0 || key != "score_sum" && n > 1.0)
                {
                    errors.push(key);
                }
            }
            for key in ["critical_pass_rate", "stateful_pass_rate"] {
                if !value[key].is_null()
                    && value[key]
                        .as_f64()
                        .is_none_or(|n| !n.is_finite() || !(0.0..=1.0).contains(&n))
                {
                    errors.push(key);
                }
            }
            if value["trajectory_case_count"] != value["case_count"] {
                errors.push("trajectory_case_count must equal case_count");
            }
            for key in ["critical_case_count", "stateful_case_count"] {
                if value[key].as_u64() > value["case_count"].as_u64() {
                    errors.push(key);
                }
            }
            for key in [
                "trajectory_failed_count",
                "critical_failed_count",
                "stateful_failed_count",
            ] {
                if value[key].as_u64() > value["failed_case_count"].as_u64() {
                    errors.push(key);
                }
            }
        }
        if !errors.is_empty() {
            return Err(CompatError::value(
                422,
                json!({"error":"incompatible_gate_metrics_schema","required_schema_version":"eval-gate-metrics/v2","validation_errors":errors}),
            ));
        }
        let mut gate = super::eval_gate::apply_gate(
            &metrics,
            body.get("thresholds"),
            baseline_metrics.as_ref(),
            true,
            true,
        );
        if baseline.is_some() {
            let mut failures = arr(&gate["failures"]).to_vec();
            for (key, valid, label) in [
                (
                    "reasons",
                    compatibility["compatible"] == true && compatibility["status"] == "compatible",
                    "authoritative run comparison is incompatible",
                ),
                (
                    "failures",
                    authoritative["status"] == "pass",
                    "authoritative run comparison gate failed",
                ),
            ] {
                if !valid {
                    let source = if key == "reasons" {
                        &compatibility
                    } else {
                        &authoritative
                    };
                    let reason = arr(&source[key])
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let failure = json!(format!(
                        "{label}: {}",
                        if reason.is_empty() {
                            "unknown"
                        } else {
                            &reason
                        }
                    ));
                    if !failures.contains(&failure) {
                        failures.push(failure);
                    }
                    gate["status"] = json!("fail");
                }
            }
            gate["failures"] = json!(failures);
        }
        gate["compatibility"] = compatibility.clone();
        gate["authoritative_gate"] = authoritative.clone();
        gate["report"] = json!({"source":"api-dry-run","gate_profile":"release","baseline_run_id":baseline,"candidate_run_id":candidate,"compatibility":compatibility,"authoritative_gate":authoritative});
        Ok(gate)
    }
    pub async fn run_results(
        &self,
        tenant: &str,
        id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Value, CompatError> {
        let run = self
            .get(tenant, "experiment-runs", id)
            .await?
            .ok_or_else(|| CompatError::new(404, "Experiment run not found"))?;
        let (cases, total) = if run["run_mode"] == "live_candidate" {
            let total=sqlx::query_scalar::<_,i64>("SELECT COUNT(DISTINCT case_id) FROM eval_experiment_run_cases WHERE tenant_id=$1 AND run_id=$2::text::uuid").bind(tenant).bind(id).fetch_one(&self.pool).await?;
            let rows=sqlx::query_scalar::<_,Value>("WITH paged_cases AS (SELECT case_id,MIN(created_at) AS first_created_at FROM eval_experiment_run_cases WHERE tenant_id=$1 AND run_id=$2::text::uuid GROUP BY case_id ORDER BY first_created_at,case_id LIMIT $3 OFFSET $4) SELECT to_jsonb(q) FROM (SELECT c.*,t.trace_family,t.status AS trace_status,t.model_id,t.provider FROM eval_experiment_run_cases c INNER JOIN paged_cases p ON p.case_id=c.case_id LEFT JOIN agent_traces t ON t.tenant_id=c.tenant_id AND t.trace_id=c.candidate_trace_id WHERE c.tenant_id=$1 AND c.run_id=$2::text::uuid ORDER BY p.first_created_at,c.case_id,c.trial_index) q").bind(tenant).bind(id).bind(limit).bind(offset).fetch_all(&self.pool).await?;
            let grouped = super::eval_comparison::aggregate_live_case_rows(&rows);
            let mut seen = std::collections::BTreeSet::new();
            let cases = rows
                .iter()
                .filter_map(|v| {
                    let key = text(v, "case_id");
                    if !seen.insert(key) {
                        return None;
                    }
                    grouped.get(key)
                })
                .map(|case| {
                    let reason = if arr(&case["errors"]).is_empty() {
                        arr(&case["contract_failures"])
                    } else {
                        arr(&case["errors"])
                    }
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ");
                    let reason = if reason.is_empty() {
                        if matches!(text(case, "execution_status"), "cancelled" | "skipped") {
                            json!(format!(
                                "Trial {} before scoring",
                                text(case, "execution_status")
                            ))
                        } else {
                            Value::Null
                        }
                    } else {
                        json!(reason)
                    };
                    let mut public = json!({"failure_reason":reason,"scores":[]});
                    for key in [
                        "example_id",
                        "case_id",
                        "candidate_trace_id",
                        "status",
                        "execution_status",
                        "retry_available",
                        "aggregate_score",
                        "input",
                        "expected_output",
                        "trial_count",
                        "score_stddev",
                        "flaky",
                        "observed_metrics",
                        "trace",
                    ] {
                        public[key] = case[key].clone();
                    }
                    if public["candidate_trace_id"].is_null() {
                        public["candidate_trace_id"] = json!("");
                    }
                    public
                })
                .collect::<Vec<_>>();
            (cases, total)
        } else {
            self.rescore_results(tenant, id, limit, offset).await?
        };
        Ok(json!({"run":run,"cases":cases,"total":total,"limit":limit,"offset":offset}))
    }
    async fn rescore_results(
        &self,
        tenant: &str,
        id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Value>, i64), CompatError> {
        let case_key = "COALESCE(NULLIF(s.metadata->>'example_id',''),NULLIF(s.metadata->>'case_id',''),s.trace_id::text)";
        let total=sqlx::query_scalar::<_,i64>(&format!("SELECT COUNT(DISTINCT {case_key}) FROM agent_trace_scores s INNER JOIN agent_traces t ON t.trace_id=s.trace_id AND t.tenant_id=$1 WHERE s.metadata->>'experiment_run_id'=$2")).bind(tenant).bind(id).fetch_one(&self.pool).await?;
        let rows=sqlx::query_scalar::<_,Value>(&format!("WITH ranked_scores AS (SELECT s.*,{case_key} AS case_key,ROW_NUMBER() OVER(PARTITION BY {case_key},s.score_name,s.target_type,COALESCE(NULLIF(s.target_id,''),s.span_id::text,s.trace_id::text) ORDER BY s.created_at DESC,s.score_id DESC) AS score_revision FROM agent_trace_scores s INNER JOIN agent_traces t ON t.trace_id=s.trace_id AND t.tenant_id=$1 WHERE s.metadata->>'experiment_run_id'=$2),paged_cases AS (SELECT case_key,MAX(created_at) AS latest_created_at FROM ranked_scores WHERE score_revision=1 GROUP BY case_key ORDER BY latest_created_at DESC,case_key LIMIT $3 OFFSET $4) SELECT to_jsonb(q) FROM (SELECT s.case_key,s.score_id,s.trace_id AS candidate_trace_id,s.score_name,s.target_type,s.target_id,s.span_id,s.numeric_value,s.label AS score_label,s.explanation AS score_explanation,s.score_source,s.metadata AS score_metadata,e.example_id,e.source_trace_id,e.input,e.expected_output,e.metadata AS example_metadata,t.trace_family,t.status AS trace_status,t.model_id,t.provider,t.total_latency_ms,t.total_tokens,t.output_preview FROM ranked_scores s INNER JOIN paged_cases p ON p.case_key=s.case_key INNER JOIN agent_traces t ON t.trace_id=s.trace_id AND t.tenant_id=$1 LEFT JOIN eval_examples e ON e.tenant_id=$1 AND e.example_id::text=NULLIF(s.metadata->>'example_id','') WHERE s.score_revision=1 ORDER BY p.latest_created_at DESC,s.case_key,s.score_name) q")).bind(tenant).bind(id).bind(limit).bind(offset).fetch_all(&self.pool).await?;
        let mut cases: Vec<Value> = vec![];
        for row in rows {
            let key = text(&row, "case_key");
            let index=cases.iter().position(|v|v["_key"]==key).unwrap_or_else(||{let fallback=|values:Vec<&Value>|values.into_iter().find(|v|v.as_str().is_some_and(|s|!s.is_empty())).cloned().unwrap_or(Value::Null);cases.push(json!({"_key":key,"example_id":fallback(vec![&row["example_id"],&row["score_metadata"]["example_id"]]),"case_id":fallback(vec![&row["score_metadata"]["case_id"],&row["example_metadata"]["case_id"],&row["example_id"],&row["case_key"]]),"candidate_trace_id":row["candidate_trace_id"],"source_trace_id":row["source_trace_id"],"status":"unscored","aggregate_score":null,"failure_reason":null,"input":object(&row["input"]),"expected_output":object(&row["expected_output"]),"trace":{"trace_family":row["trace_family"],"status":row["trace_status"],"model_id":row["model_id"],"provider":row["provider"],"total_latency_ms":row["total_latency_ms"],"total_tokens":row["total_tokens"],"output_preview":row["output_preview"]},"scores":[]}));cases.len()-1});
            cases[index]["scores"].as_array_mut().unwrap().push(json!({"score_name":row["score_name"],"target_type":row["target_type"],"target_id":row["target_id"],"span_id":row["span_id"],"numeric_value":row["numeric_value"],"label":row["score_label"],"explanation":text(&row,"score_explanation"),"score_source":row["score_source"],"failure_kind":row["score_metadata"]["failure_kind"]}));
        }
        for case in &mut cases {
            case.as_object_mut().unwrap().remove("_key");
            let scores = arr(&case["scores"]);
            let numbers = scores
                .iter()
                .filter(|v| matches!(text(v, "label"), "pass" | "fail"))
                .filter_map(|v| v["numeric_value"].as_f64())
                .collect::<Vec<_>>();
            let failure = scores
                .iter()
                .find(|v| v["label"] == "fail" || v["failure_kind"] == "infrastructure");
            let review = scores.iter().find(|v| v["label"] == "review");
            let (status, reason) = if let Some(failure) = failure {
                (
                    "failed",
                    Some(
                        if text(failure, "explanation").is_empty() {
                            "Evaluation failed"
                        } else {
                            text(failure, "explanation")
                        }
                        .to_owned(),
                    ),
                )
            } else if let Some(review) = review {
                (
                    "review",
                    Some(
                        if text(review, "explanation").is_empty() {
                            "Manual review required"
                        } else {
                            text(review, "explanation")
                        }
                        .to_owned(),
                    ),
                )
            } else if scores.iter().any(|v| v["label"] == "pass") {
                ("passed", None)
            } else {
                ("unscored", None)
            };
            case["status"] = json!(status);
            case["failure_reason"] = json!(reason);
            case["aggregate_score"] = if numbers.is_empty() {
                Value::Null
            } else {
                json!(
                    (numbers.iter().sum::<f64>() / numbers.len() as f64 * 10000.0)
                        .round_ties_even()
                        / 10000.0
                )
            };
        }
        Ok((cases, total))
    }
}

#[cfg(test)]
mod model_configuration_tests {
    use super::*;
    #[test]
    fn injected_model_resolves_current_and_missing_without_overwriting_frozen_models() {
        let options = EvalHttpOptions {
            default_model: "configured-model".into(),
            ..Default::default()
        };
        for value in [
            json!({}),
            json!({"model_id":"current"}),
            json!({"model_id":" current "}),
            json!({"model_id":null}),
        ] {
            let mut execution = value;
            apply_default_model(&mut execution, &options).unwrap();
            assert_eq!(execution["model_id"], "configured-model");
        }
        let mut explicit = json!({"model_id":"frozen-model","provider_id":"frozen-provider"});
        let before = explicit.clone();
        apply_default_model(&mut explicit, &options).unwrap();
        assert_eq!(explicit, before);
        apply_default_model(&mut explicit, &EvalHttpOptions::default()).unwrap();
        assert_eq!(explicit, before);
    }
    #[test]
    fn absent_configuration_keeps_original_unavailable_error_without_mutating_request() {
        let mut execution = json!({"model_id":"current","provider_id":"provider"});
        let before = execution.clone();
        let error = apply_default_model(&mut execution, &EvalHttpOptions::default()).unwrap_err();
        assert_eq!(error.status, 503);
        assert_eq!(error.detail, "Default model unavailable");
        assert_eq!(execution, before);
    }
}
