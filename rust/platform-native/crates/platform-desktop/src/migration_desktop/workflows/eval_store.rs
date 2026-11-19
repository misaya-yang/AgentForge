//! Canonical knowledge evaluation case mapping, paging, diff and import semantics.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
pub const SOURCE: &str = "kb-retrieval-workbench";
#[derive(Clone, Default)]
pub struct EvalCase {
    pub id: String,
    pub example_id: Option<String>,
    pub query: String,
    pub relevant: String,
    pub candidates: Value,
}
pub fn relevant_ids(input: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    input
        .split([',', '，', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty() && seen.insert((*s).to_owned()))
        .take(500)
        .map(str::to_owned)
        .collect()
}
pub fn string_ids(value: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && seen.insert((*s).to_owned()))
        .map(str::to_owned)
        .collect()
}
pub fn hash_input(text: &str) -> String {
    let normalized = text
        .split(|c: char| matches!(c as u32, 0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut hash = 0x811c9dc5u32;
    for code in normalized.encode_utf16() {
        hash ^= u32::from(code);
        hash = hash.wrapping_mul(0x01000193);
    }
    format!("{hash:08x}")
}
pub fn project(example: &Value) -> Option<EvalCase> {
    if example["metadata"]["source_kind"] == "kb_failure" {
        return None;
    }
    let query = example["input"]["query"].as_str()?.trim();
    if query.is_empty() {
        return None;
    }
    let example_id = example["example_id"].as_str()?.to_owned();
    let id = example["metadata"]["case_id"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| example_id.clone());
    Some(EvalCase {
        id,
        example_id: Some(example_id),
        query: query.into(),
        relevant: string_ids(&example["expected_output"]["relevant_segment_ids"]).join("\n"),
        candidates: Value::Null,
    })
}
pub fn import_item(dataset: &str, case: &EvalCase) -> Value {
    json!({"case_id":case.id,"split":"regression","input":{"query":case.query},"expected_output":{"relevant_segment_ids":relevant_ids(&case.relevant)},"expected_trajectory":{},"assertions":[],"source_trace_id":null,"metadata":{"source":SOURCE,"kb_dataset_id":dataset}})
}
pub fn signature(cases: &[EvalCase]) -> String {
    serde_json::to_string(
        &cases
            .iter()
            .map(|c| {
                let mut ids = relevant_ids(&c.relevant);
                ids.sort();
                json!({"id":c.id,"query":c.query,"relevantSegmentIds":ids})
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default()
}
pub async fn find_link(api: &DesktopApi, kb: &str) -> Result<Option<Value>, ApiError> {
    let mut offset = 0u64;
    loop {
        let page = api
            .execute(
                ApiRequest::get(&["api", "v1", "eval", "datasets"])
                    .query("limit", 200)
                    .query("offset", offset),
            )
            .await?;
        let rows = page["datasets"].as_array().ok_or(ApiError::Response)?;
        if let Some(found) = rows.iter().find(|d| d["metadata"]["kb_dataset_id"] == kb) {
            return Ok(Some(found.clone()));
        }
        offset = offset
            .checked_add(rows.len() as u64)
            .ok_or(ApiError::Response)?;
        let total = page["total"].as_u64().ok_or(ApiError::Response)?;
        if offset >= total {
            return Ok(None);
        }
        if rows.is_empty() {
            return Err(ApiError::Response);
        }
    }
}
pub async fn resolve_link(api: &DesktopApi, kb: &str) -> Result<Value, ApiError> {
    if let Some(found) = find_link(api, kb).await? {
        return Ok(found);
    }
    api.execute(ApiRequest::mutation(Method::POST,&["api","v1","eval","datasets"],Some(json!({"name":format!("kb-retrieval-eval-{kb}"),"description":format!("Retrieval evaluation cases linked to knowledge dataset {kb}"),"metadata":{"kb_dataset_id":kb,"source":SOURCE}})))).await
}
pub async fn list_examples(api: &DesktopApi, dataset: &str) -> Result<Vec<Value>, ApiError> {
    let mut examples = Vec::new();
    let mut offset = 0u64;
    loop {
        let page = api
            .execute(
                ApiRequest::get(&["api", "v1", "eval", "datasets", dataset, "examples"])
                    .query("limit", 500)
                    .query("offset", offset),
            )
            .await?;
        let rows = page["examples"].as_array().ok_or(ApiError::Response)?;
        offset = offset
            .checked_add(rows.len() as u64)
            .ok_or(ApiError::Response)?;
        examples.extend(rows.clone());
        if offset >= page["total"].as_u64().ok_or(ApiError::Response)? {
            return Ok(examples);
        }
        if rows.is_empty() {
            return Err(ApiError::Response);
        }
    }
}
fn content_key(case: &EvalCase) -> (String, Vec<String>) {
    let mut ids = relevant_ids(&case.relevant);
    ids.sort();
    (case.query.trim().into(), ids)
}
/// Only explicit removals are deleted, and deletion runs after imports and PATCHes.
pub async fn save_cases(
    api: &DesktopApi,
    kb: &str,
    local: Vec<EvalCase>,
    removed: HashSet<String>,
) -> Result<Value, ApiError> {
    let dataset = resolve_link(api, kb).await?;
    let id = dataset["dataset_id"].as_str().ok_or(ApiError::Response)?;
    let examples = list_examples(api, id).await?;
    let server: BTreeMap<String, EvalCase> = examples
        .iter()
        .filter_map(project)
        .map(|c| (c.id.clone(), c))
        .collect();
    let mut imports = Vec::new();
    let mut updates = Vec::new();
    for case in &local {
        if let Some(previous) = server.get(&case.id) {
            if content_key(case) != content_key(previous) {
                updates.push((
                    previous.example_id.clone().ok_or(ApiError::Response)?,
                    case.clone(),
                ));
            }
        } else {
            imports.push(import_item(kb, case));
        }
    }
    let mut imported = 0u64;
    let mut skipped = 0u64;
    for batch in imports.chunks(500) {
        let result = api
            .execute(ApiRequest::mutation(
                Method::POST,
                &["api", "v1", "eval", "datasets", id, "examples:import"],
                Some(json!({"examples":batch,"mode":"skip_duplicates"})),
            ))
            .await?;
        imported += result["imported"].as_u64().unwrap_or(0);
        skipped += result["skipped"].as_u64().unwrap_or(0);
    }
    let mut updated = 0;
    for (example, case) in updates {
        api.execute(ApiRequest::mutation(Method::PATCH,&["api","v1","eval","datasets",id,"examples",&example],Some(json!({"input":{"query":case.query},"expected_output":{"relevant_segment_ids":relevant_ids(&case.relevant)},"expected_trajectory":{},"assertions":[],"metadata":{"case_id":case.id,"source":SOURCE,"kb_dataset_id":kb}})))).await?;
        updated += 1;
    }
    let mut deleted = 0;
    for case in server.values() {
        if let Some(example) = &case.example_id {
            if removed.contains(example) {
                api.execute(ApiRequest::mutation(
                    Method::DELETE,
                    &["api", "v1", "eval", "datasets", id, "examples", example],
                    None,
                ))
                .await?;
                deleted += 1;
            }
        }
    }
    Ok(
        json!({"dataset_id":id,"imported":imported,"skipped":skipped,"updated":updated,"deleted":deleted,"examples":list_examples(api,id).await?}),
    )
}
pub fn parse_jsonl(text: &str) -> Result<Vec<Value>, String> {
    if text.len() > 8 * 1024 * 1024 {
        return Err("JSONL exceeds 8 MiB".into());
    }
    let mut rows = Vec::new();
    let mut ids = HashSet::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let raw: Value = serde_json::from_str(line)
            .map_err(|_| format!("Invalid JSON on line {}", index + 1))?;
        if !raw.is_object() {
            return Err(format!("Line {} must be an object", index + 1));
        }
        let id = raw["case_id"].as_str().unwrap_or("").trim();
        if id.is_empty() || !ids.insert(id.to_owned()) {
            return Err(format!(
                "Line {} has a missing or duplicate case_id",
                index + 1
            ));
        }
        if raw["metadata"]
            .get("critical")
            .is_some_and(|v| !v.is_null() && !v.is_boolean())
        {
            return Err(format!(
                "Line {} metadata.critical must be boolean",
                index + 1
            ));
        }
        let mut row = json!({"case_id":id,"split":raw["split"].as_str().filter(|s|!s.trim().is_empty()).map(str::trim).unwrap_or("regression")});
        for key in [
            "input",
            "expected_output",
            "expected_trajectory",
            "metadata",
        ] {
            row[key] = raw
                .get(key)
                .filter(|v| v.is_object())
                .cloned()
                .unwrap_or(json!({}));
        }
        row["assertions"] = json!(
            raw["assertions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|v| v.is_object())
                .cloned()
                .collect::<Vec<_>>()
        );
        for key in ["source_trace_id", "source_span_id"] {
            row[key] = raw
                .get(key)
                .filter(|v| v.is_string())
                .cloned()
                .unwrap_or(Value::Null);
        }
        rows.push(row);
    }
    Ok(rows)
}
pub fn source_versions(kb: &str, hits: &[Value]) -> Vec<Value> {
    hits.iter().filter_map(|hit|{let doc=hit["document_id"].as_str().filter(|s|!s.is_empty())?;let segment=hit["segment_id"].as_str().filter(|s|!s.is_empty())?;let version=hit["source_version"].as_u64().filter(|v|*v>0)?;let hash=hit["source_hash"].as_str().filter(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit()))?;Some(json!({"kb_dataset_id":kb,"document_id":doc,"segment_id":segment,"source_version":version,"source_hash":hash.to_ascii_lowercase()}))}).collect()
}
