//! Paired frozen-run comparison and baseline eligibility receipts.
use super::{
    common::{CompatError, CompatUser, object, text},
    eval::EvalService,
    eval_gate::apply_gate,
    eval_live::HARD_BLOCKERS,
    eval_operations::retry_available,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn known(v: &Value) -> Option<f64> {
    v.as_f64().filter(|v| v.is_finite())
}
fn round(v: f64) -> f64 {
    (v * 10000.0).round_ties_even() / 10000.0
}
fn average(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        None
    } else {
        Some(round(v.iter().sum::<f64>() / v.len() as f64))
    }
}
fn dedup(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|v| seen.insert(v.clone()))
        .collect()
}
pub fn aggregate_live_case_rows(rows: &[Value]) -> BTreeMap<String, Value> {
    let mut groups: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for row in rows {
        groups
            .entry(text(row, "case_id").into())
            .or_default()
            .push(row);
    }
    let mut result = BTreeMap::new();
    for (case_id, trials) in groups {
        let observed = trials
            .iter()
            .map(|v| &v["observed_metrics"])
            .collect::<Vec<_>>();
        let scores = observed
            .iter()
            .filter_map(|v| known(&v["aggregate_score"]))
            .collect::<Vec<_>>();
        let labels = observed
            .iter()
            .filter_map(|v| v["behavior_pass"].as_bool())
            .collect::<Vec<_>>();
        let executed =
            !trials.is_empty() && observed.iter().all(|v| v["execution_succeeded"] == true);
        let complete = executed
            && trials.iter().all(|v| v["status"] == "succeeded")
            && labels.len() == trials.len();
        let execution_status = if trials.iter().any(|v| {
            v["status"] == "failed" && v["observed_metrics"]["execution_succeeded"] != true
        }) {
            "failed"
        } else if trials.iter().any(|v| v["status"] == "skipped") {
            if observed
                .iter()
                .any(|v| v["execution_outcome"] == "cancelled")
            {
                "cancelled"
            } else {
                "skipped"
            }
        } else if trials.iter().any(|v| v["status"] == "running") {
            "running"
        } else if trials.iter().any(|v| v["status"] == "queued") {
            "queued"
        } else if executed {
            "succeeded"
        } else {
            "failed"
        };
        let representative = observed
            .iter()
            .find(|v| !text(v, "trace_id").is_empty())
            .copied()
            .unwrap_or(observed[0]);
        let row = trials
            .iter()
            .find(|v| text(v, "candidate_trace_id") == text(representative, "trace_id"))
            .copied()
            .unwrap_or(trials[0]);
        let mut metrics = json!({});
        for key in [
            "latency_ms",
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "cost_cents",
        ] {
            let values = observed
                .iter()
                .map(|v| known(&v[key]))
                .collect::<Option<Vec<_>>>();
            metrics[key] = json!(values.as_deref().and_then(average));
        }
        let errors = observed
            .iter()
            .filter_map(|v| v["error"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let contract = observed
            .iter()
            .flat_map(|v| arr(&v["contract_failures"]))
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let stddev = if complete && scores.len() > 1 {
            let mean = scores.iter().sum::<f64>() / scores.len() as f64;
            Some(round(
                (scores.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / scores.len() as f64)
                    .sqrt(),
            ))
        } else if complete && !scores.is_empty() {
            Some(0.0)
        } else {
            None
        };
        result.insert(case_id.clone(),json!({"case_id":case_id,"example_id":trials[0]["example_id"],"input":trials[0]["input"],"expected_output":trials[0]["expected_output"],"assertions":trials[0]["assertions"],"metadata":trials[0]["metadata"],"candidate_trace_id":representative["trace_id"],"trace_ids":observed.iter().filter_map(|v|v["trace_id"].as_str().filter(|s|!s.is_empty())).collect::<Vec<_>>(),"status":if !complete{"unscored"}else if labels.iter().all(|v|*v){"passed"}else{"failed"},"execution_status":execution_status,"retry_available":retry_available(&trials),"behavior_pass":if complete{json!(labels.iter().all(|v|*v))}else{Value::Null},"execution_succeeded":executed,"critical":trials[0]["metadata"]["critical"]==true,"aggregate_score":if complete&&scores.len()==trials.len(){average(&scores)}else{None},"score_stddev":stddev,"flaky":complete&&labels.contains(&true)&&labels.contains(&false),"trial_count":trials.len(),"observed_metrics":metrics,"output_preview":text(representative,"output_preview"),"trace":{"trace_family":if text(row,"trace_family").is_empty(){"assistant"}else{text(row,"trace_family")},"status":row["trace_status"],"model_id":row["model_id"],"provider":row["provider"],"total_latency_ms":representative["latency_ms"],"total_tokens":representative["total_tokens"],"output_preview":text(representative,"output_preview")},"tool_trajectory":representative.get("tool_trajectory").filter(|v|!v.is_null()).cloned().unwrap_or(json!([])),"rag_evidence":representative.get("rag_evidence").filter(|v|!v.is_null()).cloned().unwrap_or(json!([])),"exit_reason":representative["exit_reason"],"contract_failures":contract,"errors":errors}));
    }
    result
}
/// Python Random(42) integer seeding, randrange rejection, and 10,000 paired resamples.
struct PyRandom {
    state: [u32; 624],
    index: usize,
}
impl PyRandom {
    fn seeded() -> Self {
        let mut state = [0u32; 624];
        state[0] = 19650218;
        for i in 1..624 {
            state[i] = 1812433253u32
                .wrapping_mul(state[i - 1] ^ (state[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        let mut i = 1;
        for _ in 0..624 {
            state[i] = (state[i] ^ (state[i - 1] ^ (state[i - 1] >> 30)).wrapping_mul(1664525))
                .wrapping_add(42);
            i += 1;
            if i >= 624 {
                state[0] = state[623];
                i = 1;
            }
        }
        for _ in 0..623 {
            state[i] = (state[i] ^ (state[i - 1] ^ (state[i - 1] >> 30)).wrapping_mul(1566083941))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= 624 {
                state[0] = state[623];
                i = 1;
            }
        }
        state[0] = 0x80000000;
        Self { state, index: 624 }
    }
    fn next(&mut self) -> u32 {
        if self.index >= 624 {
            for i in 0..624 {
                let y = (self.state[i] & 0x80000000) | (self.state[(i + 1) % 624] & 0x7fffffff);
                self.state[i] = self.state[(i + 397) % 624]
                    ^ (y >> 1)
                    ^ if y & 1 != 0 { 0x9908b0df } else { 0 };
            }
            self.index = 0;
        }
        let mut y = self.state[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c5680;
        y ^= (y << 15) & 0xefc60000;
        y ^= y >> 18;
        y
    }
    fn range(&mut self, n: usize) -> usize {
        let bits = usize::BITS - n.leading_zeros();
        loop {
            let value = (self.next() >> (32 - bits.min(32))) as usize;
            if value < n {
                return value;
            }
        }
    }
}
pub fn paired_bootstrap_ci(deltas: &[f64]) -> Option<[f64; 2]> {
    if deltas.is_empty() {
        return None;
    }
    let mut rng = PyRandom::seeded();
    let mut means = Vec::with_capacity(10000);
    for _ in 0..10000 {
        let mut sum = 0.0;
        for _ in deltas {
            sum += deltas[rng.range(deltas.len())];
        }
        means.push(sum / deltas.len() as f64);
    }
    means.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        let position = (means.len() - 1) as f64 * p;
        let lower = position.floor() as usize;
        let upper = position.ceil() as usize;
        round(means[lower] + (means[upper] - means[lower]) * (position - lower as f64))
    };
    Some([percentile(0.025), percentile(0.975)])
}
fn versioned_metrics(value: &Value) -> bool {
    if value["case_count"].as_u64().is_none_or(|n| n == 0) {
        return false;
    }
    let required = [
        "overall_score",
        "score_sum",
        "pass_rate",
        "case_count",
        "failed_case_count",
        "critical_case_count",
        "critical_failed_count",
        "critical_pass_rate",
        "stateful_case_count",
        "stateful_failed_count",
        "stateful_pass_rate",
        "trajectory_case_count",
        "trajectory_failed_count",
        "trajectory_pass_rate",
    ];
    if required.iter().any(|k| value.get(*k).is_none()) {
        return false;
    }
    let zero = json!({"overall_score":0.0,"trajectory_pass_rate":0.0,"critical_pass_rate":0.0,"stateful_pass_rate":0.0,"baseline_tolerance":0.0});
    apply_gate(value, Some(&zero), None, false, false)["status"] == "pass"
}
pub fn compare_runs(
    baseline: &Value,
    candidate: &Value,
    left_rows: &[Value],
    right_rows: &[Value],
) -> Value {
    let left_summary = &baseline["score_summary"];
    let right_summary = &candidate["score_summary"];
    let left_metrics = &baseline["metrics"];
    let right_metrics = &candidate["metrics"];
    let mut reasons = vec![];
    if !versioned_metrics(left_summary) {
        reasons.push("baseline_gate_metrics_unverifiable".into());
    }
    if !versioned_metrics(right_summary) {
        reasons.push("candidate_gate_metrics_unverifiable".into());
    }
    if baseline["run_mode"] != "live_candidate" || candidate["run_mode"] != "live_candidate" {
        reasons.push("legacy_unverified".into());
    }
    if baseline["status"] != "succeeded" || candidate["status"] != "succeeded" {
        reasons.push("run_not_succeeded".into());
    }
    if baseline["experiment_id"] != candidate["experiment_id"] {
        reasons.push("different_experiment".into());
    }
    for (key, reason) in [
        ("dataset_manifest_hash", "dataset_manifest_mismatch"),
        ("evaluator_suite_hash", "evaluator_suite_mismatch"),
    ] {
        if text(baseline, key).is_empty()
            || text(candidate, key).is_empty()
            || baseline[key] != candidate[key]
        {
            reasons.push(reason.into());
        }
    }
    if baseline["repetitions"].as_i64().unwrap_or(1)
        != candidate["repetitions"].as_i64().unwrap_or(1)
    {
        reasons.push("trial_plan_mismatch".into());
    }
    if left_metrics["mixed_runtime"] == true || right_metrics["mixed_runtime"] == true {
        reasons.push("mixed_runtime_fingerprint".into());
    }
    if left_rows
        .iter()
        .chain(right_rows.iter())
        .any(|v| !matches!(text(v, "status"), "succeeded" | "failed" | "skipped"))
    {
        reasons.push("incomplete_run_cases".into());
    }
    let left = aggregate_live_case_rows(left_rows);
    let right = aggregate_live_case_rows(right_rows);
    if left
        .values()
        .chain(right.values())
        .any(|v| v["status"] == "unscored")
    {
        reasons.push("unscored_case_results".into());
    }
    if left.is_empty() || left.keys().ne(right.keys()) {
        reasons.push("case_set_mismatch".into());
    } else if left
        .iter()
        .any(|(id, l)| l["trial_count"] != right[id]["trial_count"])
    {
        reasons.push("trial_plan_mismatch".into());
    }
    let lf = &left_metrics["actual_fingerprint"];
    let rf = &right_metrics["actual_fingerprint"];
    let required = [
        "system_prompt_hash",
        "tool_schema_hash",
        "model_id",
        "provider",
        "runtime_revision",
    ];
    if required
        .iter()
        .any(|key| text(lf, key).is_empty() || text(rf, key).is_empty())
    {
        reasons.push("missing_runtime_fingerprint".into());
    }
    for (run, fingerprint) in [(baseline, lf), (candidate, rf)] {
        let snapshot = &run["target_snapshot"];
        if snapshot["candidate_type"] == "agent_version"
            && [
                "agent_id",
                "agent_version_id",
                "agent_spec_hash",
                "agent_runtime_snapshot_hash",
            ]
            .iter()
            .any(|key| text(snapshot, key).is_empty() || snapshot[*key] != fingerprint[*key])
        {
            reasons.push("agent_version_fingerprint_mismatch".into());
        }
    }
    if baseline["target_snapshot"]["candidate_type"]
        != candidate["target_snapshot"]["candidate_type"]
    {
        reasons.push("candidate_type_mismatch".into());
    }
    let dimensions: [(&str, &[&str]); 9] = [
        (
            "agent_version",
            &[
                "agent_id",
                "agent_version_id",
                "agent_spec_hash",
                "agent_runtime_snapshot_hash",
            ],
        ),
        ("prompt", &["system_prompt_hash"]),
        ("tools", &["tool_schema_hash"]),
        ("model", &["model_id"]),
        ("provider", &["provider"]),
        ("sampling", &["sampling"]),
        ("runtime", &["runtime_revision"]),
        ("rag", &["rag_config_hash", "rag_revision_hash"]),
        ("execution_policy", &["execution_policy"]),
    ];
    let changed = dimensions
        .iter()
        .filter(|(_, keys)| keys.iter().any(|key| lf[*key] != rf[*key]))
        .map(|(name, _)| *name)
        .collect::<Vec<_>>();
    let specs = [
        ("quality_score", "overall_score", "higher"),
        ("behavior_pass_rate", "behavior_pass_rate", "higher"),
        ("critical_pass_rate", "critical_pass_rate", "higher"),
        ("flaky_rate", "flaky_rate", "lower"),
        ("latency_ms", "latency_p50_ms", "lower"),
        ("latency_p95_ms", "latency_p95_ms", "lower"),
        ("input_tokens_per_task", "input_tokens_per_task", "lower"),
        ("output_tokens_per_task", "output_tokens_per_task", "lower"),
        ("total_tokens_per_task", "total_tokens_per_task", "lower"),
        ("cost_per_task_cents", "cost_per_task_cents", "lower"),
        ("execution_error_rate", "execution_error_rate", "lower"),
        ("behavior_failure_rate", "behavior_failure_rate", "lower"),
    ];
    let (mut metric_diffs, mut deltas) = (json!({}), json!({}));
    for (public, key, direction) in specs {
        let l = known(&left_summary[key]).or_else(|| known(&left_metrics[key]));
        let r = known(&right_summary[key]).or_else(|| known(&right_metrics[key]));
        let delta = l.zip(r).map(|(l, r)| round(r - l));
        let status = delta
            .map(|delta| {
                let signed = if direction == "higher" { delta } else { -delta };
                if signed > 0.0 {
                    "improved"
                } else if signed < 0.0 {
                    "regressed"
                } else {
                    "unchanged"
                }
            })
            .unwrap_or("unknown");
        metric_diffs[public] =
            json!({"baseline":l,"candidate":r,"delta":delta,"direction":direction,"status":status});
        deltas[public] = json!(delta);
        if deltas.get(key).is_none() {
            deltas[key] = json!(delta);
        }
    }
    for (key, reason) in [
        ("quality_score", "missing_quality_score"),
        ("execution_error_rate", "missing_execution_error_rate"),
        ("latency_ms", "missing_latency"),
    ] {
        if metric_diffs[key]["baseline"].is_null() || metric_diffs[key]["candidate"].is_null() {
            reasons.push(reason.into());
        }
    }
    let attribution = if !reasons.is_empty() {
        "unverifiable"
    } else if changed.is_empty() {
        "repeatability"
    } else if changed.len() == 1 {
        "isolated_change"
    } else {
        "confounded"
    };
    let (mut case_diffs, mut paired) = (vec![], vec![]);
    for (id, l) in &left {
        let Some(r) = right.get(id) else {
            continue;
        };
        let ls = known(&l["aggregate_score"]);
        let rs = known(&r["aggregate_score"]);
        let delta = ls.zip(rs).map(|(l, r)| round(r - l));
        if let Some(delta) =
            delta.filter(|_| l["status"] != "unscored" && r["status"] != "unscored")
        {
            paired.push(delta);
        }
        let status = if l["status"] == "unscored" || r["status"] == "unscored" {
            "unscored"
        } else if l["behavior_pass"] == true && r["behavior_pass"] == false {
            "regressed"
        } else if l["behavior_pass"] == false && r["behavior_pass"] == true {
            "improved"
        } else if l["behavior_pass"] == false && r["behavior_pass"] == false {
            "same_failure"
        } else if delta.is_some_and(|v| v < -0.02) {
            "regressed"
        } else if delta.is_some_and(|v| v > 0.02) {
            "improved"
        } else if r["flaky"] == true {
            "flaky"
        } else {
            "unchanged"
        };
        let tools = |v: &Value| {
            arr(&v["tool_trajectory"])
                .iter()
                .filter(|v| v.is_object())
                .map(|v| json!({"name":v["name"],"status":v["status"]}))
                .collect::<Vec<_>>()
        };
        let lt = tools(l);
        let rt = tools(r);
        let failure_values = if arr(&r["errors"]).is_empty() {
            &r["contract_failures"]
        } else {
            &r["errors"]
        };
        let error = arr(failure_values)
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("; ");
        case_diffs.push(json!({"case_id":id,"status":status,"baseline_quality_status":l["status"],"candidate_quality_status":r["status"],"baseline_execution_status":l["execution_status"],"candidate_execution_status":r["execution_status"],"critical":r["critical"],"baseline_score":ls,"candidate_score":rs,"score_delta":delta,"baseline_trace_id":l["candidate_trace_id"],"candidate_trace_id":r["candidate_trace_id"],"baseline_output":l["output_preview"],"candidate_output":r["output_preview"],"baseline_metrics":l["observed_metrics"],"candidate_metrics":r["observed_metrics"],"baseline_trial_count":l["trial_count"],"candidate_trial_count":r["trial_count"],"flaky":r["flaky"],"failure_reason":if error.is_empty(){Value::Null}else{json!(error)},"tool_diffs":if lt!=rt{json!([{"type":"trajectory_changed","baseline":lt,"candidate":rt}])}else{json!([])},"rag_diffs":if l["rag_evidence"]!=r["rag_evidence"]{json!([{"type":"evidence_changed","baseline":l["rag_evidence"],"candidate":r["rag_evidence"]}])}else{json!([])}}));
    }
    let rank = |v: &Value| match text(v, "status") {
        "regressed" => 0,
        "unscored" => 1,
        "flaky" => 2,
        "same_failure" => 3,
        "improved" => 4,
        "unchanged" => 5,
        _ => 9,
    };
    case_diffs.sort_by(|a, b| (rank(a), text(a, "case_id")).cmp(&(rank(b), text(b, "case_id"))));
    let ci = if reasons.is_empty() {
        paired_bootstrap_ci(&paired)
    } else {
        None
    };
    let evidence = if !reasons.is_empty() {
        "unverifiable"
    } else if paired.len() >= 10 && ci.is_some() {
        let ci = ci.unwrap();
        if ci[0] > 0.0 {
            "improvement"
        } else if ci[1] < 0.0 {
            "regression"
        } else {
            "inconclusive"
        }
    } else {
        "insufficient_evidence"
    };
    let mut failures = dedup(reasons.clone());
    let mut warnings = vec![];
    let flips = left
        .iter()
        .filter(|(id, l)| {
            right.get(*id).is_some_and(|r| {
                r["critical"] == true && l["behavior_pass"] == true && r["behavior_pass"] == false
            })
        })
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    if !flips.is_empty() {
        failures.push("critical_case_regression".into());
    }
    if right
        .values()
        .any(|r| r["critical"] == true && r["behavior_pass"] == false)
    {
        failures.push("candidate_critical_case_failed".into());
    }
    if known(&deltas["quality_score"]).is_some_and(|v| v < -0.02) {
        failures.push("quality_regression".into());
    }
    if known(&left_metrics["failed_trials"])
        .zip(known(&right_metrics["failed_trials"]))
        .is_some_and(|(l, r)| r > l)
        || known(&deltas["execution_error_rate"]).is_some_and(|v| v > 0.0)
    {
        failures.push("execution_error_regression".into());
    }
    for row in right_rows {
        for assertion in arr(&row["assertions"]) {
            let key = match text(assertion, "type") {
                "latency_ms_lt" => "latency_ms",
                "total_tokens_lt" => "total_tokens",
                "cost_cents_lt" => "cost_cents",
                _ => continue,
            };
            if known(&row["observed_metrics"][key])
                .zip(known(&assertion["value"]))
                .is_none_or(|(actual, max)| actual >= max)
            {
                failures.push("explicit_performance_constraint_failed".into());
                break;
            }
        }
    }
    for key in ["latency_ms", "total_tokens_per_task", "cost_per_task_cents"] {
        if metric_diffs[key]["status"] == "regressed" {
            warnings.push(format!("{key}_increased"));
        }
    }
    if matches!(evidence, "insufficient_evidence" | "inconclusive") {
        warnings.push(evidence.into());
    }
    if case_diffs
        .iter()
        .any(|v| v["status"] == "regressed" && v["critical"] != true)
    {
        warnings.push("noncritical_case_regressions".into());
    }
    if case_diffs.iter().any(|v| v["flaky"] == true) {
        warnings.push("flaky_cases_present".into());
    }
    let typed = baseline["target_snapshot"]["candidate_type"] == "agent_version"
        && candidate["target_snapshot"]["candidate_type"] == "agent_version";
    if typed {
        warnings.push("fixed_sample_only".into());
    }
    let failures = dedup(failures);
    let warnings = dedup(warnings);
    let count = |status: &str| case_diffs.iter().filter(|v| v["status"] == status).count();
    json!({"baseline_run_id":baseline["run_id"],"candidate_run_id":candidate["run_id"],"baseline_summary":left_summary,"candidate_summary":right_summary,"compatibility":{"status":if reasons.is_empty(){"compatible"}else{"incompatible"},"compatible":reasons.is_empty(),"reasons":dedup(reasons)},"changed_dimensions":changed,"attribution":attribution,"deltas":deltas,"metric_diffs":metric_diffs,"regression_summary":{"baseline_status":baseline["status"],"candidate_status":candidate["status"],"regressed_metrics":specs.iter().filter(|(key,_,_)|metric_diffs[*key]["status"]=="regressed").map(|(key,_,_)|*key).collect::<Vec<_>>(),"regressed_case_count":count("regressed"),"improved_case_count":count("improved"),"unchanged_case_count":count("unchanged"),"flaky_case_count":case_diffs.iter().filter(|v|v["flaky"]==true).count(),"same_failure_case_count":count("same_failure"),"unscored_case_count":count("unscored"),"critical_regressions":flips,"attribution_status":attribution},"statistics":{"paired_case_count":paired.len(),"quality_delta_ci_95":ci,"evidence_status":evidence,"wins":paired.iter().filter(|v|**v>0.02).count(),"ties":paired.iter().filter(|v|(-0.02..=0.02).contains(*v)).count(),"losses":paired.iter().filter(|v|**v< -0.02).count(),"seed":42},"gate":{"status":if !failures.is_empty(){"fail"}else if typed{"warning"}else{"pass"},"failures":failures,"warnings":warnings},"case_diffs":case_diffs})
}
impl EvalService {
    pub async fn compare_experiment_runs(
        &self,
        tenant: &str,
        baseline: &str,
        candidate: &str,
    ) -> Result<Option<Value>, CompatError> {
        let Some(left) = self.get(tenant, "experiment-runs", baseline).await? else {
            return Ok(None);
        };
        let Some(right) = self.get(tenant, "experiment-runs", candidate).await? else {
            return Ok(None);
        };
        Ok(Some(compare_runs(
            &left,
            &right,
            &self.list_run_cases(tenant, baseline).await?,
            &self.list_run_cases(tenant, candidate).await?,
        )))
    }
    pub async fn promote_baseline(
        &self,
        user: &CompatUser,
        experiment: &str,
        run_id: &str,
    ) -> Result<Value, CompatError> {
        let info = self
            .get(&user.tenant_id, "experiments", experiment)
            .await?
            .ok_or_else(|| CompatError::new(404, "Experiment run not found"))?;
        let run = self
            .get(&user.tenant_id, "experiment-runs", run_id)
            .await?
            .filter(|v| v["experiment_id"] == experiment)
            .ok_or_else(|| CompatError::new(404, "Experiment run not found"))?;
        let reject = |message: &str| CompatError::new(409, message);
        if run["run_mode"] != "live_candidate" || run["status"] != "succeeded" {
            return Err(reject("Baseline requires a succeeded live_candidate run"));
        }
        let summary = &run["score_summary"];
        if !versioned_metrics(summary) {
            return Err(CompatError::value(
                409,
                json!({"error":"incompatible_gate_metrics_schema","required_schema_version":"eval-gate-metrics/v2"}),
            ));
        }
        let gate = apply_gate(summary, None, None, true, true);
        if gate["status"] != "pass" {
            return Err(CompatError::value(
                409,
                json!({"error":"baseline_quality_gate_failed","failures":gate["failures"]}),
            ));
        }
        if summary["critical_pass_rate"]
            .as_f64()
            .is_none_or(|v| v < 1.0)
        {
            return Err(reject("All critical behavior cases must pass"));
        }
        let metrics = &run["metrics"];
        let required = arr(&metrics["required_hard_blockers"])
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>();
        if metrics["critical_case_count"]
            .as_u64()
            .is_none_or(|v| v < 1)
            || metrics["critical_case_count"] != summary["critical_case_count"]
            || arr(&metrics["required_hard_blockers"]).len() != 2
            || required != HARD_BLOCKERS.into_iter().collect()
            || object(&metrics["hard_blocker_results"]).len() != 2
            || HARD_BLOCKERS
                .iter()
                .any(|key| metrics["hard_blocker_results"][*key] != true)
            || metrics["hard_blockers_passed"] != true
        {
            return Err(reject(
                "Baseline requires all mandatory Assistant safety blockers",
            ));
        }
        for key in ["dataset_manifest_hash", "evaluator_suite_hash"] {
            let hash = text(&run, key);
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(reject(
                    "Baseline requires verified dataset and evaluator provenance",
                ));
            }
        }
        if metrics["mixed_runtime"] != false {
            return Err(reject(
                "Baseline requires an explicit single-runtime fingerprint cohort",
            ));
        }
        let attempted = metrics["attempted_trials"].as_u64();
        let completed = metrics["completed_trials"].as_u64();
        let failed = metrics["failed_trials"].as_u64();
        let total = metrics["total_trials"].as_u64();
        if metrics["gate"]["status"] != "pass"
            || total.is_none_or(|n| n == 0)
            || attempted != total
            || completed != total
            || failed != Some(0)
        {
            return Err(reject("Baseline requires a complete, error-free live run"));
        }
        if [
            "system_prompt_hash",
            "tool_schema_hash",
            "model_id",
            "provider",
            "runtime_revision",
        ]
        .iter()
        .any(|key| text(&metrics["actual_fingerprint"], key).is_empty())
        {
            return Err(reject(
                "Baseline requires a complete verified runtime fingerprint",
            ));
        }
        let previous = info["baseline_run_id"].as_str().filter(|s| !s.is_empty());
        if let Some(previous) = previous.filter(|s| *s != run_id) {
            let comparison = self
                .compare_experiment_runs(&user.tenant_id, previous, run_id)
                .await?;
            if comparison.is_none_or(|v| v["gate"]["status"] != "pass") {
                return Err(reject("Candidate must pass the current baseline gate"));
            }
        }
        self.promote_baseline_storage(user, experiment, run_id, previous)
            .await?
            .ok_or_else(|| reject("Run is not eligible for baseline promotion"))
    }
}

#[cfg(test)]
mod retry_projection_tests {
    use super::super::eval_operations::{retry_effect_run_ids, retry_source_cases};
    use super::*;

    fn judge_failure() -> Value {
        json!({
            "case_id": "case-a", "run_case_id": "attempt-a", "trial_index": 1,
            "status": "failed", "dispatch_state": "completed", "candidate_trace_id": "trace-a",
            "observed_metrics": {
                "case_id": "case-a", "execution_succeeded": true,
                "execution_outcome": "judge_failed", "behavior_pass": null,
                "trace_id": "trace-a", "tool_trajectory": [], "exit_reason": "succeeded",
                "error": "eval_judge_infrastructure_failure"
            }
        })
    }
    fn projected(rows: &[Value]) -> Value {
        aggregate_live_case_rows(rows).remove("case-a").unwrap()
    }

    #[test]
    fn safe_judge_failure_is_retryable_without_becoming_a_quality_or_execution_failure() {
        let case = projected(&[judge_failure()]);
        assert_eq!(case["status"], "unscored");
        assert_eq!(case["execution_status"], "succeeded");
        assert_eq!(case["behavior_pass"], Value::Null);
        assert_eq!(case["aggregate_score"], Value::Null);
        assert_eq!(case["retry_available"], true);
        assert_eq!(
            retry_source_cases(&[judge_failure()], &["case-a".into()])
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn existing_behavior_and_predispatch_failures_remain_retry_candidates() {
        let mut row = judge_failure();
        row["status"] = json!("succeeded");
        row["observed_metrics"]["behavior_pass"] = json!(false);
        row["observed_metrics"]["aggregate_score"] = json!(0.0);
        assert_eq!(projected(&[row])["retry_available"], true);
        let mut row = judge_failure();
        row["dispatch_state"] = json!("not_started");
        row["observed_metrics"]["execution_succeeded"] = json!(false);
        assert_eq!(projected(&[row])["retry_available"], true);
    }

    #[test]
    fn passed_cases_and_pending_or_cancelled_trials_are_not_retry_candidates() {
        let mut row = judge_failure();
        row["status"] = json!("succeeded");
        row["observed_metrics"]["behavior_pass"] = json!(true);
        assert_eq!(projected(&[row])["retry_available"], false);
        for status in ["queued", "running", "skipped"] {
            let mut row = judge_failure();
            row["status"] = json!(status);
            assert_eq!(projected(&[row.clone()])["retry_available"], false);
            assert_eq!(projected(&[judge_failure(), row])["retry_available"], false);
        }
        let mut row = judge_failure();
        row["status"] = json!("skipped");
        row["observed_metrics"]["execution_outcome"] = json!("cancelled");
        assert_eq!(projected(&[row])["retry_available"], false);
    }

    #[test]
    fn unknown_effect_and_unresolved_dispatch_refuse_the_hint() {
        for state in ["dispatching", "reconcile_required", ""] {
            let mut row = judge_failure();
            row["dispatch_state"] = json!(state);
            // Missing dispatch state can still prove completed model-only safety.
            let expected = state.is_empty();
            assert_eq!(projected(&[row])["retry_available"], expected);
        }
        for (key, value) in [
            ("side_effect_unknown", json!(true)),
            ("execution_outcome", json!("side_effect_unknown")),
            ("exit_reason", json!("side_effect_unknown")),
        ] {
            let mut row = judge_failure();
            row["observed_metrics"][key] = value;
            assert_eq!(projected(&[row])["retry_available"], false);
        }
    }

    #[test]
    fn unsafe_judge_failure_cannot_hide_behind_a_safe_first_attempt() {
        let mut unsafe_row = judge_failure();
        unsafe_row["trial_index"] = json!(2);
        unsafe_row["observed_metrics"]["tool_trajectory"] =
            json!([{"name":"send_email","status":"succeeded"}]);
        assert_eq!(projected(&[unsafe_row.clone()])["retry_available"], false);
        assert_eq!(
            projected(&[judge_failure(), unsafe_row.clone()])["retry_available"],
            false
        );
        assert_eq!(
            projected(&[unsafe_row.clone(), judge_failure()])["retry_available"],
            false
        );
        for rows in [
            vec![judge_failure(), unsafe_row.clone()],
            vec![unsafe_row, judge_failure()],
        ] {
            assert!(retry_source_cases(&rows, &["case-a".into()]).is_err());
        }
    }

    #[test]
    fn missing_tool_proof_and_kb_provenance_refuse_the_hint() {
        let mut row = judge_failure();
        row["observed_metrics"]["tool_trajectory"] = Value::Null;
        assert_eq!(projected(&[row])["retry_available"], false);
        let mut row = judge_failure();
        row["metadata"] = json!({"source_kind":"kb_failure"});
        assert_eq!(projected(&[row])["retry_available"], false);
    }

    #[test]
    fn safe_mixed_quality_attempts_remain_one_unscored_case() {
        let mut passed = judge_failure();
        passed["trial_index"] = json!(2);
        passed["status"] = json!("succeeded");
        passed["observed_metrics"]["behavior_pass"] = json!(true);
        passed["observed_metrics"]["aggregate_score"] = json!(1.0);
        let case = projected(&[judge_failure(), passed]);
        assert_eq!(case["trial_count"], 2);
        assert_eq!(case["status"], "unscored");
        assert_eq!(case["aggregate_score"], Value::Null);
        assert_eq!(case["retry_available"], true);
    }

    #[test]
    fn judge_failure_stays_out_of_quality_denominator_and_blocks_gate() {
        let judge = judge_failure()["observed_metrics"].clone();
        let (summary, metrics, status) =
            super::super::eval_live::aggregate_live_trials(&[judge], 1, 0, true);
        assert_eq!(summary["case_count"], 0);
        assert_eq!(summary["expected_case_count"], 1);
        assert_eq!(summary["excluded_case_count"], 1);
        assert_eq!(summary["overall_score"], Value::Null);
        assert_eq!(summary["pass_rate"], Value::Null);
        assert_eq!(metrics["judge_failed_trials"], 1);
        assert_eq!(metrics["execution_failed_trials"], 0);
        assert_eq!(metrics["gate"]["status"], "unavailable");
        assert_eq!(status, "failed");
    }

    #[test]
    fn durable_effect_lookup_covers_every_attempt_and_all_known_turn_identities() {
        let source = judge_failure();
        let mut other = judge_failure();
        other["run_case_id"] = json!("attempt-b");
        other["candidate_trace_id"] = json!("trace-b");
        other["runtime_handle"] = json!({"turn_id":"runtime-b"});
        other["status"] = json!("succeeded");
        other["observed_metrics"]["behavior_pass"] = json!(true);
        let rows = vec![source.clone(), other.clone()];
        assert_eq!(
            retry_source_cases(&rows, &["case-a".into()]).unwrap(),
            vec![source]
        );
        assert_eq!(
            retry_effect_run_ids(&rows),
            vec!["runtime-b", "trace-a", "trace-b"]
        );
        assert_eq!(
            retry_effect_run_ids(&[other.clone(), other]),
            vec!["runtime-b", "trace-a", "trace-b"]
        );
        assert!(retry_effect_run_ids(&[json!({"dispatch_state":"not_started"})]).is_empty());
    }

    #[test]
    fn judge_failure_never_becomes_an_independent_scored_comparison_pair() {
        let mut passed = judge_failure();
        passed["status"] = json!("succeeded");
        passed["observed_metrics"]["behavior_pass"] = json!(true);
        passed["observed_metrics"]["aggregate_score"] = json!(1.0);
        let run = json!({"run_mode":"live_candidate","status":"succeeded","experiment_id":"e"});
        let result = compare_runs(&run, &run, &[passed], &[judge_failure()]);
        assert_eq!(result["case_diffs"][0]["status"], "unscored");
        assert_eq!(
            result["case_diffs"][0]["candidate_quality_status"],
            "unscored"
        );
        assert_eq!(
            result["case_diffs"][0]["candidate_execution_status"],
            "succeeded"
        );
        assert_eq!(result["case_diffs"][0]["candidate_score"], Value::Null);
        assert!(
            result["compatibility"]["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("unscored_case_results"))
        );
        assert_eq!(result["statistics"]["paired_case_count"], 0);
    }
}
