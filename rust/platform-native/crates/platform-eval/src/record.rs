use crate::{Result, canonical::sha256, cli::Args};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
pub fn replay(case: &Value, payload: &Value) -> Result<Value> {
    let id = case["case_id"].as_str().unwrap_or("");
    if !payload.is_object() {
        return Err(format!("case '{id}': KS response must be a JSON object").into());
    }
    let results=payload["results"].as_array().ok_or_else(||format!("case '{id}': KS response missing a 'results' list (unexpected response shape; aborting without recording)"))?;
    let mut ranked = vec![];
    for (index, item) in results.iter().enumerate() {
        if !item.is_object() {
            return Err(format!("case '{id}': results[{}] is not an object", index + 1).into());
        }
        let segment = item["segment_id"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                format!(
                    "case '{id}': results[{}] has no usable segment_id",
                    index + 1
                )
            })?;
        ranked.push(segment.trim());
    }
    if ranked.is_empty() {
        return Err(format!("case '{id}': KS returned zero results; refusing to emit an observation that violates the validator contract").into());
    }
    Ok(
        json!({"status":"succeeded","ranked_segment_ids":ranked,"answer_source":"retrieval_only","answer":null}),
    )
}
pub fn verify_manifest(path: &str) -> Result<Value> {
    let path = Path::new(path);
    let manifest: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if !manifest.is_object() {
        return Err(format!("manifest must be a JSON object: {}", path.display()).into());
    }
    if !crate::nonempty(&manifest["version"]) {
        return Err(format!(
            "manifest requires a non-empty 'version': {}",
            path.display()
        )
        .into());
    }
    let files = manifest["files"]
        .as_object()
        .filter(|m| !m.is_empty())
        .ok_or("manifest requires a non-empty 'files' object")?;
    let mut keys = files.keys().collect::<Vec<_>>();
    keys.sort();
    let mut entries = vec![];
    for key in keys {
        let spec = &files[key];
        let digest = spec["sha256"]
            .as_str()
            .filter(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
            .ok_or_else(|| {
                format!("manifest entry '{key}' requires a lowercase sha256 hex digest")
            })?;
        let target = path.parent().unwrap_or(Path::new(".")).join(key);
        let mut entry = json!({"file":key,"expected_sha256":digest,"purpose":spec["purpose"]});
        if !target.is_file() {
            entry["status"] = json!("missing");
        } else {
            let actual = sha256(&std::fs::read(target)?);
            entry["status"] = json!(if actual == digest { "ok" } else { "mismatch" });
            entry["actual_sha256"] = json!(actual);
        }
        entries.push(entry);
    }
    Ok(
        json!({"valid":entries.iter().all(|v|v["status"]=="ok"),"version":manifest["version"],"frozen_at":manifest["frozen_at"],"manifest":path,"entries":entries}),
    )
}
pub async fn run(args: Args) -> Result<(Value, i32)> {
    let command = args.positional.first().map(String::as_str).unwrap_or("");
    if args.positional.len() != 1 {
        return Err("usage: regen_rag_observations {record,verify}".into());
    }
    if command == "verify" {
        args.reject_unknown(&["manifest"])?;
        let result =
            verify_manifest(args.get("manifest", "tests/fixtures/eval/rag/golden/manifest.json"))?;
        let code = if result["valid"] == true { 0 } else { 1 };
        return Ok((result, code));
    }
    if command != "record" {
        return Err("unknown command".into());
    }
    args.reject_unknown(&[
        "expectations",
        "output",
        "url",
        "dataset-id",
        "user-id",
        "tenant-id",
        "user-tier",
        "user-type",
        "user-roles",
        "top-k",
        "timeout",
        "retrieval-only",
        "force",
        "dry-run",
    ])?;
    let expectations = args.required("expectations")?;
    let (cases, hash) = crate::snapshot(expectations)?;
    if cases.is_empty() {
        return Err(format!("expectations file {expectations} contains zero cases").into());
    }
    let validation = crate::rag::validate_cases(&cases);
    if validation["valid"] != true {
        return Err(format!(
            "expectations failed validate_rag_cases: {}",
            validation["errors"]
        )
        .into());
    }
    let selected = cases
        .into_iter()
        .filter(|c| !args.options.contains_key("retrieval-only") || c["track"] == "retrieval_only")
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err("zero cases to record after --retrieval-only filtering".into());
    }
    let answers = selected
        .iter()
        .filter(|c| c["track"] == "answer_aware")
        .map(|c| c["case_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    if !answers.is_empty() {
        return Err(format!("answer_aware cases cannot be recorded from the retrieval plane ({}: {}); /retrieve emits ranked segments only and generated answers belong to the recorded judge/answer channel — pass --retrieval-only to record the retrieval track",answers.len(),answers.iter().take(5).copied().collect::<Vec<_>>().join(", ")).into());
    }
    let mut bindings = BTreeMap::new();
    let mut missing = vec![];
    for case in &selected {
        let id = case["case_id"].as_str().unwrap();
        if let Some(dataset) = args.options.get("dataset-id") {
            bindings.insert(id.to_string(), dataset.clone());
        } else if let Some(dataset) = case["metadata"]["dataset_id"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
        {
            bindings.insert(id.to_string(), dataset.trim().to_string());
        } else {
            missing.push(id)
        }
    }
    if !missing.is_empty() {
        return Err(format!("{} case(s) carry no dataset binding ({}); pass --dataset-id or set metadata.dataset_id on every expectations row",missing.len(),missing.iter().take(5).copied().collect::<Vec<_>>().join(", ")).into());
    }
    let mut headers = BTreeMap::<String, String>::new();
    for (flag, header) in [
        ("user-id", "X-User-Id"),
        ("tenant-id", "X-Tenant-Id"),
        ("user-tier", "X-User-Tier"),
        ("user-type", "X-User-Type"),
        ("user-roles", "X-User-Roles"),
    ] {
        if let Some(v) = args.options.get(flag).filter(|s| !s.trim().is_empty()) {
            headers.insert(header.into(), v.trim().into());
        }
    }
    if let Ok(secret) = std::env::var("AI_PLATFORM_INTERNAL_TOKEN") {
        if !secret.trim().is_empty() {
            headers.insert("X-Gateway-Secret".into(), secret.trim().into());
        }
    }
    let url = args
        .get("url", "http://localhost:8092")
        .trim_end_matches('/');
    let endpoint = format!("{url}/api/v1/knowledge/{{dataset_id}}/retrieve");
    let top_k: i64 = args.number("top-k", 5)?;
    let output = args.get("output", "tmp/eval-e1/observations-live.jsonl");
    if args.options.contains_key("dry-run") {
        return Ok((
            json!({"mode":"dry-run","expectations":{"source":expectations,"sha256":hash},"endpoint":endpoint,"case_count":selected.len(),"case_ids":selected.iter().map(|c|&c["case_id"]).collect::<Vec<_>>(),"datasets":bindings.values().collect::<BTreeSet<_>>(),"top_k":top_k,"identity_headers":headers.keys().collect::<Vec<_>>(),"output":null}),
            0,
        ));
    }
    let target = Path::new(output);
    if target.exists() && !args.options.contains_key("force") {
        return Err(format!("refusing to overwrite {output} without --force").into());
    }
    let timeout: f64 = args.number("timeout", 30.)?;
    if !timeout.is_finite() || timeout <= 0. {
        return Err("invalid timeout".into());
    }
    let client = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs_f64(timeout))
        .build()?;
    let mut rows = vec![];
    for case in &selected {
        let id = case["case_id"].as_str().unwrap();
        let endpoint = format!("{url}/api/v1/knowledge/{}/retrieve", bindings[id]);
        let mut request = client
            .post(endpoint)
            .json(&json!({"query":case["query"],"top_k":top_k}));
        for (name, value) in &headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|_| "retrieval plane unreachable; aborting without recording")?;
        if response.status() != 200 {
            return Err(format!(
                "KS returned HTTP {}; aborting without recording",
                response.status().as_u16()
            )
            .into());
        }
        let payload: Value = response
            .json()
            .await
            .map_err(|_| "KS response is not valid JSON")?;
        rows.push(json!({"case_id":id,"replay":replay(case,&payload)?}));
    }
    let validation = crate::rag::validate_observations(&selected, &crate::observations(&rows)?);
    if validation["valid"] != true {
        return Err(format!(
            "recorded observations violate the validator contract: {}",
            validation["errors"]
        )
        .into());
    }
    let bytes = crate::seed::render(&rows);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?
    }
    std::fs::write(target, &bytes)?;
    Ok((
        json!({"recorded_case_count":rows.len(),"expectations_sha256":hash,"observations":{"source":output,"sha256":sha256(&bytes)},"endpoint":endpoint}),
        0,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_results_never_fabricate_evidence() {
        assert!(replay(&json!({"case_id":"x"}), &json!({"results":[]})).is_err());
    }
    #[test]
    fn ordered_ids_preserved() {
        assert_eq!(
            replay(
                &json!({"case_id":"x"}),
                &json!({"results":[{"segment_id":" b "},{"segment_id":"a"}]})
            )
            .unwrap()["ranked_segment_ids"],
            json!(["b", "a"])
        );
    }
}
