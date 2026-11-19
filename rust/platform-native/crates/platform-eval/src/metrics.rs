//! Duplicate-aware IR metrics ported from the shared retrieval contract.
use crate::Result;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn invalid(message: &str) -> Box<dyn std::error::Error + Send + Sync> {
    message.into()
}
pub struct QueryRetrievalJudgement {
    pub query_id: String,
    pub retrieved: Vec<String>,
    pub relevance: BTreeMap<String, f64>,
}
fn ranked_grades(retrieved: &[String], grades: &BTreeMap<String, f64>) -> Vec<f64> {
    let mut seen = BTreeSet::new();
    retrieved
        .iter()
        .map(|id| {
            if seen.insert(id) {
                grades.get(id).copied().unwrap_or(0.0)
            } else {
                0.0
            }
        })
        .collect()
}
fn dcg(grades: &[f64], k: usize) -> f64 {
    grades
        .iter()
        .take(k)
        .enumerate()
        .filter(|(_, g)| **g > 0.0)
        .map(|(i, g)| g / ((i + 2) as f64).log2())
        .sum()
}
fn metrics(retrieved: &[String], relevance: &BTreeMap<String, f64>, k: Option<usize>) -> Value {
    let all = ranked_grades(retrieved, relevance);
    let cutoff = k.unwrap_or(all.len());
    let top = &all[..all.len().min(cutoff)];
    let relevant = relevance.values().filter(|g| **g > 0.0).count();
    let mut hits = 0usize;
    let mut rr = 0.0;
    let mut precisions = 0.0;
    for (i, grade) in top.iter().enumerate() {
        if *grade > 0.0 {
            hits += 1;
            precisions += hits as f64 / (i + 1) as f64;
            if rr == 0.0 {
                rr = 1.0 / (i + 1) as f64;
            }
        }
    }
    let denominator = k.map_or(relevant, |k| relevant.min(k));
    let mut ideal: Vec<_> = relevance.values().copied().collect();
    ideal.sort_by(|a, b| b.total_cmp(a));
    let ideal_dcg = dcg(&ideal, cutoff);
    json!({"hit_rate":if hits>0{1.0}else{0.0},"precision_at_k":if cutoff>0{hits as f64/cutoff as f64}else{0.0},"recall_at_k":if relevant>0{hits as f64/relevant as f64}else{0.0},"mrr":rr,"ndcg_at_k":if ideal_dcg>0.0{(dcg(&all,cutoff)/ideal_dcg).clamp(0.0,1.0)}else{0.0},"map":if denominator>0{precisions/denominator as f64}else{0.0}})
}
pub fn score_single_query(
    retrieved: &[String],
    relevance: &BTreeMap<String, f64>,
    ks: &[usize],
) -> Result<Value> {
    if relevance.values().any(|g| !g.is_finite() || *g < 0.0) {
        return Err(invalid("relevance grades must be finite and non-negative"));
    }
    let mut by_k = Map::new();
    for k in ks {
        by_k.insert(k.to_string(), metrics(retrieved, relevance, Some(*k)));
    }
    let full = metrics(retrieved, relevance, None);
    Ok(
        json!({"reciprocal_rank":full["mrr"],"average_precision":full["map"],"retrieved_count":retrieved.len(),"unique_retrieved_count":retrieved.iter().collect::<BTreeSet<_>>().len(),"by_k":by_k}),
    )
}
pub fn evaluate_retrieval(judgements: &[QueryRetrievalJudgement], ks: &[usize]) -> Result<Value> {
    if ks.is_empty() || ks.iter().any(|k| !(1..=100).contains(k)) {
        return Err(invalid("k_values must be between 1 and 100"));
    }
    let ks: Vec<_> = ks
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let names = [
        "hit_rate",
        "precision_at_k",
        "recall_at_k",
        "mrr",
        "ndcg_at_k",
        "map",
    ];
    let mut report = json!({"k_values":ks,"metrics":{},"per_query":{}});
    for k in &ks {
        report["metrics"][k.to_string()] = json!({"k":k,"num_queries":judgements.len()});
        for name in names {
            report["metrics"][k.to_string()][name] = json!(0.0)
        }
    }
    for j in judgements {
        if report["per_query"].get(&j.query_id).is_some() {
            return Err(invalid("duplicate query_id"));
        }
        let detail = score_single_query(&j.retrieved, &j.relevance, &ks)?;
        for k in &ks {
            for name in names {
                let value = report["metrics"][k.to_string()][name]
                    .as_f64()
                    .unwrap_or(0.0)
                    + detail["by_k"][k.to_string()][name].as_f64().unwrap_or(0.0);
                report["metrics"][k.to_string()][name] = json!(value)
            }
        }
        report["per_query"][&j.query_id] = detail;
    }
    for k in &ks {
        for name in names {
            let sum = report["metrics"][k.to_string()][name]
                .as_f64()
                .unwrap_or(0.0);
            report["metrics"][k.to_string()][name] = json!(sum / judgements.len().max(1) as f64);
        }
    }
    Ok(report)
}
