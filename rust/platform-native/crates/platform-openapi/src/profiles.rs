//! Explicit legacy/public/native ownership metadata; never infer exclusions from drift.
use crate::gate::{METHODS, operation_ids};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};
pub const PROFILES: &[&str] = &["python-hybrid", "native", "published"];
fn string_set(value: &Value) -> Result<BTreeSet<String>, String> {
    value
        .as_array()
        .ok_or("expected string array")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "expected string".into())
        })
        .collect()
}
fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, _)| k.as_str())
        .collect()
}
fn check_proofs(root: &Path, relative: &str, proofs: &Value) -> Result<String, String> {
    let source = root
        .join(relative)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let native = root
        .join("rust/platform-native")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !source.starts_with(native) {
        return Err("invalid native source".into());
    }
    let text = fs::read_to_string(source).map_err(|e| e.to_string())?;
    let proofs = proofs
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or("missing native source evidence")?;
    if proofs
        .iter()
        .any(|p| p.as_str().is_none_or(|s| s.is_empty() || !text.contains(s)))
    {
        return Err("stale native source evidence".into());
    }
    Ok(text)
}
pub fn load_ownership(published: &Value, path: &Path, root: &Path) -> Result<Value, String> {
    let ownership: Value = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    validate_ownership(published, &ownership, root)?;
    Ok(ownership)
}
pub fn validate_ownership(published: &Value, ownership: &Value, root: &Path) -> Result<(), String> {
    if keys(ownership)
        != BTreeSet::from([
            "version",
            "native_only",
            "native_security_schemes",
            "python_paths",
        ])
        || ownership["version"] != 1
    {
        return Err("unknown OpenAPI ownership format".into());
    }
    let mut seen = BTreeSet::new();
    for entry in ownership["native_only"]
        .as_array()
        .ok_or("invalid native_only")?
    {
        let fields = keys(entry);
        if fields != BTreeSet::from(["path", "operations", "source", "source_evidence"])
            && fields
                != BTreeSet::from([
                    "path",
                    "operations",
                    "source",
                    "source_evidence",
                    "shared_source_evidence",
                ])
        {
            return Err("unknown ownership entry fields".into());
        }
        let route = entry["path"].as_str().ok_or("invalid native path")?;
        if !seen.insert(route.to_string()) {
            return Err(format!("overlapping ownership: {route}"));
        }
        let operations: serde_json::Map<String, Value> = published["paths"][route]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(m, _)| METHODS.contains(&m.as_str()))
            .map(|(m, o)| (m.clone(), o["operationId"].clone()))
            .collect();
        if operations.is_empty() || Value::Object(operations) != entry["operations"] {
            return Err(format!("stale native operation ownership: {route}"));
        }
        let text = check_proofs(
            root,
            entry["source"].as_str().ok_or("invalid source")?,
            &entry["source_evidence"],
        )?;
        let empty = json!({});
        let shared = entry
            .get("shared_source_evidence")
            .unwrap_or(&empty)
            .as_object()
            .ok_or("invalid shared native source evidence")?;
        if text.contains("native_contract") && shared.is_empty() {
            return Err(format!("missing shared native source evidence: {route}"));
        }
        for (relative, proofs) in shared {
            check_proofs(root, relative, proofs)?;
        }
    }
    let paths = string_set(&ownership["python_paths"])?;
    if paths.len() != ownership["python_paths"].as_array().unwrap().len()
        || !paths.is_disjoint(&seen)
    {
        return Err("overlapping Python/native path ownership".into());
    }
    let all: BTreeSet<String> = published["paths"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(p, _)| p.clone())
        .collect();
    if paths.union(&seen).cloned().collect::<BTreeSet<_>>() != all {
        return Err("unknown or stale published path ownership".into());
    }
    let schemes = string_set(&ownership["native_security_schemes"])?;
    let declared: BTreeSet<String> = published["components"]["securitySchemes"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, _)| k.clone())
        .collect();
    if schemes.len()
        != ownership["native_security_schemes"]
            .as_array()
            .unwrap()
            .len()
        || !schemes.is_subset(&declared)
    {
        return Err("stale or overlapping native security ownership".into());
    }
    for (route, item) in published["paths"].as_object().into_iter().flatten() {
        for (method, operation) in item.as_object().into_iter().flatten() {
            if !METHODS.contains(&method.as_str()) {
                continue;
            }
            let security = operation
                .get("security")
                .or_else(|| published.get("security"));
            let refs: BTreeSet<_> = security
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(|v| v.as_object().into_iter().flatten().map(|(k, _)| k.clone()))
                .collect();
            if !seen.contains(route) && !refs.is_disjoint(&schemes) {
                return Err(format!(
                    "native security overlaps Python ownership: {route}"
                ));
            }
        }
    }
    Ok(())
}
pub fn project(published: &Value, profile: &str, ownership: &Value) -> Result<Value, String> {
    if !PROFILES.contains(&profile) {
        return Err(format!("unknown OpenAPI profile: {profile}"));
    }
    let mut result = published.clone();
    if profile != "python-hybrid" {
        return Ok(result);
    }
    let paths = string_set(&ownership["python_paths"])?;
    if let Some(map) = result["paths"].as_object_mut() {
        map.retain(|p, _| paths.contains(p));
    }
    let schemes = string_set(&ownership["native_security_schemes"])?;
    if let Some(map) = result["components"]["securitySchemes"].as_object_mut() {
        map.retain(|name, _| !schemes.contains(name));
    }
    Ok(result)
}
pub fn check_legacy_ownership(current: &Value, ownership: &Value) -> Result<(), String> {
    let native: BTreeSet<_> = ownership["native_only"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["path"].as_str())
        .collect();
    let schemes = string_set(&ownership["native_security_schemes"])?;
    let overlap: BTreeSet<_> = current["paths"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(p, _)| native.contains(p.as_str()))
        .map(|(p, _)| p.clone())
        .chain(
            current["components"]["securitySchemes"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, _)| schemes.contains(*k))
                .map(|(k, _)| k.clone()),
        )
        .collect();
    if overlap.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Python export overlaps native ownership: {overlap:?}"
        ))
    }
}
pub fn declaration_drift(previous: &Value, current: &Value) -> Result<Vec<String>, String> {
    if previous["x-native-contract"]["scope"] != "native-only" {
        return Err("previous document must be a bounded native-source export".into());
    }
    Ok([
        "openapi",
        "paths",
        "components",
        "x-native-knowledge-forwarding",
        "x-python-knowledge-owner",
    ]
    .into_iter()
    .filter(|key| previous[*key] != current[*key])
    .map(str::to_owned)
    .collect())
}
pub fn operation_count(document: &Value) -> usize {
    operation_ids(document).len()
}
