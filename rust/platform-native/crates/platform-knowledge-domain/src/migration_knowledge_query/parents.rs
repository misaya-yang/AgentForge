use super::{
    config::ParentSettings,
    lexical,
    store::QueryStore,
    types::{Candidate, Dataset, QueryError, Result},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn parent_id(candidate: &Candidate) -> String {
    candidate.metadata["parent_segment_id"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or(candidate.metadata["metadata"]["parent_segment_id"].as_str())
        .unwrap_or("")
        .trim()
        .into()
}
fn summary_link(c: &Candidate) -> Option<String> {
    for v in [&c.metadata, &c.metadata["metadata"]] {
        if v["is_summary"].as_bool() == Some(true) {
            return Some(v["original_chunk_id"].as_str().unwrap_or("").trim().into());
        }
    }
    None
}
pub fn merge_summary_siblings(
    candidates: &mut BTreeMap<String, Candidate>,
    prepend: bool,
) -> Value {
    let summaries: Vec<_> = candidates
        .iter()
        .filter_map(|(id, c)| summary_link(c).map(|original| (id.clone(), original)))
        .collect();
    let mut merged = 0;
    let mut unresolved = 0;
    for (id, original) in summaries {
        if id == original || !candidates.contains_key(&original) {
            unresolved += 1;
            continue;
        }
        let Some(summary) = candidates.remove(&id) else {
            continue;
        };
        let Some(target) = candidates.get_mut(&original) else {
            continue;
        };
        let combined = summary.final_score.max(target.final_score);
        if combined > target.final_score {
            target.fusion_score = Some(combined)
        }
        target.final_score = combined;
        if prepend {
            target.metadata["_summary_prefix"] = json!(summary.text)
        }
        target.metadata["_summary_hit"] = json!(true);
        target.metadata["_summary_segment_id"] = json!(id);
        target.sources.extend(summary.sources);
        merged += 1;
    }
    json!({"enabled":true,"merged":merged,"unresolved":unresolved})
}
pub async fn fold_candidates_to_parents(
    candidates: Vec<Candidate>,
    settings: &ParentSettings,
    store: &QueryStore,
    dataset: &Dataset,
) -> Result<(Vec<Candidate>, Value)> {
    let mut groups = BTreeMap::<String, Vec<usize>>::new();
    for (i, c) in candidates.iter().enumerate() {
        let id = parent_id(c);
        if !id.is_empty() {
            groups.entry(id).or_default().push(i)
        }
    }
    let child_hits: usize = groups.values().map(Vec::len).sum();
    let parents = groups.len();
    let mut unresolved = 0;
    let mut reps = BTreeMap::new();
    let mut skip = BTreeSet::new();
    for (parent, mut indices) in groups {
        indices.sort_by(|a, b| {
            candidates[*b]
                .final_score
                .total_cmp(&candidates[*a].final_score)
        });
        let best = indices[0];
        let mut representative = candidates[best].clone();
        let row = store.segment_scoped(dataset, &parent).await.ok().flatten();
        if row.is_none() {
            unresolved += 1
        }
        let parent_text = row.as_ref().map(|r| r.text.trim()).unwrap_or("");
        if let Some(parent_row) = &row {
            if let Some(mut merged) = parent_row.metadata.as_object().cloned() {
                if let Some(child) = representative.metadata.as_object() {
                    for (k, v) in child {
                        merged.entry(k.clone()).or_insert(v.clone());
                    }
                }
                representative.metadata = Value::Object(merged)
            }
            if !parent_row.document_id.is_empty() {
                representative.document_id = parent_row.document_id.clone()
            }
            if !parent_row.content_type.is_empty() {
                representative.metadata["content_type"] = json!(parent_row.content_type)
            }
        }
        representative.metadata["parent_segment_id"] = json!(parent);
        let status = if parent_text.is_empty() {
            "parent_text_missing"
        } else {
            "resolved"
        };
        if !parent_text.is_empty() {
            if settings.return_mode == "parent" {
                representative.segment_id = parent.clone();
                representative.text = parent_text.into();
                representative.metadata["segment_id"] = json!(parent)
            } else {
                representative.metadata["parent_context"] = json!(parent_text)
            }
        }
        let children:Vec<Value>=indices.iter().map(|i|json!({"segment_id":candidates[*i].segment_id,"score":round(candidates[*i].final_score,6)})).collect();
        for i in &indices {
            representative
                .sources
                .extend(candidates[*i].sources.clone());
            if *i != best {
                skip.insert(*i);
            }
        }
        representative.metadata["_parent_child"] = json!({"mode":settings.return_mode,"status":status,"parent_segment_id":parent,"children":children});
        reps.insert(best, representative);
    }
    let mut output = Vec::new();
    for (i, c) in candidates.into_iter().enumerate() {
        if let Some(rep) = reps.remove(&i) {
            output.push(rep)
        } else if !skip.contains(&i) {
            output.push(c)
        }
    }
    Ok((
        output,
        json!({"enabled":true,"mode":settings.return_mode,"child_hits":child_hits,"parents":parents,"collapsed_children":child_hits-parents,"unresolved_parents":unresolved}),
    ))
}
pub fn round(v: f64, precision: i32) -> f64 {
    let scale = 10f64.powi(precision);
    (v * scale).round_ties_even() / scale
}
#[derive(Clone, Debug)]
pub struct StructuralSettings {
    pub boost: f64,
    pub min_affinity: f64,
}
pub fn parse_structural_settings(defaults: &Value) -> Result<Option<StructuralSettings>> {
    let raw = &defaults["structural_routing"];
    if raw.is_null() || raw == &Value::Bool(false) || raw["enabled"].as_bool() == Some(false) {
        return Ok(None);
    }
    if !raw.is_object() && raw != &Value::Bool(true) {
        return Err(QueryError::Validation(
            "structural_routing must be a boolean or object".into(),
        ));
    }
    if raw["mode"]
        .as_str()
        .is_some_and(|s| s.trim().to_lowercase() != "heading_priority")
    {
        return Err(QueryError::Validation(
            "structural_routing.mode must be heading_priority".into(),
        ));
    }
    let mut numbers = [0.15, 0.25];
    for (i, key) in ["boost", "min_affinity"].iter().enumerate() {
        if !raw[*key].is_null() {
            numbers[i] = raw[*key]
                .as_f64()
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .ok_or_else(|| {
                    QueryError::Validation(format!(
                        "structural_routing.{key} must be between 0 and 1"
                    ))
                })?
        }
    }
    Ok(Some(StructuralSettings {
        boost: numbers[0],
        min_affinity: numbers[1],
    }))
}
fn breadcrumb(payload: &Value) -> String {
    let mut parts = Vec::new();
    let nested = &payload["metadata"];
    for v in [
        &payload["section_header"],
        &nested["section_header"],
        &nested["breadcrumb"],
        &nested["heading_path"],
        &nested["heading"],
        &nested["section_title"],
        &payload["breadcrumb"],
        &payload["heading"],
        &payload["section_title"],
    ] {
        if let Some(s) = v.as_str() {
            if !s.trim().is_empty() {
                parts.push(s.trim().to_owned())
            }
        } else if let Some(a) = v.as_array() {
            parts.extend(
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            )
        }
    }
    let mut seen = BTreeSet::new();
    parts.retain(|p| seen.insert(p.clone()));
    parts.join(" > ")
}
pub fn apply_structural_routing(
    candidates: &mut BTreeMap<String, Candidate>,
    query: &str,
    settings: &StructuralSettings,
) -> Value {
    let query_tokens: BTreeSet<_> = lexical::tokenize(query, true, false, true)
        .into_iter()
        .collect();
    let mut count = 0;
    let mut boosted = 0;
    for c in candidates.values_mut() {
        let path = breadcrumb(&c.metadata);
        if path.is_empty() {
            continue;
        }
        count += 1;
        let tokens: BTreeSet<_> = lexical::tokenize(&path, true, false, true)
            .into_iter()
            .collect();
        if query_tokens.is_empty() || tokens.is_empty() {
            continue;
        }
        let affinity =
            query_tokens.intersection(&tokens).count() as f64 / query_tokens.len() as f64;
        if affinity < settings.min_affinity {
            continue;
        }
        c.final_score = (c.final_score + settings.boost * affinity).min(1.0);
        if let Some(score) = c.fusion_score {
            c.fusion_score = Some((score + settings.boost * affinity).min(1.0))
        }
        c.metadata["_structural_route"] =
            json!({"mode":"heading_priority","breadcrumb":path,"affinity":round(affinity,4)});
        boosted += 1;
    }
    json!({"enabled":true,"mode":"heading_priority","candidates_with_breadcrumb":count,"boosted":boosted})
}
