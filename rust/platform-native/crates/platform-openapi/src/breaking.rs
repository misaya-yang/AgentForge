//! Conservative release gate: unproved contract changes are blocking, never silently skipped.
//! Annotation-only changes and additive paths/operations are compatible. Schema assertions,
//! referenced components, inherited parameters/security and streaming records are compared.
use serde_json::Value;
use std::collections::BTreeSet;
const METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace", "query",
];
fn annotation(key: &str) -> bool {
    matches!(
        key,
        "description" | "summary" | "title" | "example" | "examples" | "externalDocs" | "$comment"
    ) || key.starts_with("x-")
}
fn canonical(value: &Value, doc: &Value, stack: &mut BTreeSet<String>) -> Result<Value, String> {
    match value {
        Value::Object(map) => {
            let mut result = serde_json::Map::new();
            for (key, child) in map {
                if annotation(key) {
                    continue;
                }
                if key == "$ref" {
                    let reference = child.as_str().ok_or("non-string $ref")?;
                    let pointer = reference.strip_prefix('#').ok_or_else(|| {
                        format!("external reference requires bundled release: {reference}")
                    })?;
                    let target = doc
                        .pointer(pointer)
                        .ok_or_else(|| format!("unresolved reference: {reference}"))?;
                    if stack.insert(reference.to_owned()) {
                        result.insert("$resolved".into(), canonical(target, doc, stack)?);
                        stack.remove(reference);
                    } else {
                        // Preserve recursive identity; changes to the surrounding target still compare.
                        result.insert("$recursive".into(), child.clone());
                    }
                } else if ["const", "enum", "default", "security"].contains(&key.as_str()) {
                    // Literal payloads and security scheme names are not schema annotations.
                    result.insert(key.clone(), child.clone());
                } else if [
                    "properties",
                    "patternProperties",
                    "$defs",
                    "definitions",
                    "dependentSchemas",
                    "schemas",
                    "securitySchemes",
                    "responses",
                    "content",
                    "headers",
                    "links",
                    "callbacks",
                    "webhooks",
                    "paths",
                    "encoding",
                    "additionalOperations",
                ]
                .contains(&key.as_str())
                {
                    // Keys in named maps are user-defined identifiers, including "description".
                    let named = match child.as_object() {
                        Some(map) => Value::Object(
                            map.iter()
                                .map(|(name, value)| {
                                    Ok((name.clone(), canonical(value, doc, stack)?))
                                })
                                .collect::<Result<serde_json::Map<_, _>, String>>()?,
                        ),
                        None => canonical(child, doc, stack)?,
                    };
                    result.insert(key.clone(), named);
                } else {
                    result.insert(key.clone(), canonical(child, doc, stack)?);
                }
            }
            Ok(Value::Object(result))
        }
        Value::Array(values) => values.iter().map(|v| canonical(v, doc, stack)).collect(),
        _ => Ok(value.clone()),
    }
}
fn normalized(value: &Value, doc: &Value) -> Result<Value, String> {
    canonical(value, doc, &mut BTreeSet::new())
}
/// Fail closed on unknown assertion changes. This deliberately prefers reviewable false
/// positives over claiming arbitrary JSON Schema language inclusion is decidable here.
pub fn compare(base: &Value, candidate: &Value) -> Result<Vec<String>, String> {
    let mut violations = vec![];
    let old_paths = base["paths"]
        .as_object()
        .ok_or("baseline paths must be object")?;
    let new_paths = candidate["paths"]
        .as_object()
        .ok_or("candidate paths must be object")?;
    for (path, item) in old_paths {
        let Some(new_item) = new_paths.get(path) else {
            violations.push(format!("path removed: {path}"));
            continue;
        };
        if item.get("$ref").is_some() || new_item.get("$ref").is_some() {
            if normalized(item, base)? != normalized(new_item, candidate)? {
                violations.push(format!("referenced path item changed: {path}"));
            }
            continue;
        }
        if normalized(
            &serde_json::json!({"additionalOperations":item["additionalOperations"]}),
            base,
        )? != normalized(
            &serde_json::json!({"additionalOperations":new_item["additionalOperations"]}),
            candidate,
        )? {
            violations.push(format!("additional HTTP operations changed: {path}"));
        }
        for method in METHODS {
            let Some(old) = item.get(*method) else {
                continue;
            };
            let Some(new) = new_item.get(*method) else {
                violations.push(format!("operation removed: {method} {path}"));
                continue;
            };
            let inherited = |operation: &Value, path_item: &Value, doc: &Value| {
                let mut op = operation.clone();
                if op.get("security").is_none() {
                    op["security"] = doc
                        .get("security")
                        .cloned()
                        .unwrap_or(serde_json::json!([]));
                }
                // Path parameters remain separate because operation parameters override by name/in.
                op["effectivePathParameters"] = path_item
                    .get("parameters")
                    .cloned()
                    .unwrap_or(serde_json::json!([]));
                op["effectiveServers"] = operation
                    .get("servers")
                    .or_else(|| path_item.get("servers"))
                    .or_else(|| doc.get("servers"))
                    .cloned()
                    .unwrap_or(serde_json::json!([]));
                op
            };
            if normalized(&inherited(old, item, base), base)?
                != normalized(&inherited(new, new_item, candidate), candidate)?
            {
                violations.push(format!(
                    "operation contract changed (compatibility unproved): {method} {path}"
                ));
            }
        }
    }
    for field in ["security", "servers"] {
        if normalized(&serde_json::json!({field:base[field]}), base)?
            != normalized(&serde_json::json!({field:candidate[field]}), candidate)?
        {
            violations.push(format!("root {field} changed"));
        }
    }
    // Security scheme definitions do not need a $ref from security requirements.
    if normalized(
        &serde_json::json!({"securitySchemes":base["components"]["securitySchemes"]}),
        base,
    )? != normalized(
        &serde_json::json!({"securitySchemes":candidate["components"]["securitySchemes"]}),
        candidate,
    )? {
        violations.push("security scheme definitions changed".into());
    }
    // Webhooks and reusable callbacks can carry independently exposed operations.
    if normalized(&serde_json::json!({"webhooks":base["webhooks"]}), base)?
        != normalized(
            &serde_json::json!({"webhooks":candidate["webhooks"]}),
            candidate,
        )?
    {
        violations.push("webhook contract changed".into());
    }
    Ok(violations)
}
pub fn major(version: &str) -> Result<u64, String> {
    let core = version.split_once('-').map_or(version, |(core, _)| core);
    let fields: Vec<_> = core.split('.').collect();
    if fields.len() != 3
        || fields.iter().any(|v| {
            v.is_empty()
                || !v.bytes().all(|c| c.is_ascii_digit())
                || (v.len() > 1 && v.starts_with('0'))
        })
    {
        return Err(
            "release version must be numeric MAJOR.MINOR.PATCH (optional prerelease)".into(),
        );
    }
    fields[0]
        .parse()
        .map_err(|_| "release major overflows".into())
}
pub fn authorize(
    base: &Value,
    candidate: &Value,
    allow_major: bool,
) -> Result<Vec<String>, String> {
    let issues = compare(base, candidate)?;
    if issues.is_empty() {
        return Ok(issues);
    }
    if allow_major {
        let previous = major(
            base["info"]["version"]
                .as_str()
                .ok_or("baseline release version missing")?,
        )?;
        let next = major(
            candidate["info"]["version"]
                .as_str()
                .ok_or("candidate release version missing")?,
        )?;
        if next <= previous {
            return Err("breaking changes require an explicit increasing major release".into());
        }
    }
    Ok(issues)
}
