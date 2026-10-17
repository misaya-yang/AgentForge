//! Frozen candidate replay, measured evidence and live-run aggregation.
use super::{
    common::{CompatError, object, text},
    eval_behavior::evaluate_case,
    eval_candidate::EvalCandidateClient,
    eval_clients::LlmCompleteContext,
    eval_executor::{EvaluatorExecutor, EvaluatorRunResult, trace_target},
    eval_observation::{candidate_fingerprint_from_context, runtime_model_evidence},
};
use crate::migration_trace::TraceRepository;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
pub const HARD_BLOCKERS: [&str; 2] = [
    "assistant.runtime.policy_bypass",
    "assistant.runtime.repeated_unknown_side_effect",
];
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn storage(e: crate::migration_trace::TraceError) -> CompatError {
    CompatError::new(e.status, e.code)
}
fn failure(value: &str) -> CompatError {
    CompatError::new(503, value)
}
fn terminal(detail: &Value) -> bool {
    matches!(
        text(&detail["trace"], "status"),
        "succeeded" | "failed" | "cancelled" | "timeout"
    )
}
fn round(value: f64, precision: i32) -> f64 {
    let factor = 10f64.powi(precision);
    (value * factor).round_ties_even() / factor
}
pub fn candidate_cost(usage: &Value, snapshot: &Value) -> Option<f64> {
    if ![
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "completion_tokens",
    ]
    .iter()
    .any(|k| usage.get(*k).is_some())
        || snapshot["pricing_status"] == "unknown"
    {
        return None;
    }
    let tokens = |a: &str, b: &str| {
        usage[a]
            .as_i64()
            .filter(|v| *v != 0)
            .or_else(|| usage[b].as_i64())
            .unwrap_or(0)
            .max(0) as f64
    };
    let rate = |k: &str| {
        snapshot[k]
            .as_str()
            .and_then(|v| v.parse::<f64>().ok())
            .or_else(|| snapshot[k].as_f64())
    };
    Some(round(
        (tokens("input_tokens", "prompt_tokens") * rate("input_price_per_1k")?
            + tokens("output_tokens", "completion_tokens") * rate("output_price_per_1k")?)
            / 10.0,
        6,
    ))
}
pub struct EvalCandidateRunner {
    pub client: Arc<EvalCandidateClient>,
    pub traces: Arc<TraceRepository>,
}
impl EvalCandidateRunner {
    pub async fn run_candidate(
        &self,
        job: &Value,
        case: &Value,
        config: &Value,
    ) -> Result<Value, CompatError> {
        let tenant = text(job, "tenant_id");
        let case_id = text(case, "run_case_id");
        let repository = self.traces.with_outbox_claim(job).map_err(storage)?;
        let mut trace = text(case, "candidate_trace_id").to_owned();
        let mut detail = if !trace.is_empty() {
            repository
                .get_trace_detail(tenant, &trace, None, "assistant")
                .await
                .map_err(storage)?
        } else {
            None
        };
        if detail.is_none() {
            let user = if text(&case["runtime_handle"], "user_id").is_empty() {
                "eval-candidate"
            } else {
                text(&case["runtime_handle"], "user_id")
            };
            let existing=sqlx::query_scalar::<_,String>("SELECT trace_id::text FROM agent_traces WHERE tenant_id=$1 AND user_id=$2 AND trace_family='assistant' AND session_id=$3 ORDER BY started_at DESC LIMIT 1").bind(tenant).bind(user).bind(case_id).fetch_optional(&self.client.outbox.pool).await?;
            if let Some(id) = existing {
                trace = id;
                detail = repository
                    .get_trace_detail(tenant, &trace, None, "assistant")
                    .await
                    .map_err(storage)?;
            }
        }
        if detail.as_ref().is_some_and(|v| !terminal(v)) {
            detail = None;
        }
        let mut result = None;
        if detail.is_none() {
            let mut observed = self.client.run(job, case, config).await?;
            trace = observed.trace_id.clone();
            if config["model_ref"].is_object() && !object(&config["model_ref"]).is_empty() {
                let evidence = self
                    .client
                    .runtime_evidence(tenant, case_id, &trace)
                    .await?;
                let (fingerprint, usage, spans) = runtime_model_evidence(
                    &evidence,
                    tenant,
                    case_id,
                    &trace,
                    &config["model_ref"],
                )?;
                observed
                    .fingerprint
                    .as_object_mut()
                    .ok_or_else(|| failure("AGENT_EVAL_MODEL_EVIDENCE_UNAVAILABLE"))?
                    .extend(object(&fingerprint));
                observed.usage = usage;
                let trace_payload = observed
                    .trace_payload
                    .as_mut()
                    .ok_or_else(|| failure("AGENT_EVAL_TRACE_CAPTURE_UNAVAILABLE"))?;
                trace_payload["provider"] = observed.fingerprint["provider"].clone();
                trace_payload["model_id"] = observed.fingerprint["model_id"].clone();
                for key in ["input_tokens", "output_tokens", "total_tokens"] {
                    trace_payload["metrics"][key] = observed.usage[key].clone();
                }
                trace_payload["metadata"]["candidate_fingerprint"] = observed.fingerprint.clone();
                let mut original = arr(&trace_payload["spans"]).to_vec();
                original.extend(spans);
                trace_payload["spans"] = json!(original);
            }
            if let Some(payload) = &observed.trace_payload {
                let subject = text(payload, "user_id");
                if subject.trim().is_empty() {
                    return Err(failure("AGENT_EVAL_TRACE_IDENTITY_UNAVAILABLE"));
                }
                repository
                    .ingest_trace(
                        tenant,
                        subject,
                        &json!({"trace":payload,"enqueue":false}),
                        false,
                    )
                    .await
                    .map_err(storage)?;
            }
            for delay in [50, 100, 200, 400, 800, 1600, 3200, 3200] {
                detail = repository
                    .get_trace_detail(tenant, &trace, None, "assistant")
                    .await
                    .map_err(storage)?;
                if detail.as_ref().is_some_and(terminal) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            result = Some(observed);
        }
        let mut detail = detail.ok_or_else(|| failure("Candidate trace persistence timed out"))?;
        if let Some(error) = result.as_ref().and_then(|r| r.error.as_deref()) {
            return Err(failure(error));
        }
        let mut fingerprint = result
            .as_ref()
            .map(|r| r.fingerprint.clone())
            .unwrap_or_else(|| {
                if detail["trace"]["metadata"]["candidate_fingerprint"].is_object() {
                    detail["trace"]["metadata"]["candidate_fingerprint"].clone()
                } else {
                    arr(&detail["spans"])
                        .iter()
                        .find(|s| s["span_kind"] == "context_building")
                        .map(|s| candidate_fingerprint_from_context(&s["attributes"]))
                        .unwrap_or(json!({}))
                }
            });
        let mut recovered_usage = None;
        if result.is_none()
            && config["model_ref"].is_object()
            && !object(&config["model_ref"]).is_empty()
        {
            let evidence = self
                .client
                .runtime_evidence(tenant, case_id, &trace)
                .await?;
            let (recovered, usage, spans) =
                runtime_model_evidence(&evidence, tenant, case_id, &trace, &config["model_ref"])?;
            fingerprint
                .as_object_mut()
                .ok_or_else(|| failure("AGENT_EVAL_MODEL_EVIDENCE_UNAVAILABLE"))?
                .extend(object(&recovered));
            recovered_usage = Some(usage);
            if config["candidate_type"] == "agent_version"
                && [
                    "agent_id",
                    "agent_version_id",
                    "agent_spec_hash",
                    "agent_runtime_snapshot_hash",
                ]
                .iter()
                .any(|key| fingerprint[*key] != config[*key])
            {
                return Err(failure("AGENT_EVAL_VERSION_PIN_MISMATCH"));
            }
            let mut existing = arr(&detail["spans"]).to_vec();
            let ids = existing
                .iter()
                .map(|v| text(v, "span_id").to_owned())
                .collect::<BTreeSet<_>>();
            existing.extend(
                spans
                    .into_iter()
                    .filter(|s| !ids.contains(text(s, "span_id"))),
            );
            detail["spans"] = json!(existing);
        }
        if config["model_ref"].is_object()
            && !object(&config["model_ref"]).is_empty()
            && (fingerprint["model_ref_verified"] != true
                || [
                    "tenant_id",
                    "provider_id",
                    "model_id",
                    "capability_revision",
                    "price_version",
                ]
                .iter()
                .any(|key| fingerprint["model_ref"][*key] != config["model_ref"][*key]))
        {
            return Err(failure("AGENT_EVAL_MODEL_REF_MISMATCH"));
        }
        let trace_data = &detail["trace"];
        let usage = recovered_usage.unwrap_or_else(|| {
            result.as_ref().map(|r| r.usage.clone()).unwrap_or_else(|| {
                let mut u = json!({});
                for key in ["input_tokens", "output_tokens", "total_tokens"] {
                    if trace_data[key].is_number() {
                        u[key] = trace_data[key].clone();
                    }
                }
                u
            })
        });
        let output = result
            .as_ref()
            .map(|r| r.output.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(text(trace_data, "output_preview"));
        let total = usage["total_tokens"].as_i64().or_else(|| {
            if object(&usage).is_empty() {
                None
            } else {
                Some(
                    usage["input_tokens"]
                        .as_i64()
                        .or_else(|| usage["prompt_tokens"].as_i64())
                        .unwrap_or(0)
                        + usage["output_tokens"]
                            .as_i64()
                            .or_else(|| usage["completion_tokens"].as_i64())
                            .unwrap_or(0),
                )
            }
        });
        let mut observation = json!({"status":trace_data["status"],"output_preview":output,"span_kinds":arr(&detail["spans"]).iter().filter_map(|v|v["span_kind"].as_str()).collect::<Vec<_>>(),"spans":detail["spans"],"total_latency_ms":trace_data["total_latency_ms"],"total_tokens":total,"total_cost_cents":candidate_cost(&usage,&config["model_ref"]["pricing_snapshot"])});
        observation
            .as_object_mut()
            .unwrap()
            .extend(object(&trace_data["metadata"]["runtime_trajectory"]));
        let contract = json!({"case_id":case["case_id"],"input":case["input"],"expected_output":case["expected_output"],"expected_trajectory":case["expected_trajectory"],"assertions":case["assertions"],"metadata":case["metadata"]});
        Ok(
            json!({"trace_id":trace,"detail":detail,"usage":usage,"fingerprint":fingerprint,"contract_result":evaluate_case(&contract,Some(&observation))}),
        )
    }
}
fn observed_trajectory(target: &Value) -> (Vec<Value>, Vec<Value>) {
    let (mut tools, mut rag) = (vec![], vec![]);
    for span in arr(&target["spans"]) {
        if !span.is_object() {
            continue;
        }
        let mut item = json!({"name":span["name"],"status":span["status"],"sequence_no":span["sequence_no"],"input_preview":text(span,"input_preview"),"output_preview":text(span,"output_preview"),"error_message":text(span,"error_message")});
        let kind = text(span, "span_kind");
        if kind == "tool_execution" {
            tools.push(item);
        } else if matches!(
            kind,
            "retrieval" | "rag_retrieval" | "retriever" | "document_fetch"
        ) {
            let attributes = &span["attributes"];
            let docs = if attributes["retrieval.documents"].is_array() {
                arr(&attributes["retrieval.documents"])
            } else {
                arr(&attributes["retrieval"]["documents"])
            };
            item["dataset_ids"] = attributes
                .get("retrieval.dataset_ids")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(json!([]));
            item["documents"] = json!(
                docs.iter()
                    .filter(|v| v.is_object())
                    .map(|v| {
                        let mut map = serde_json::Map::new();
                        for key in ["document_id", "id", "chunk_id", "rank", "score"] {
                            if let Some(value) = v.get(key).filter(|v| !v.is_null()) {
                                map.insert(key.into(), value.clone());
                            }
                        }
                        json!(map)
                    })
                    .collect::<Vec<_>>()
            );
            rag.push(item);
        }
    }
    (tools, rag)
}
impl EvaluatorExecutor {
    pub async fn run_live_job(&self, job: &Value) -> Result<EvaluatorRunResult, CompatError> {
        let run = text(&job["payload"], "run_id");
        let tenant = text(job, "tenant_id");
        let Some(runner) = &self.candidate else {
            return self
                .fail(job, run, "Live candidate runner is not configured")
                .await;
        };
        let Some(stored) = self.service.get(tenant, "experiment-runs", run).await? else {
            return Ok(EvaluatorRunResult::terminal(
                run,
                "failed",
                json!({}),
                json!({}),
                Some("Run not found".into()),
                0,
            ));
        };
        let config = &stored["execution_config"];
        let evaluators = arr(&config["evaluators"]);
        if evaluators.is_empty() {
            return self
                .fail(job, run, "Frozen evaluator suite is missing")
                .await;
        }
        let cases=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(c) FROM eval_experiment_run_cases c WHERE tenant_id=$1 AND run_id=$2::text::uuid ORDER BY case_id,trial_index").bind(tenant).bind(run).fetch_all(&self.service.pool).await?;
        if cases
            .iter()
            .any(|case| case["metadata"]["source_kind"] == "kb_failure")
        {
            return self
                .fail(
                    job,
                    run,
                    "KB review cases cannot run without a source ACL execution contract",
                )
                .await;
        }
        let actor_scope = if evaluators.iter().any(|e| e["evaluator_type"] == "ragas") {
            Some(self.ragas_actor_scope(job).await?)
        } else {
            None
        };
        let (mut trials, mut written, mut errors) = (vec![], 0usize, vec![]);
        for case in &cases {
            let case_id = text(case, "run_case_id");
            let mut observed = json!({"case_id":case["case_id"],"trial_index":case["trial_index"].as_i64().unwrap_or(1),"critical":case["metadata"]["critical"]==true});
            observed
                .as_object_mut()
                .unwrap()
                .extend(object(&case["observed_metrics"]));
            if matches!(text(case, "status"), "succeeded" | "failed" | "skipped") {
                trials.push(observed);
                continue;
            }
            runner
                .client
                .persist_case(job, case_id, "running", None, None, None, None, None)
                .await?;
            let (mut trace_id, mut succeeded, mut target) = (None, false, None::<Value>);
            let trial_result:Result<Value,CompatError>=async{let candidate=runner.run_candidate(job,case,config).await?;if config["candidate_type"]=="agent_version"&&["agent_id","agent_version_id","agent_spec_hash","agent_runtime_snapshot_hash"].iter().any(|key|candidate["fingerprint"][*key]!=config[*key]){return Err(failure("AGENT_EVAL_VERSION_FINGERPRINT_MISMATCH"));}let mut t=trace_target(&candidate["detail"]).ok_or_else(||failure("Candidate trace was not persisted"))?;for key in ["example_id","case_id","expected_output","expected_trajectory","assertions"]{t[key]=case[key].clone();}let usage=&candidate["usage"];let known=["input_tokens","output_tokens","prompt_tokens","completion_tokens"].iter().any(|key|usage.get(*key).is_some());for(key,alias)in[("input_tokens","prompt_tokens"),("output_tokens","completion_tokens")]{t[key]=if known{json!(usage[key].as_i64().filter(|v|*v!=0).or_else(||usage[alias].as_i64()).unwrap_or(0))}else{Value::Null};}t["total_tokens"]=if known{json!(usage["total_tokens"].as_i64().filter(|v|*v!=0).unwrap_or(t["input_tokens"].as_i64().unwrap_or(0)+t["output_tokens"].as_i64().unwrap_or(0)))}else{Value::Null};t["total_cost_cents"]=json!(candidate_cost(usage,&config["model_ref"]["pricing_snapshot"]));trace_id=Some(text(&t,"trace_id").to_owned());succeeded=t["status"]=="succeeded";target=Some(t.clone());if !succeeded{return Err(failure(&format!("Candidate execution status: {}",text(&t,"status"))));}let trace=text(&t,"trace_id");let contract=&candidate["contract_result"];let contract_pass=contract["passed"]==true;let stateful=case["expected_trajectory"]["stateful"].is_object();let contract_failures=arr(&contract["failures"]).iter().filter_map(Value::as_str).collect::<Vec<_>>();let behavior=json!({"score_name":"behavior_contract","score_type":"numeric","numeric_value":if contract_pass{1.0}else{0.0},"label":if contract_pass{"pass"}else{"fail"},"explanation":if contract_failures.is_empty(){"behavior contract passed".into()}else{contract_failures.join("; ")},"scorer_type":"rule","score_source":"rule","evaluator_name":"behavior_contract","evaluator_version":"v1","confidence":1.0,"target_type":"trace","target_id":trace,"metadata":{"experiment_run_id":run,"run_case_id":case_id,"example_id":case["example_id"],"case_id":case["case_id"],"trial_index":case["trial_index"]}});if self.traces.create_eval_score_claimed(tenant,trace,&self.created_by,&behavior,"assistant",job).await.map_err(storage)?.is_some(){written+=1;}let mut scores=vec![if contract_pass{1.0}else{0.0}];let mut labels=vec![if contract_pass{"pass".to_owned()}else{"fail".to_owned()}];let context=LlmCompleteContext{tenant_id:tenant.into(),trace_family:"assistant".into(),trace_id:Some(trace.into()),actor_scope:actor_scope.clone(),..Default::default()};for evaluator in evaluators{if !evaluator.is_object()||evaluator["evaluator_type"]=="human"{continue;}for mut score in self.scorer.score_target(evaluator,&t,&context).await{let infrastructure=score["metadata"]["failure_kind"]=="infrastructure";let mut metadata=object(&score["metadata"]);for key in ["run_case_id","example_id","case_id","trial_index"]{metadata.insert(key.into(),case[key].clone());}metadata.insert("experiment_run_id".into(),json!(run));metadata.insert("candidate_trace_id".into(),json!(trace));score["metadata"]=json!(metadata);let created=self.traces.create_eval_score_claimed(tenant,trace,&self.created_by,&score,"assistant",job).await.map_err(storage)?;if infrastructure{return Err(failure("eval_judge_infrastructure_failure"));}if let Some(created)=created{written+=1;let label=text(&created,"label").to_owned();if matches!(label.as_str(),"pass"|"fail"){if let Some(number)=created["numeric_value"].as_f64().filter(|v|v.is_finite()){scores.push(number);}}labels.push(label);}}}let(tools,rag)=observed_trajectory(&t);let runtime=&t["metadata"]["runtime_trajectory"];let unknown=runtime["exit_reason"]=="side_effect_unknown"||tools.iter().any(|v|v["status"]=="side_effect_unknown");let performance=arr(&case["assertions"]).iter().all(|a|{let key=match text(a,"type"){"latency_ms_lt"=>"total_latency_ms","total_tokens_lt"=>"total_tokens","cost_cents_lt"=>"total_cost_cents",_=>return true};t[key].as_f64().zip(a["value"].as_f64()).is_some_and(|(v,max)|v<max)});Ok(json!({"case_id":case["case_id"],"trial_index":case["trial_index"].as_i64().unwrap_or(1),"critical":case["metadata"]["critical"]==true,"trace_id":trace,"execution_succeeded":succeeded,"deterministic_pass":contract_pass,"trajectory_pass":contract["trajectory_pass"]==true,"stateful_expected":stateful,"stateful_pass":if stateful{json!(contract["stateful_pass"]==true)}else{Value::Null},"behavior_pass":contract_pass&&labels.iter().all(|s|s=="pass"),"aggregate_score":scores.iter().sum::<f64>()/scores.len()as f64,"score_labels":labels,"latency_ms":t["total_latency_ms"],"input_tokens":t["input_tokens"],"output_tokens":t["output_tokens"],"total_tokens":t["total_tokens"],"cost_cents":t["total_cost_cents"],"output_preview":text(&t,"output_preview"),"fingerprint":candidate["fingerprint"],"contract_failures":contract_failures,"explicit_performance_pass":performance,"tool_trajectory":tools,"side_effect_unknown":unknown,"rag_evidence":rag,"exit_reason":runtime["exit_reason"]}))}.await;
            match trial_result {
                Ok(trial) => {
                    runner
                        .client
                        .persist_case(
                            job,
                            case_id,
                            "succeeded",
                            trace_id.as_deref(),
                            Some(&trial),
                            None,
                            None,
                            None,
                        )
                        .await?;
                    trials.push(trial);
                }
                Err(error) => {
                    if error.status == 409
                        && text(&json!({"message":error.detail}), "message").contains("lease")
                    {
                        return Err(error);
                    }
                    let message = error
                        .detail
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| error.to_string())
                        .chars()
                        .take(2000)
                        .collect::<String>();
                    let runtime = target
                        .as_ref()
                        .map(|t| t["metadata"]["runtime_trajectory"].clone())
                        .unwrap_or(json!({}));
                    let tools = target.as_ref().map(|t| observed_trajectory(t).0);
                    let unknown = runtime["exit_reason"] == "side_effect_unknown"
                        || message.to_uppercase().contains("SIDE_EFFECT_UNKNOWN")
                        || message.contains("DISPATCH_RECONCILIATION_REQUIRED")
                        || tools.as_ref().is_some_and(|tools| {
                            tools.iter().any(|v| v["status"] == "side_effect_unknown")
                        });
                    errors.push(format!(
                        "{}#{}: {message}",
                        text(case, "case_id"),
                        case["trial_index"]
                    ));
                    let trial = json!({"case_id":case["case_id"],"trial_index":case["trial_index"].as_i64().unwrap_or(1),"critical":case["metadata"]["critical"]==true,"execution_succeeded":succeeded,"trace_id":trace_id,"execution_outcome":if unknown{"side_effect_unknown"}else if succeeded{"judge_failed"}else{"execution_failed"},"tool_trajectory":tools,"exit_reason":runtime["exit_reason"],"side_effect_unknown":unknown,"trajectory_pass":null,"stateful_expected":case["expected_trajectory"]["stateful"].is_object(),"stateful_pass":null,"behavior_pass":null,"error":message});
                    runner
                        .client
                        .persist_case(
                            job,
                            case_id,
                            "failed",
                            trace_id.as_deref(),
                            Some(&trial),
                            Some(&message),
                            None,
                            None,
                        )
                        .await?;
                    trials.push(trial);
                }
            }
        }
        let (summary, metrics, status) = aggregate_live_trials(
            &trials,
            cases.len(),
            written,
            config["candidate_type"] == "agent_version",
        );
        let error = if status == "succeeded" {
            None
        } else {
            let joined = errors.join("; ").chars().take(4000).collect::<String>();
            Some(if joined.is_empty() {
                format!(
                    "{} case(s) have no valid quality result",
                    summary["excluded_case_count"]
                )
            } else {
                joined
            })
        };
        self.update_run(
            job,
            run,
            &status,
            Some(&summary),
            Some(&metrics),
            error.as_deref(),
            false,
            true,
        )
        .await?;
        Ok(EvaluatorRunResult::terminal(
            run, &status, summary, metrics, error, written,
        ))
    }
}
pub fn aggregate_live_trials(
    trials: &[Value],
    total: usize,
    written: usize,
    version: bool,
) -> (Value, Value, String) {
    let executed = trials
        .iter()
        .filter(|v| v["execution_succeeded"] == true)
        .collect::<Vec<_>>();
    let mut groups: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for trial in trials {
        groups
            .entry(text(trial, "case_id").into())
            .or_default()
            .push(trial);
    }
    let valid = groups
        .iter()
        .filter(|(_, items)| {
            !items.is_empty()
                && items.iter().all(|v| {
                    v["execution_succeeded"] == true
                        && v["behavior_pass"].is_boolean()
                        && v["aggregate_score"].as_f64().is_some_and(f64::is_finite)
                })
        })
        .collect::<BTreeMap<_, _>>();
    let excluded = groups
        .keys()
        .filter(|key| !valid.contains_key(key))
        .cloned()
        .collect::<Vec<_>>();
    let score_sum = valid
        .values()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v["aggregate_score"].as_f64())
                .sum::<f64>()
                / items.len() as f64
        })
        .sum::<f64>();
    let passed = valid
        .values()
        .filter(|items| items.iter().all(|v| v["behavior_pass"] == true))
        .count();
    let flaky = valid
        .values()
        .filter(|items| {
            items.iter().any(|v| v["behavior_pass"] == true)
                && items.iter().any(|v| v["behavior_pass"] == false)
        })
        .count();
    let critical = valid
        .values()
        .filter(|items| items.iter().any(|v| v["critical"] == true))
        .collect::<Vec<_>>();
    let critical_pass = critical
        .iter()
        .filter(|items| items.iter().all(|v| v["behavior_pass"] == true))
        .count();
    let trajectory_failed = valid
        .values()
        .filter(|items| !items.iter().all(|v| v["trajectory_pass"] == true))
        .count();
    let stateful = valid
        .values()
        .filter(|items| items.iter().any(|v| v["stateful_expected"] == true))
        .collect::<Vec<_>>();
    let stateful_failed = stateful
        .iter()
        .filter(|items| !items.iter().all(|v| v["stateful_pass"] == true))
        .count();
    let mut blockers = json!({});
    if !version {
        for key in HARD_BLOCKERS {
            blockers[key] = json!(
                valid
                    .iter()
                    .find(|(id, _)| id.as_str() == key)
                    .is_some_and(|(_, items)| items.iter().all(|v| v["behavior_pass"] == true))
            );
        }
    }
    let blockers_passed = if version {
        None
    } else {
        Some(HARD_BLOCKERS.iter().all(|k| blockers[*k] == true))
    };
    let unknown = trials
        .iter()
        .filter(|v| v["side_effect_unknown"] == true)
        .count();
    let fingerprints = trials
        .iter()
        .filter_map(|v| {
            v.get("fingerprint")
                .filter(|v| v.is_object() && !object(v).is_empty())
        })
        .collect::<Vec<_>>();
    let complete = !trials.is_empty() && fingerprints.len() == trials.len();
    let unique = fingerprints
        .iter()
        .map(|v| crate::migration_content::python_json(v, false, true))
        .collect::<BTreeSet<_>>();
    let average = |key: &str| {
        if executed.is_empty() || executed.iter().any(|v| !v[key].is_number()) {
            None
        } else {
            Some(round(
                executed.iter().filter_map(|v| v[key].as_f64()).sum::<f64>()
                    / executed.len() as f64,
                4,
            ))
        }
    };
    let mut latencies =
        if !executed.is_empty() && executed.iter().all(|v| v["latency_ms"].is_number()) {
            executed
                .iter()
                .filter_map(|v| v["latency_ms"].as_f64())
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
    latencies.sort_by(f64::total_cmp);
    let p50 = if latencies.is_empty() {
        None
    } else {
        let mid = latencies.len() / 2;
        Some(round(
            if latencies.len() % 2 == 1 {
                latencies[mid]
            } else {
                (latencies[mid - 1] + latencies[mid]) / 2.0
            },
            2,
        ))
    };
    let p95 = if latencies.is_empty() {
        None
    } else {
        Some(round(
            latencies[((latencies.len() as f64 * 0.95).ceil() as usize).saturating_sub(1)],
            2,
        ))
    };
    let rate = |passed: usize, total: usize| {
        if total == 0 {
            None
        } else {
            Some(passed as f64 / total as f64)
        }
    };
    let overall = if valid.is_empty() {
        None
    } else {
        Some(score_sum / valid.len() as f64)
    };
    let summary = json!({"schema_version":"eval-gate-metrics/v2","score_sum":score_sum,"overall_score":overall,"average_score":overall,"quality_score":overall,"behavior_pass_rate":rate(passed,valid.len()),"pass_rate":rate(passed,valid.len()),"trajectory_case_count":valid.len(),"trajectory_failed_count":trajectory_failed,"trajectory_pass_rate":rate(valid.len()-trajectory_failed,valid.len()),"critical_pass_rate":rate(critical_pass,critical.len()),"critical_case_count":critical.len(),"critical_failed_count":critical.len()-critical_pass,"flaky_rate":rate(flaky,valid.len()).map(|v|round(v,4)),"case_count":valid.len(),"expected_case_count":groups.len(),"excluded_case_count":excluded.len(),"excluded_case_ids":excluded,"failed_case_count":valid.len()-passed,"stateful_case_count":stateful.len(),"stateful_failed_count":stateful_failed,"stateful_pass_rate":rate(stateful.len()-stateful_failed,stateful.len()),"trial_count":trials.len()});
    let performance = trials
        .iter()
        .all(|v| v.get("explicit_performance_pass").is_none_or(|v| v == true));
    let typed_failure = version
        && (unknown > 0
            || (!critical.is_empty() && critical_pass != critical.len())
            || !performance);
    let unavailable = !excluded.is_empty() || valid.is_empty();
    let gate_status = if unavailable {
        "unavailable"
    } else if typed_failure {
        "fail"
    } else if version {
        "warning"
    } else if !critical.is_empty()
        && critical_pass == critical.len()
        && blockers_passed == Some(true)
        && trials.len() == executed.len()
        && complete
        && performance
    {
        "pass"
    } else {
        "fail"
    };
    let metrics = json!({"attempted_trials":trials.len(),"completed_trials":executed.len(),"failed_trials":trials.iter().filter(|v|!v["behavior_pass"].is_boolean()).count(),"judge_failed_trials":trials.iter().filter(|v|v["execution_outcome"]=="judge_failed").count(),"execution_failed_trials":trials.iter().filter(|v|v["execution_outcome"]=="execution_failed").count(),"unknown_side_effect_trial_count":unknown,"total_trials":total,"unscored_trials":trials.iter().filter(|v|!v["behavior_pass"].is_boolean()).count(),"execution_error_rate":if trials.is_empty(){1.0}else{round((trials.len()-executed.len())as f64/trials.len()as f64,4)},"behavior_failure_rate":rate(valid.len()-passed,valid.len()).map(|v|round(v,4)),"latency_p50_ms":p50,"latency_p95_ms":p95,"input_tokens_per_task":average("input_tokens"),"output_tokens_per_task":average("output_tokens"),"total_tokens_per_task":average("total_tokens"),"cost_per_task_cents":average("cost_cents"),"critical_pass_rate":summary["critical_pass_rate"],"critical_case_count":summary["critical_case_count"],"critical_failed_count":summary["critical_failed_count"],"required_hard_blockers":if version{json!([])}else{json!(HARD_BLOCKERS)},"hard_blocker_results":blockers,"hard_blockers_passed":blockers_passed,"mixed_runtime":unique.len()>1,"fingerprint_complete":complete,"actual_fingerprint":if complete&&unique.len()==1{fingerprints[0].clone()}else{json!({})},"scores_written":written,"quality_evidence":{"scope":"fixed_dataset_cases","statistical_significance":"not_assessed","scored_case_count":valid.len(),"excluded_case_count":excluded.len()},"gate":{"profile":if version{"agent_version_task_suite"}else{"builtin_assistant_hard_blockers"},"reason":if unavailable{json!("incomplete_execution")}else if typed_failure{json!("known_hard_failure")}else if version{json!("fixed_sample_only")}else{Value::Null},"status":gate_status}});
    let status = if trials.len() == total && excluded.is_empty() {
        "succeeded"
    } else {
        "failed"
    };
    (summary, metrics, status.into())
}
