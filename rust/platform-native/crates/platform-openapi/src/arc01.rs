//! Full operation-object comparison and explicit historical wording delta.
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};
pub const KEYS: &[&str] = &[
    "DELETE /api/v1/assistant/artifacts/{artifact_id}",
    "DELETE /api/v1/assistant/sessions/{session_id}",
    "GET /api/v1/assistant/artifacts/{artifact_id}",
    "GET /api/v1/assistant/artifacts/{artifact_id}/download",
    "GET /api/v1/assistant/config",
    "GET /api/v1/assistant/datasets",
    "GET /api/v1/assistant/metrics/tenant",
    "GET /api/v1/assistant/models",
    "GET /api/v1/assistant/policies",
    "GET /api/v1/assistant/runs/{run_id}",
    "GET /api/v1/assistant/sessions",
    "GET /api/v1/assistant/sessions/{session_id}",
    "GET /api/v1/assistant/sessions/{session_id}/artifacts",
    "GET /api/v1/assistant/sessions/{session_id}/history",
    "GET /api/v1/assistant/sessions/{session_id}/metrics",
    "GET /api/v1/assistant/tools",
    "POST /api/v1/assistant/approvals/{approval_id}",
    "POST /api/v1/assistant/artifacts",
    "POST /api/v1/assistant/chat",
    "POST /api/v1/assistant/chat/stream",
    "POST /api/v1/assistant/runs/{run_id}/resume",
    "POST /api/v1/assistant/sessions",
    "POST /api/v1/assistant/tasks/{task_id}/cancel",
];
pub fn deltas() -> Value {
    serde_json::from_str(r#"{"POST /api/v1/assistant/tasks/{task_id}/cancel": {"description": {"published": "Authenticate at the edge and cancel in the owning Assistant process.", "current": "Map the V1 task identifier to the owning Runtime run/turn interrupt.", "reason": "fe2e1b88 completed the single Rust Agent Runtime cutover; the public wording must no longer claim that an Assistant process owns cancellation", "source_commit": "fe2e1b88"}}}"#).expect("static delta")
}

pub fn operations(doc: &Value, scoped: bool) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    if let Some(paths) = doc["paths"].as_object() {
        for (path, item) in paths {
            if let Some(item) = item.as_object() {
                for (method, operation) in item {
                    if !["get", "post", "put", "delete", "patch", "options", "head"]
                        .contains(&method.as_str())
                    {
                        continue;
                    }
                    let key = format!("{} {path}", method.to_uppercase());
                    if !scoped || KEYS.contains(&key.as_str()) {
                        out.insert(key, operation.clone());
                    }
                }
            }
        }
    }
    out
}
pub fn compare(current: &Value, published: &Value) -> Vec<String> {
    let c = operations(current, true);
    let p = operations(published, true);
    let mut errors = vec![];
    for (name, map) in [("current", &c), ("published", &p)] {
        let missing = KEYS
            .iter()
            .filter(|k| !map.contains_key(**k))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            errors.push(format!(
                "{name} OpenAPI is missing ARC-01 operations: {missing:?}"
            ));
        }
    }
    let deltas = deltas();
    for key in KEYS {
        let (Some(actual), Some(expected)) = (c.get(*key), p.get(*key)) else {
            continue;
        };
        let mut actual = actual.clone();
        let mut expected = expected.clone();
        if let Some(fields) = deltas[*key].as_object() {
            for (field, delta) in fields {
                if expected.get(field) == actual.get(field) {
                    continue;
                }
                if expected.get(field) != delta.get("published") {
                    errors.push(format!("{key} intentional delta manifest has stale published {field:?}: {:?} != {:?}",expected.get(field),delta.get("published")));
                }
                if actual.get(field) != delta.get("current") {
                    errors.push(format!("{key} intentional delta manifest has stale current {field:?}: {:?} != {:?}",actual.get(field),delta.get("current")));
                }
                if let Some(o) = actual.as_object_mut() {
                    o.remove(field);
                }
                if let Some(o) = expected.as_object_mut() {
                    o.remove(field);
                }
            }
        }
        if actual != expected {
            errors.push(format!(
                "unapproved public operation drift for {key}:\npublished={}\ncurrent={}",
                expected, actual
            ));
        }
    }
    errors
}
pub fn export(doc: &Value) -> Value {
    let all = operations(doc, false);
    let mut routes = vec![];
    if let Some(paths) = doc["paths"].as_object() {
        for (path, item) in paths {
            if let Some(item) = item.as_object() {
                for (method, op) in item {
                    if !all.contains_key(&format!("{} {path}", method.to_uppercase())) {
                        continue;
                    }
                    let responses = op["responses"]
                        .as_object()
                        .map(|o| o.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default();
                    routes.push(json!({"path":path,"methods":[method.to_uppercase()],"operation_id":op["operationId"],"status_code":null,"responses":responses}));
                }
            }
        }
    }
    json!({"schema":"arc01-public-operation-contract/v2","routes":routes,"public_operations":all,"arc01_public_operations":operations(doc,true),"intentional_public_operation_deltas":deltas(),"native_declaration_complete":false})
}
pub fn run(args: &[String], doc: &Value) -> u8 {
    if args == ["--check-sdk"] {
        let source = fs::read("sdk/openapi.json")
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let Some(source) = source else {
            eprintln!("unable to read sdk/openapi.json");
            return 2;
        };
        let failures = compare(doc, &source);
        if failures.is_empty() {
            println!(
                "ARC-01 OpenAPI contract matches 23 published operation objects (1 exact intentional field delta)"
            );
            return 0;
        }
        eprintln!("{}", failures.join("\n\n"));
        return 1;
    }
    if args.len() > 1 || args.first().is_some_and(|v| v.starts_with('-')) {
        eprintln!("Usage: ai-platform-openapi arc01 [output | --check-sdk]");
        return 2;
    }
    let output = Path::new(
        args.first()
            .map(String::as_str)
            .unwrap_or("tmp/api-routes.json"),
    );
    let result = (|| -> Result<(), String> {
        if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_string_pretty(&export(doc)).map_err(|e| e.to_string())?;
        fs::write(output, bytes + "\n").map_err(|e| e.to_string())
    })();
    match result {
        Ok(()) => {
            println!("wrote {}", output.display());
            0
        }
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        let mut d = json!({"paths":{}});
        for key in KEYS {
            let (method, path) = key.split_once(' ').unwrap();
            d["paths"][path][method.to_lowercase()] =
                json!({"description":"same","responses":{"200":{"description":"ok"}}});
        }
        d
    }
    #[test]
    fn exact_allowance_does_not_hide_other_fields() {
        let old = fixture();
        let mut current = old.clone();
        assert!(compare(&current, &old).is_empty());
        current["paths"]["/api/v1/assistant/config"]["get"]["description"] = json!("drift");
        assert_eq!(compare(&current, &old).len(), 1);
        assert!(!compare(&json!({"paths":{}}), &old).is_empty());
    }
}
