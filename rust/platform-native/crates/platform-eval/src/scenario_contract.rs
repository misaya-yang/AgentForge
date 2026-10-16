//! Strict real-domain scenario contracts and independently re-read source bindings.
use crate::{
    Result,
    canonical::{digest, sha256},
    seals::{finite, hash, integer, strict_keys},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};
fn identifier(v: &Value) -> Result<&str> {
    let s = v.as_str().ok_or("identifier must be text")?.trim();
    if !regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$")?.is_match(s) {
        return Err("invalid identifier".into());
    }
    Ok(s)
}
fn bounded(v: &Value, max: usize) -> Result<&str> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().count() <= max)
        .ok_or_else(|| "invalid bounded text".into())
}
fn list(v: &Value, max: usize, minimum: usize) -> Result<&Vec<Value>> {
    v.as_array()
        .filter(|a| (minimum..=max).contains(&a.len()))
        .ok_or_else(|| "invalid list length".into())
}
fn tokens(v: &Value, max: usize, min: usize, width: usize) -> Result<Vec<String>> {
    let a = list(v, max, min)?;
    let strings = a
        .iter()
        .map(|v| bounded(v, width).map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    if strings.iter().collect::<BTreeSet<_>>().len() != strings.len() {
        return Err("duplicate text tokens".into());
    }
    Ok(strings)
}
pub fn load(path: &Path) -> Result<Value> {
    let v = crate::strict_json::parse(&std::fs::read(path)?)?;
    validate(&v)?;
    Ok(v)
}
pub fn validate(v: &Value) -> Result<()> {
    strict_keys(
        v,
        &["schema_version", "suite_id", "scenarios"],
        &[],
        "scenario contract",
    )?;
    if v["schema_version"] != "real-agent-scenarios/v1" {
        return Err("unsupported scenario schema".into());
    }
    identifier(&v["suite_id"])?;
    let mut ids = BTreeSet::new();
    for s in list(&v["scenarios"], 64, 1)? {
        strict_keys(
            s,
            &[
                "scenario_id",
                "domain",
                "prompt",
                "repetitions",
                "required_agent_ids",
                "require_parallel",
                "expected_assertions",
            ],
            &[
                "answer_locator",
                "model_id",
                "max_tokens",
                "execution_profile",
                "source_artifacts",
                "delegation_task_requirements",
                "canonical_delegation",
                "output_conformance_hints",
            ],
            "scenario",
        )?;
        if !ids.insert(identifier(&s["scenario_id"])?.to_owned()) {
            return Err("duplicate scenario ID".into());
        }
        bounded(&s["domain"], 120)?;
        bounded(&s["prompt"], 40000)?;
        integer(&s["repetitions"], 1, 5, "repetitions")?;
        if let Some(hints) = s.get("output_conformance_hints") {
            tokens(hints, 32, 0, 800)?;
        }
        let agents = list(&s["required_agent_ids"], 5, 0)?
            .iter()
            .map(identifier)
            .collect::<Result<Vec<_>>>()?;
        if agents.iter().collect::<BTreeSet<_>>().len() != agents.len() {
            return Err("duplicate required agents".into());
        }
        let parallel = s["require_parallel"]
            .as_bool()
            .ok_or("parallel must be boolean")?;
        if parallel && agents.len() < 2 {
            return Err("parallel requires two agents".into());
        }
        if !agents.is_empty() {
            let requirements = list(
                &s["delegation_task_requirements"],
                agents.len(),
                agents.len(),
            )?;
            let mut requirement_ids = vec![];
            for r in requirements {
                strict_keys(
                    r,
                    &[
                        "agent_id",
                        "prompt_contains_all",
                        "prompt_excludes_all",
                        "min_prompt_chars",
                    ],
                    &[],
                    "delegation requirement",
                )?;
                requirement_ids.push(identifier(&r["agent_id"])?);
                tokens(&r["prompt_contains_all"], 32, 1, 240)?;
                tokens(&r["prompt_excludes_all"], 32, 1, 240)?;
                integer(&r["min_prompt_chars"], 160, 20000, "min_prompt_chars")?;
            }
            requirement_ids.sort();
            let mut sorted_agents = agents.clone();
            sorted_agents.sort();
            if requirement_ids != sorted_agents {
                return Err("delegation requirements do not match required agents".into());
            }
            let c = &s["canonical_delegation"];
            strict_keys(
                c,
                &["max_concurrency", "tasks", "canonical_sha256"],
                &[],
                "canonical delegation",
            )?;
            if c["max_concurrency"].as_u64() != Some(if parallel { agents.len() as u64 } else { 1 })
            {
                return Err("canonical concurrency must be exact".into());
            }
            let tasks = list(&c["tasks"], agents.len(), agents.len())?;
            for (i, t) in tasks.iter().enumerate() {
                strict_keys(
                    t,
                    &["prompt", "description"],
                    &["agent_id", "agent_type"],
                    "canonical task",
                )?;
                let aid = t
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty());
                let atype = t
                    .get("agent_type")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty());
                if aid.is_some() == atype.is_some() {
                    return Err("canonical task requires exactly one agent identity".into());
                }
                let id = if aid.is_some() {
                    identifier(&t["agent_id"])?.to_owned()
                } else {
                    format!("builtin:{}", identifier(&t["agent_type"])?)
                };
                if id != agents[i] {
                    return Err("canonical task order differs from required agents".into());
                }
                bounded(&t["description"], 200)?;
                let prompt = bounded(&t["prompt"], 20000)?;
                let r = requirements
                    .iter()
                    .find(|r| r["agent_id"] == id)
                    .ok_or("missing task requirement")?;
                if prompt.trim().chars().count()
                    < r["min_prompt_chars"].as_u64().unwrap_or(0) as usize
                    || !r["prompt_contains_all"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|v| prompt.contains(v.as_str().unwrap()))
                    || !r["prompt_excludes_all"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|v| {
                            !prompt
                                .to_lowercase()
                                .contains(&v.as_str().unwrap().to_lowercase())
                        })
                {
                    return Err("canonical task violates prompt requirement".into());
                }
            }
            hash(&c["canonical_sha256"], "canonical delegation digest")?;
            if c["canonical_sha256"]
                != digest(&json!({"tasks":tasks,"max_concurrency":c["max_concurrency"]}))
            {
                return Err("canonical delegation digest mismatch".into());
            }
        } else {
            if s.get("delegation_task_requirements")
                .is_some_and(|v| !v.is_null() && v != &json!([]))
                || s.get("canonical_delegation")
                    .is_some_and(|v| !v.is_null() && v != &json!({}))
            {
                return Err("delegation declared without agents".into());
            }
        }
        if !["final_json_tag", "whole_output_json"].contains(
            &s.get("answer_locator")
                .and_then(Value::as_str)
                .unwrap_or("final_json_tag"),
        ) {
            return Err("unsupported answer locator".into());
        }
        if let Some(m) = s.get("model_id") {
            bounded(m, 120)?;
        }
        if let Some(m) = s.get("max_tokens") {
            integer(m, 256, 8192, "max_tokens")?;
        }
        if !["safe", "balanced", "powerful"].contains(
            &s.get("execution_profile")
                .and_then(Value::as_str)
                .unwrap_or("safe"),
        ) {
            return Err("unsupported execution profile".into());
        }
        let mut assertion_ids = BTreeSet::new();
        for a in list(&s["expected_assertions"], 64, 1)? {
            validate_assertion(a)?;
            if !assertion_ids.insert(identifier(&a["assertion_id"])?.to_owned()) {
                return Err("duplicate assertion ID".into());
            }
        }
        if let Some(artifacts) = s.get("source_artifacts") {
            let mut seen = BTreeSet::new();
            for a in list(artifacts, 32, 0)? {
                strict_keys(
                    a,
                    &["artifact_id", "path", "sha256"],
                    &["description"],
                    "source artifact",
                )?;
                if !seen.insert(identifier(&a["artifact_id"])?.to_owned()) {
                    return Err("duplicate artifact ID".into());
                }
                let p = bounded(&a["path"], 512)?;
                if Path::new(p).is_absolute()
                    || Path::new(p)
                        .components()
                        .any(|c| matches!(c, Component::ParentDir))
                {
                    return Err("artifact path must be contained relative path".into());
                }
                hash(&a["sha256"], "source artifact digest")?;
                if let Some(d) = a.get("description") {
                    bounded(d, 1000)?;
                }
            }
        }
    }
    Ok(())
}
pub fn validate_assertion(a: &Value) -> Result<()> {
    let kind = a["kind"].as_str().ok_or("assertion kind missing")?;
    if ![
        "json_equals",
        "json_number",
        "json_set_equals",
        "json_contains_all",
        "json_excludes_all",
        "json_keys_equals",
        "json_nonempty_string",
        "text_contains_all",
        "text_excludes_all",
    ]
    .contains(&kind)
    {
        return Err("unsupported assertion kind".into());
    }
    let mut required = vec!["assertion_id", "kind", "expected"];
    if kind.starts_with("json_") {
        required.push("path")
    }
    let mut optional = vec!["description"];
    if kind == "json_number" {
        optional.extend(["absolute_tolerance", "relative_tolerance"])
    }
    strict_keys(a, &required, &optional, "assertion")?;
    identifier(&a["assertion_id"])?;
    if let Some(d) = a.get("description") {
        bounded(d, 1000)?;
    }
    if kind.starts_with("json_") {
        let p = a["path"].as_str().ok_or("JSON pointer must be text")?;
        if (!p.is_empty() && !p.starts_with('/')) || p.chars().count() > 512 {
            return Err("invalid JSON pointer".into());
        }
    }
    if [
        "json_set_equals",
        "json_contains_all",
        "json_excludes_all",
        "json_keys_equals",
        "text_contains_all",
        "text_excludes_all",
    ]
    .contains(&kind)
    {
        let values = list(&a["expected"], 64, 1)?;
        if kind.starts_with("text_") || kind == "json_keys_equals" {
            for v in values {
                if !v.as_str().is_some_and(|s| !s.is_empty()) {
                    return Err("expected text values".into());
                }
            }
        }
    }
    if kind == "json_nonempty_string" {
        integer(&a["expected"], 1, 10000, "minimum string length")?;
    }
    if kind == "json_number" {
        if !a["expected"].is_number() {
            return Err("expected must be numeric".into());
        }
        for key in ["absolute_tolerance", "relative_tolerance"] {
            if let Some(v) = a.get(key) {
                finite(v, 0., key)?;
            }
        }
    }
    Ok(())
}
pub fn verify_source_artifacts(v: &Value, directory: &Path) -> Result<Vec<Value>> {
    let root = directory.canonicalize()?;
    let mut receipts = vec![];
    for s in v["scenarios"].as_array().ok_or("missing scenarios")? {
        for a in list(&s["source_artifacts"], 32, 1)? {
            let relative = a["path"].as_str().ok_or("artifact path missing")?;
            let unresolved = directory.join(relative);
            if std::fs::symlink_metadata(&unresolved)?
                .file_type()
                .is_symlink()
            {
                return Err("source symlinks not allowed".into());
            }
            let path = unresolved.canonicalize()?;
            if !path.starts_with(&root) || !path.is_file() {
                return Err("source escapes scenario directory".into());
            }
            let metadata = path.metadata()?;
            if metadata.len() > 2_000_000 {
                return Err("source artifact exceeds 2 MB".into());
            }
            let observed = sha256(&std::fs::read(&path)?);
            if a["sha256"] != observed {
                return Err("source artifact digest mismatch".into());
            }
            receipts.push(json!({"scenario_id":s["scenario_id"],"artifact_id":a["artifact_id"],"relative_path":relative,"content_sha256":observed,"size_bytes":metadata.len()}));
        }
    }
    Ok(receipts)
}
pub fn verify_plugin_definitions(v: &Value, repository: &Path) -> Result<Vec<Value>> {
    let root = repository.join("agent-plugins").canonicalize()?;
    let mut ids = BTreeSet::new();
    for s in v["scenarios"].as_array().ok_or("missing scenarios")? {
        for id in s["required_agent_ids"]
            .as_array()
            .ok_or("missing required agents")?
        {
            let id = id.as_str().ok_or("invalid agent ID")?;
            if !id.starts_with("builtin:") {
                ids.insert(id);
            }
        }
    }
    let component = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")?;
    let mut out = vec![];
    for id in ids {
        let pieces = id.split(':').collect::<Vec<_>>();
        if pieces.len() != 2 || pieces.iter().any(|s| !component.is_match(s)) {
            return Err("malformed qualified plugin agent ID".into());
        }
        let dir = root.join(pieces[0]).canonicalize()?;
        if !dir.starts_with(&root) {
            return Err("unsafe plugin directory".into());
        }
        let paths = [
            dir.join("plugin.json"),
            dir.join("agents").join(format!("{}.md", pieces[1])),
        ];
        let mut hashes = vec![];
        for p in paths {
            let m = std::fs::symlink_metadata(&p)?;
            if m.file_type().is_symlink()
                || !m.is_file()
                || m.len() > 100000
                || !p.canonicalize()?.starts_with(&dir)
            {
                return Err("unsafe or unavailable plugin definition".into());
            }
            hashes.push(sha256(&std::fs::read(p)?));
        }
        out.push(json!({"qualified_agent_id":id,"plugin_manifest_sha256":hashes[0],"definition_sha256":hashes[1]}));
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migrated_rust_fixture_contract_is_hash_bound() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let path = root.join("tests/fixtures/eval/general_agent/coding_parallel_v2/scenario.json");
        let v = load(&path).unwrap();
        assert_eq!(v["suite_id"], "general-agent.real-coding.v2");
        assert_eq!(
            verify_source_artifacts(&v, path.parent().unwrap())
                .unwrap()
                .len(),
            15
        );
    }
    #[test]
    fn unknown_assertion_fields_are_rejected() {
        assert!(validate_assertion(&json!({"assertion_id":"x","kind":"json_number","expected":1,"path":"/x","candidate_score":100})).is_err());
    }
}
