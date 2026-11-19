use crate::{nonempty, string_list, validation};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
pub const TRACKS: [&str; 2] = ["retrieval_only", "answer_aware"];
pub fn validate_cases(cases: &[Value]) -> Value {
    let mut errors = vec![];
    let mut seen = BTreeSet::new();
    for (index, case) in cases.iter().enumerate() {
        let id = case["case_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("line-{}", index + 1));
        let mut e: Vec<String> = vec![];
        if let Some(m) = case.as_object() {
            let mut unknown = m
                .keys()
                .filter(|k| {
                    ![
                        "case_id",
                        "track",
                        "query",
                        "relevance",
                        "reference_answer",
                        "metadata",
                    ]
                    .contains(&k.as_str())
                })
                .cloned()
                .collect::<Vec<_>>();
            unknown.sort();
            if !unknown.is_empty() {
                e.push(format!("unsupported fields {}", unknown.join(", ")));
            }
        }
        if !seen.insert(id.clone()) {
            e.push("duplicate case_id".into());
        }
        if !case["case_id"].is_string() || id.trim().is_empty() {
            e.push("case_id must be a non-empty string".into());
        }
        if !TRACKS.contains(&case["track"].as_str().unwrap_or("")) {
            e.push("track must be retrieval_only or answer_aware".into());
        }
        if !nonempty(&case["query"]) {
            e.push("query must be a non-empty string".into());
        }
        if let Some(r) = case["relevance"].as_object().filter(|r| !r.is_empty()) {
            let mut positive = 0;
            for (key, grade) in r {
                if key.trim().is_empty() {
                    e.push("relevance IDs must be non-empty strings".into());
                    continue;
                }
                match grade.as_f64() {
                    None => e.push("relevance grades must be numeric".into()),
                    Some(g) if !g.is_finite() || g < 0. => {
                        e.push("relevance grades must be finite and non-negative".into())
                    }
                    Some(g) if g > 0. => positive += 1,
                    _ => (),
                }
            }
            if positive == 0 {
                e.push("relevance must contain at least one positive grade".into());
            }
        } else {
            e.push("relevance must be a non-empty object".into());
        }
        if case["track"] == "answer_aware" && !nonempty(&case["reference_answer"]) {
            e.push("answer_aware cases require reference_answer".into());
        }
        if !case["metadata"].is_null() && !case["metadata"].is_object() {
            e.push("metadata must be an object".into());
        } else if case["metadata"]
            .get("critical")
            .is_some_and(|v| !v.is_boolean())
        {
            e.push("metadata.critical must be boolean".into());
        }
        if !e.is_empty() {
            errors.push(json!({"case_id":id,"errors":e}));
        }
    }
    validation(cases.len(), errors)
}
pub fn validate_observations(cases: &[Value], observations: &BTreeMap<String, Value>) -> Value {
    let by_id = cases
        .iter()
        .map(|c| (c["case_id"].as_str().unwrap_or("").to_owned(), c))
        .collect::<BTreeMap<_, _>>();
    let mut errors = vec![];
    for id in by_id.keys().filter(|id| !observations.contains_key(*id)) {
        errors.push(json!({"case_id":id,"errors":["missing RAG observation"]}));
    }
    for id in observations.keys().filter(|id| !by_id.contains_key(*id)) {
        errors.push(json!({"case_id":id,"errors":["observation has no RAG expectation"]}));
    }
    let mut joined = 0;
    for (id, case) in &by_id {
        let Some(replay) = observations.get(id) else {
            continue;
        };
        joined += 1;
        let mut e = vec![];
        if replay["status"] != "succeeded" {
            e.push("RAG replay status must be succeeded");
        }
        if !string_list(&replay["ranked_segment_ids"], false) {
            e.push("ranked_segment_ids must be a non-empty string list");
        }
        if replay.get("answer_metrics").is_some() {
            e.push("answer_metrics must use the explicit judge observation channel");
        }
        if case["track"] == "retrieval_only" {
            if !replay["answer"].is_null() && replay["answer"] != "" {
                e.push("retrieval_only observation must not contain an answer");
            }
            if replay["answer_source"] != "retrieval_only" {
                e.push("retrieval_only observation requires answer_source=retrieval_only");
            }
        } else {
            if !nonempty(&replay["answer"]) {
                e.push("answer_aware observation requires a generated answer");
            }
            if replay["answer_source"] != "generated" {
                e.push("answer_aware observation requires answer_source=generated");
            }
        }
        if !e.is_empty() {
            errors.push(json!({"case_id":id,"errors":e}));
        }
    }
    json!({"valid":errors.is_empty(),"case_count":by_id.len(),"observation_count":observations.len(),"joined_count":joined,"errors":errors})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_boolean_grades_and_wrong_answer_channel() {
        let case =
            json!({"case_id":"x","track":"retrieval_only","query":"q","relevance":{"a":true}});
        assert_eq!(validate_cases(&[case.clone()])["valid"], false);
        let obs = BTreeMap::from([(
            "x".into(),
            json!({"status":"succeeded","ranked_segment_ids":["a"],"answer_source":"generated","answer_metrics":{}}),
        )]);
        assert_eq!(validate_observations(&[case], &obs)["valid"], false);
    }
}
#[derive(Clone)]
pub struct GateOptions {
    pub ks: Vec<usize>,
    pub min_total: usize,
    pub min_track: usize,
    pub min_answer: usize,
    pub bootstrap_samples: usize,
    pub required_tracks: Vec<String>,
    pub retrieval_thresholds: BTreeMap<String, f64>,
}
impl Default for GateOptions {
    fn default() -> Self {
        Self {
            ks: vec![1, 3, 5],
            min_total: 8,
            min_track: 4,
            min_answer: 4,
            bootstrap_samples: 2000,
            required_tracks: TRACKS.iter().map(|s| s.to_string()).collect(),
            retrieval_thresholds: [("recall_at_k", 0.8), ("mrr", 0.8), ("ndcg_at_k", 0.8)]
                .into_iter()
                .map(|(k, v)| (k.into(), v))
                .collect(),
        }
    }
}
fn retrieval_metrics(
    cases: &[Value],
    observations: &BTreeMap<String, Value>,
    options: &GateOptions,
) -> crate::Result<Value> {
    let mut cases = cases.iter().collect::<Vec<_>>();
    cases.sort_by_key(|c| c["case_id"].as_str().unwrap_or(""));
    let judgements = cases
        .iter()
        .map(|case| crate::metrics::QueryRetrievalJudgement {
            query_id: case["case_id"].as_str().unwrap().into(),
            retrieved: observations[case["case_id"].as_str().unwrap()]["ranked_segment_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().into())
                .collect(),
            relevance: case["relevance"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_f64().unwrap()))
                .collect(),
        })
        .collect::<Vec<_>>();
    let mut payload = crate::metrics::evaluate_retrieval(&judgements, &options.ks)?;
    let k = *options.ks.iter().max().unwrap();
    payload["primary_k"] = json!(k);
    payload["confidence_intervals_95"] = json!({});
    for metric in ["recall_at_k", "mrr", "ndcg_at_k"] {
        let values = cases
            .iter()
            .map(|c| {
                payload["per_query"][c["case_id"].as_str().unwrap()]["by_k"][k.to_string()][metric]
                    .as_f64()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        payload["confidence_intervals_95"][metric] = json!(crate::bootstrap::mean_ci(
            &values,
            options.bootstrap_samples
        ));
    }
    Ok(payload)
}
pub fn evaluate(
    cases: &[Value],
    observations: &BTreeMap<String, Value>,
    o: &GateOptions,
) -> crate::Result<Value> {
    if o.ks.is_empty() || o.ks.iter().any(|k| !(1..=100).contains(k)) {
        return Err("k_values must contain integers between 1 and 100".into());
    }
    if o.ks.iter().collect::<BTreeSet<_>>().len() != o.ks.len() {
        return Err("k_values must not contain duplicates".into());
    }
    if [o.min_total, o.min_track, o.min_answer].contains(&0) {
        return Err("minimum sample counts must be positive".into());
    }
    if o.bootstrap_samples < 100 {
        return Err("bootstrap samples must be at least 100".into());
    }
    if o.required_tracks
        .iter()
        .any(|t| !TRACKS.contains(&t.as_str()))
    {
        return Err("required_tracks must be a subset of the RAG tracks".into());
    }
    for (name, v) in &o.retrieval_thresholds {
        if !["recall_at_k", "mrr", "ndcg_at_k"].contains(&name.as_str())
            || !v.is_finite()
            || !(0.0..=1.0).contains(v)
        {
            return Err("invalid retrieval thresholds".into());
        }
    }
    let cv = validate_cases(cases);
    let ov = validate_observations(cases, observations);
    let mut failures: Vec<String> = vec![];
    for v in [&cv, &ov] {
        for row in v["errors"].as_array().unwrap() {
            failures.push(format!(
                "{}: {}",
                row["case_id"].as_str().unwrap_or(""),
                row["errors"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
    }
    let tracks = TRACKS
        .iter()
        .map(|t| {
            (
                t.to_string(),
                cases
                    .iter()
                    .filter(|c| c["track"] == *t)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if cases.len() < o.min_total {
        failures.push(format!(
            "sample_count {} < minimum {}",
            cases.len(),
            o.min_total
        ));
    }
    for track in &o.required_tracks {
        let count = tracks[track].len();
        if count < o.min_track {
            failures.push(format!(
                "{track} sample_count {count} < minimum {}",
                o.min_track
            ));
        }
    }
    let can_score = cv["valid"] == true && ov["valid"] == true && !cases.is_empty();
    let critical_count = cases
        .iter()
        .filter(|c| c["metadata"]["critical"] == true)
        .count();
    let mut retrieval = json!({"status":"not_scored","thresholds":o.retrieval_thresholds,"all":null,"tracks":{},"critical":{"case_count":critical_count,"status":"not_scored","results":[]}});
    if can_score {
        retrieval["all"] = retrieval_metrics(cases, observations, o)?;
        for (track, rows) in &tracks {
            if !rows.is_empty() {
                retrieval["tracks"][track] = retrieval_metrics(rows, observations, o)?;
            }
        }
        let scopes = std::iter::once(("all".to_owned(), retrieval["all"].clone()))
            .chain(
                retrieval["tracks"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            )
            .collect::<Vec<_>>();
        for (scope, payload) in scopes {
            let k = payload["primary_k"].to_string();
            for (metric, threshold) in &o.retrieval_thresholds {
                let point = payload["metrics"][&k][metric].as_f64().unwrap();
                let lower = payload["confidence_intervals_95"][metric][0].as_f64();
                if point < *threshold {
                    failures.push(format!("{scope} {metric} {point:.4} < {threshold:.4}"));
                }
                if lower.is_none_or(|l| l < *threshold) {
                    failures.push(format!(
                        "{scope} {metric} lower_ci {} < {threshold:.4}",
                        lower
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "None".into())
                    ));
                }
            }
        }
        let mut critical = vec![];
        let mut sorted = cases.iter().collect::<Vec<_>>();
        sorted.sort_by_key(|c| c["case_id"].as_str().unwrap());
        let k = retrieval["all"]["primary_k"].to_string();
        for case in sorted {
            if case["metadata"]["critical"] != true {
                continue;
            }
            let id = case["case_id"].as_str().unwrap();
            let values = &retrieval["all"]["per_query"][id]["by_k"][&k];
            let mut errors = vec![];
            if values["hit_rate"] != 1.0 {
                errors.push("required retrieval hit is missing".into());
            }
            let mut metrics = json!({"hit_rate":values["hit_rate"]});
            for (metric, threshold) in &o.retrieval_thresholds {
                let point = values[metric].as_f64().unwrap();
                metrics[metric] = json!(point);
                if point < *threshold {
                    errors.push(format!("{metric} {point:.4} < {threshold:.4}"));
                }
            }
            if !errors.is_empty() {
                failures.push(format!(
                    "critical RAG retrieval case {id} failed: {}",
                    errors.join("; ")
                ));
            }
            critical.push(json!({"case_id":id,"track":case["track"],"passed":errors.is_empty(),"metrics":metrics,"failures":errors}));
        }
        retrieval["critical"] = json!({"case_count":critical.len(),"status":if critical.is_empty(){"not_present"}else if critical.iter().all(|c|c["passed"]==true){"pass"}else{"fail"},"results":critical});
        retrieval["status"] = json!(if failures.is_empty() { "pass" } else { "fail" });
    }
    let answer = &tracks["answer_aware"];
    let mut seen = BTreeSet::new();
    failures.retain(|f| seen.insert(f.clone()));
    Ok(
        json!({"schema_version":"rag-eval-regression-v1","evidence_scope":"recorded_offline_retrieval","sample_count":cases.len(),"track_counts":tracks.iter().map(|(k,v)|(k.clone(),v.len())).collect::<BTreeMap<_,_>>(),"minimum_samples":{"total":o.min_total,"per_track":o.min_track,"answer_judge":o.min_answer},"retrieval":retrieval,"answer_quality":{"status":"not_run","evidence_scope":"external_judge_opt_in_required","eligible_case_count":answer.len(),"metrics":{"faithfulness":{"sample_count":0,"mean":null,"ci_95":null},"response_relevancy":{"sample_count":0,"mean":null,"ci_95":null}},"critical":{"case_count":answer.iter().filter(|c|c["metadata"]["critical"]==true).count(),"status":"not_run","results":[]}},"gate":{"status":if failures.is_empty(){"pass"}else{"fail"},"outcome":if failures.is_empty(){"retrieval_pass_answer_not_run"}else{"fail"},"scope":"offline_retrieval_only","not_run":["answer_quality"],"failures":failures},"evidence_tiers":{"offline_fixture_contract":if can_score{"verified"}else{"invalid"},"runtime_retrieval":"not_run","external_answer_judge":"not_run","real_provider_call":"not_run"}}),
    )
}
