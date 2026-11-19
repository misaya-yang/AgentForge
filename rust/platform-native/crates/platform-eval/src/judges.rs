use crate::{
    Result,
    bootstrap::mean_ci,
    canonical::{digest, sha256},
    nonempty,
    rag::GateOptions,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const FIELDS: [&str; 5] = [
    "provider",
    "model",
    "judge_run_id",
    "rubric_version",
    "judge_prompt_sha256",
];
const DOMAIN: &str = "rag-recorded-judge-rows/v1";
const METRICS: [&str; 2] = ["faithfulness", "response_relevancy"];
pub struct Policy {
    payload: Value,
    source: String,
    hash: String,
}
fn sha(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}
fn exact_keys(v: &Value, keys: &[&str]) -> bool {
    v.as_object()
        .is_some_and(|m| m.len() == keys.len() && m.keys().all(|k| keys.contains(&k.as_str())))
}
pub fn resolve_policy(path: &str) -> Result<Policy> {
    let raw = std::fs::read(path)?;
    let hash = sha256(&raw);
    let trusted = std::env::var("RAG_JUDGE_POLICY_SHA256").unwrap_or_default();
    if !sha(&json!(trusted)) {
        return Err("RAG_JUDGE_POLICY_SHA256 must pin a lowercase policy SHA-256".into());
    }
    if hash != trusted {
        return Err("judge policy manifest does not match the trusted environment pin".into());
    }
    let mut p: Value = serde_json::from_slice(&raw)
        .map_err(|_| "judge policy manifest must be valid UTF-8 JSON")?;
    if !exact_keys(
        &p,
        &[
            "schema_version",
            "policy_id",
            "revision",
            "artifact",
            "cohort",
        ],
    ) {
        return Err("judge policy manifest has invalid fields".into());
    }
    if p["schema_version"] != "rag-judge-policy/v1" {
        return Err("judge policy manifest has an unsupported schema".into());
    }
    for key in ["policy_id", "revision"] {
        if !nonempty(&p[key]) {
            return Err(format!("judge policy {key} must be a non-empty string").into());
        }
        p[key] = json!(p[key].as_str().unwrap().trim());
    }
    if !exact_keys(&p["artifact"], &["hash_domain", "sha256"]) {
        return Err("judge policy artifact binding has invalid fields".into());
    }
    if p["artifact"]["hash_domain"] != DOMAIN {
        return Err("judge policy artifact hash domain is unsupported".into());
    }
    if !sha(&p["artifact"]["sha256"]) {
        return Err("judge policy artifact digest must be lowercase SHA-256".into());
    }
    if !exact_keys(&p["cohort"], &FIELDS) || FIELDS.iter().any(|k| !nonempty(&p["cohort"][k])) {
        return Err("invalid expected judge cohort".into());
    }
    if !sha(&p["cohort"]["judge_prompt_sha256"]) {
        return Err("expected judge cohort prompt hash must be lowercase SHA-256".into());
    }
    Ok(Policy {
        payload: p,
        source: path.into(),
        hash,
    })
}
pub fn apply(
    report: &mut Value,
    cases: &[Value],
    observations: &BTreeMap<String, Value>,
    rows: &[Value],
    policy: &Policy,
    o: &GateOptions,
    faithfulness: f64,
    relevancy: f64,
) -> Result<()> {
    let thresholds = BTreeMap::from([
        ("faithfulness", faithfulness),
        ("response_relevancy", relevancy),
    ]);
    if thresholds
        .values()
        .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err("invalid answer thresholds".into());
    }
    let expected = cases
        .iter()
        .filter(|c| c["track"] == "answer_aware")
        .map(|c| (c["case_id"].as_str().unwrap().to_string(), c))
        .collect::<BTreeMap<_, _>>();
    let mut failures: Vec<String> = vec![];
    let mut by_case = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let id = row["case_id"].as_str().unwrap_or("");
        if id.is_empty() || by_case.contains_key(id) {
            failures.push(format!(
                "judge row {} has missing or duplicate case_id",
                index + 1
            ));
            continue;
        }
        if !expected.contains_key(id) {
            failures.push(format!("judge row '{id}' is not an answer_aware case"));
        }
        for field in FIELDS
            .into_iter()
            .chain(["expectation_sha256", "observation_sha256"])
        {
            if !nonempty(&row[field]) {
                failures.push(format!("judge row '{id}' requires {field} provenance"));
            }
        }
        if !sha(&row["judge_prompt_sha256"]) {
            failures.push(format!(
                "judge row '{id}' requires lowercase judge_prompt_sha256"
            ));
        }
        if let Some(case) = expected.get(id) {
            if row["expectation_sha256"] != digest(case) {
                failures.push(format!("judge row '{id}' expectation hash mismatch"));
            }
            if let Some(obs) = observations.get(id) {
                if row["observation_sha256"] != digest(obs) {
                    failures.push(format!("judge row '{id}' observation hash mismatch"));
                }
            } else {
                failures.push(format!("judge row '{id}' has no observation to bind"));
            }
        }
        let mut normalized = json!({});
        if let Some(metrics) = row["metrics"].as_object() {
            for (raw_name, score) in metrics {
                let name = if raw_name == "answer_relevancy" {
                    "response_relevancy"
                } else {
                    raw_name
                };
                if !METRICS.contains(&name) {
                    failures.push(format!(
                        "judge row '{id}' has unsupported metric '{raw_name}'"
                    ));
                    continue;
                }
                if !score
                    .as_f64()
                    .is_some_and(|s| s.is_finite() && (0.0..=1.0).contains(&s))
                {
                    failures.push(format!("judge row '{id}' has invalid {name} score"));
                    continue;
                }
                if normalized.get(name).is_some() {
                    failures.push(format!("judge row '{id}' duplicates metric {name}"));
                    continue;
                }
                normalized[name] = score.clone();
            }
            let missing = METRICS
                .iter()
                .filter(|m| normalized.get(**m).is_none())
                .copied()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                failures.push(format!(
                    "judge row '{id}' missing metrics {}",
                    missing.join(", ")
                ));
            }
        } else {
            failures.push(format!("judge row '{id}' requires metrics"));
        }
        let mut row = row.clone();
        row["metrics"] = normalized;
        by_case.insert(id.to_string(), row);
    }
    let missing = expected
        .keys()
        .filter(|id| !by_case.contains_key(*id))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        failures.push(format!("missing answer judge rows: {}", missing.join(", ")));
    }
    if expected.len() < o.min_answer {
        failures.push(format!(
            "answer judge sample_count {} < minimum {}",
            expected.len(),
            o.min_answer
        ));
    }
    let cohorts = by_case
        .iter()
        .filter(|(id, row)| expected.contains_key(*id) && FIELDS.iter().all(|f| row[f].is_string()))
        .map(|(_, row)| {
            FIELDS
                .iter()
                .map(|f| row[f].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    let mut cohort = Value::Null;
    let expected_cohort = &policy.payload["cohort"];
    if cohorts.len() != 1 {
        failures
            .push("answer judge rows must use one provider/model/run/rubric/prompt cohort".into());
    } else {
        cohort = json!({});
        for (field, value) in FIELDS.iter().zip(cohorts.iter().next().unwrap()) {
            cohort[*field] = json!(value);
        }
        if &cohort != expected_cohort {
            failures.push("answer judge cohort does not match the pinned expected cohort".into());
        }
        cohort["cohort_sha256"] = json!(digest(&cohort));
    }
    let mut metrics = json!({});
    for metric in METRICS {
        let values = expected
            .keys()
            .filter_map(|id| by_case.get(id).and_then(|r| r["metrics"][metric].as_f64()))
            .collect::<Vec<_>>();
        let interval = mean_ci(&values, o.bootstrap_samples);
        let mean = if values.is_empty() {
            None
        } else {
            Some(
                (values.iter().sum::<f64>() / values.len() as f64 * 10000.).round_ties_even()
                    / 10000.,
            )
        };
        metrics[metric] = json!({"sample_count":values.len(),"mean":mean,"ci_95":interval});
        if interval.is_none_or(|ci| ci[0] < thresholds[metric]) {
            failures.push(format!(
                "{metric} lower_ci {} < {:.4}",
                interval
                    .map(|ci| ci[0].to_string())
                    .unwrap_or_else(|| "None".into()),
                thresholds[metric]
            ));
        }
    }
    let mut critical = vec![];
    for (id, case) in &expected {
        if case["metadata"]["critical"] != true {
            continue;
        }
        let row = by_case.get(id);
        let mut errors: Vec<String> = vec![];
        if let Some(row) = row {
            if row["expectation_sha256"] != digest(case) {
                errors.push("expectation provenance hash mismatch".into());
            }
            if observations
                .get(id)
                .is_none_or(|obs| row["observation_sha256"] != digest(obs))
            {
                errors.push("observation provenance hash mismatch".into());
            }
            if FIELDS.iter().any(|f| row[f] != expected_cohort[f]) {
                errors.push("judge cohort does not match policy".into());
            }
        } else {
            errors.push("judge row is missing".into());
        }
        let mut cm = json!({});
        for (metric, threshold) in &thresholds {
            let score = row.and_then(|r| r["metrics"][metric].as_f64());
            cm[*metric] = json!(score);
            match score {
                None => errors.push(format!("{metric} is missing")),
                Some(score) if score < *threshold => {
                    errors.push(format!("{metric} {score:.4} < {threshold:.4}"))
                }
                _ => (),
            }
        }
        if !errors.is_empty() {
            failures.push(format!(
                "critical RAG answer case {id} failed: {}",
                errors.join("; ")
            ));
        }
        critical
            .push(json!({"case_id":id,"passed":errors.is_empty(),"metrics":cm,"failures":errors}));
    }
    let observed = digest(&json!({"hash_domain":DOMAIN,"rows":rows}));
    let matched = policy.payload["artifact"]["sha256"] == observed;
    if !matched {
        failures.insert(
            0,
            "recorded answer judge artifact does not match the policy-pinned SHA-256".into(),
        );
    }
    let mut ec = expected_cohort.clone();
    ec["cohort_sha256"] = json!(digest(expected_cohort));
    let provenance = by_case
        .iter()
        .map(|(id, row)| {
            let mut v = json!({"case_id":id});
            for f in FIELDS
                .into_iter()
                .chain(["expectation_sha256", "observation_sha256"])
            {
                v[f] = row[f].clone();
            }
            v
        })
        .collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    failures.retain(|f| seen.insert(f.clone()));
    report["answer_quality"] = json!({"status":if failures.is_empty(){"pass"}else{"fail"},"evidence_scope":"recorded_external_judge_opt_in","eligible_case_count":expected.len(),"expected_cohort":ec,"cohort":cohort,"thresholds":thresholds,"metrics":metrics,"critical":{"case_count":critical.len(),"status":if critical.is_empty(){"not_present"}else if critical.iter().all(|c|c["passed"]==true){"pass"}else{"fail"},"results":critical},"provenance":provenance,"failures":failures,"artifact_binding":{"algorithm":"sha256","hash_domain":DOMAIN,"expected_sha256":policy.payload["artifact"]["sha256"],"observed_sha256":observed,"matched":matched},"policy":{"schema_version":"rag-judge-policy/v1","policy_id":policy.payload["policy_id"],"revision":policy.payload["revision"],"manifest_sha256":policy.hash,"source":policy.source,"resolver":"environment:RAG_JUDGE_POLICY_SHA256"}});
    let all = report["gate"]["failures"].as_array_mut().unwrap();
    for f in failures {
        if !all.contains(&json!(f)) {
            all.push(json!(f));
        }
    }
    let passed = all.is_empty();
    report["gate"]["status"] = json!(if passed { "pass" } else { "fail" });
    report["gate"]["outcome"] = json!(if passed {
        "retrieval_and_recorded_answer_pass"
    } else {
        "fail"
    });
    report["gate"]["scope"] = json!("offline_retrieval_and_recorded_answer");
    report["gate"]["not_run"] = json!([]);
    report["evidence_tiers"]["external_answer_judge"] =
        json!(if matched && report["answer_quality"]["status"] == "pass" {
            "recorded_bound_opt_in"
        } else {
            "not_verified"
        });
    Ok(())
}
