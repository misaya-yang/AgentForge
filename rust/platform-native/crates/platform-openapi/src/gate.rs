//! Port of scripts/harness/openapi_contract_gate.py. No network or app startup.
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};
pub const METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options"];
fn object(v: &Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}
fn strings(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
fn members<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key).and_then(Value::as_array).into_iter().flatten()
}
fn local_ref<'a>(spec: &'a Value, reference: &str) -> &'a Value {
    let mut node = spec;
    for part in reference.trim_start_matches(['#', '/']).split('/') {
        match node.as_object().and_then(|o| o.get(part)) {
            Some(value) => node = value,
            None => return &Value::Null,
        }
    }
    if node.is_object() { node } else { &Value::Null }
}
fn required_fields(spec: &Value, schema: &Value) -> BTreeSet<String> {
    let mut required = BTreeSet::new();
    let mut seen = HashSet::new();
    let mut queue = vec![schema];
    while let Some(node) = queue.pop() {
        if !node.is_object() {
            continue;
        }
        if let Some(reference) = node["$ref"].as_str() {
            if seen.insert(reference) {
                queue.push(local_ref(spec, reference));
            }
            continue;
        }
        required.extend(strings(&node["required"]));
        queue.extend(members(node, "allOf"));
    }
    required
}
fn request_required(spec: &Value, operation: &Value) -> BTreeSet<String> {
    object(&operation["requestBody"]["content"])
        .values()
        .flat_map(|v| required_fields(spec, &v["schema"]))
        .collect()
}
fn parameters(spec: &Value, path: &Value, operation: &Value) -> BTreeMap<(String, String), Value> {
    let mut result = BTreeMap::new();
    for raw in members(path, "parameters").chain(members(operation, "parameters")) {
        let p = raw["$ref"].as_str().map_or(raw, |r| local_ref(spec, r));
        if let (Some(name), Some(location)) = (p["name"].as_str(), p["in"].as_str()) {
            result.insert((name.into(), location.into()), p.clone());
        }
    }
    result
}
fn constrain(properties: &mut Map<String, Value>, additional: Map<String, Value>) {
    for (name, value) in additional {
        if let Some(old) = properties.get(&name) {
            properties.insert(name, json!({"allOf":[old,value]}));
        } else {
            properties.insert(name, value);
        }
    }
}
fn properties(spec: &Value, schema: &Value, mut active: HashSet<usize>) -> Map<String, Value> {
    if !schema.is_object() || !active.insert(schema as *const Value as usize) {
        return Map::new();
    }
    let reference = schema["$ref"].as_str();
    let is31 = spec["openapi"].as_str().unwrap_or("").starts_with("3.1.");
    if let Some(r) = reference {
        if !is31 {
            return properties(spec, local_ref(spec, r), active);
        }
    }
    let mut result = object(&schema["properties"]);
    if let Some(r) = reference {
        constrain(
            &mut result,
            properties(spec, local_ref(spec, r), active.clone()),
        );
    }
    for member in members(schema, "allOf") {
        constrain(&mut result, properties(spec, member, active.clone()));
    }
    for keyword in ["oneOf", "anyOf"] {
        let branches: Vec<_> = members(schema, keyword)
            .map(|m| properties(spec, m, active.clone()))
            .collect();
        if branches.is_empty() {
            continue;
        }
        let common = branches.iter().skip(1).fold(
            branches[0].keys().cloned().collect::<BTreeSet<_>>(),
            |keys, b| keys.into_iter().filter(|k| b.contains_key(k)).collect(),
        );
        let mut additional = Map::new();
        for name in common {
            let mut v = Map::new();
            v.insert(
                keyword.into(),
                Value::Array(branches.iter().map(|b| b[&name].clone()).collect()),
            );
            additional.insert(name, Value::Object(v));
        }
        constrain(&mut result, additional);
    }
    result
}
fn response_types_supported(spec: &Value, schema: &Value, mut active: HashSet<usize>) -> bool {
    let Some(obj) = schema.as_object() else {
        return false;
    };
    if !active.insert(schema as *const Value as usize) {
        return false;
    }
    if let Some(reference) = schema["$ref"].as_str() {
        let resolved = local_ref(spec, reference);
        if !resolved.as_object().is_some_and(|o| !o.is_empty())
            || !response_types_supported(spec, resolved, active.clone())
        {
            return false;
        }
        if !spec["openapi"].as_str().unwrap_or("").starts_with("3.1.") {
            return true;
        }
    }
    let supported = [
        "$ref",
        "type",
        "properties",
        "required",
        "additionalProperties",
        "allOf",
        "oneOf",
        "anyOf",
        "title",
        "description",
        "$comment",
        "example",
        "examples",
        "default",
        "deprecated",
        "readOnly",
        "writeOnly",
        "discriminator",
        "nullable",
    ];
    if obj
        .keys()
        .any(|k| !supported.contains(&k.as_str()) && !k.starts_with("x-"))
        || schema["additionalProperties"].is_object()
    {
        return false;
    }
    let raw = &schema["type"];
    if !raw.is_null() && raw != "object" && !strings(raw).contains("object") {
        return false;
    }
    ["allOf", "oneOf", "anyOf"]
        .into_iter()
        .flat_map(|k| members(schema, k))
        .all(|m| response_types_supported(spec, m, active.clone()))
}
fn schema_types(
    spec: &Value,
    schema: &Value,
    mut active: HashSet<usize>,
) -> Option<BTreeSet<String>> {
    let obj = schema.as_object()?;
    if !active.insert(schema as *const Value as usize) {
        return None;
    }
    if let Some(reference) = schema["$ref"].as_str() {
        if obj.keys().any(|k| {
            ![
                "$ref",
                "title",
                "description",
                "$comment",
                "example",
                "examples",
                "deprecated",
            ]
            .contains(&k.as_str())
        }) {
            return None;
        }
        return schema_types(spec, local_ref(spec, reference), active);
    }
    if ["not", "if", "then", "else", "enum", "const"]
        .iter()
        .any(|k| obj.contains_key(*k))
    {
        return None;
    }
    let mut types = match schema["type"].as_str() {
        Some(t) => BTreeSet::from([t.to_owned()]),
        None => strings(&schema["type"]),
    };
    if types.iter().any(|t| {
        ![
            "null", "boolean", "object", "array", "number", "integer", "string",
        ]
        .contains(&t.as_str())
    }) {
        return None;
    }
    if types.contains("number") {
        types.insert("integer".into());
    }
    if !types.is_empty()
        && schema["nullable"] == true
        && spec["openapi"].as_str().unwrap_or("").starts_with("3.0.")
    {
        types.insert("null".into());
    }
    let mut result = (!types.is_empty()).then_some(types);
    for keyword in ["allOf", "oneOf", "anyOf"] {
        let branches: Vec<_> = members(schema, keyword)
            .map(|m| schema_types(spec, m, active.clone()))
            .collect();
        if branches.is_empty() {
            continue;
        }
        let branches = branches.into_iter().collect::<Option<Vec<_>>>()?;
        if keyword == "oneOf"
            && (0..branches.len()).any(|i| {
                branches
                    .iter()
                    .skip(i + 1)
                    .any(|r| !branches[i].is_disjoint(r))
            })
        {
            return None;
        }
        let combined = branches.iter().skip(1).fold(branches[0].clone(), |a, b| {
            if keyword == "allOf" {
                a.intersection(b).cloned().collect()
            } else {
                a.union(b).cloned().collect()
            }
        });
        result = Some(match result {
            Some(r) => r.intersection(&combined).cloned().collect(),
            None => combined,
        });
    }
    result
}
fn display_types(types: &BTreeSet<String>) -> String {
    types
        .iter()
        .filter(|t| t.as_str() != "integer" || !types.contains("number"))
        .cloned()
        .collect::<Vec<_>>()
        .join("|")
}
fn response_violations(
    base: &Value,
    current: &Value,
    a: &Value,
    b: &Value,
    label: &str,
) -> Vec<String> {
    let mut result = vec![];
    let ac = object(&a["content"]);
    let bc = object(&b["content"]);
    for (media, av) in ac {
        let Some(bv) = bc.get(&media) else {
            result.push(format!("response media type lost: {label} media '{media}'"));
            continue;
        };
        let ap = properties(base, &av["schema"], HashSet::new());
        let bp = properties(current, &bv["schema"], HashSet::new());
        let ak: BTreeSet<_> = ap.keys().cloned().collect();
        let bk: BTreeSet<_> = bp.keys().cloned().collect();
        for field in ak.difference(&bk) {
            result.push(format!(
                "response field lost: {label} media '{media}' field '{field}'"
            ));
        }
        if !response_types_supported(base, &av["schema"], HashSet::new())
            || !response_types_supported(current, &bv["schema"], HashSet::new())
        {
            continue;
        }
        for field in ak.intersection(&bk) {
            if let (Some(at), Some(bt)) = (
                schema_types(base, &ap[field], HashSet::new()),
                schema_types(current, &bp[field], HashSet::new()),
            ) {
                if !at.is_empty() && !bt.is_empty() && !bt.is_subset(&at) {
                    result.push(format!("response field type expanded: {label} media '{media}' field '{field}' '{}' -> '{}'",display_types(&at),display_types(&bt)));
                }
            }
        }
    }
    result
}
fn text(v: &Value) -> String {
    v.as_str().map(str::to_owned).unwrap_or_else(|| {
        if v.is_null() {
            "None".into()
        } else {
            v.to_string()
        }
    })
}
fn repr(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        _ => v.to_string(),
    }
}
/// Compatible growth is permitted; checks are intentionally bounded as in source.
pub fn compare_specs(base: &Value, current: &Value) -> Vec<String> {
    let mut result = vec![];
    let cp = object(&current["paths"]);
    for (path, item) in object(&base["paths"]) {
        let Some(ci) = cp.get(&path) else {
            result.push(format!("path removed: {path}"));
            continue;
        };
        for (method, op) in object(&item) {
            if !METHODS.contains(&method.as_str()) {
                continue;
            }
            let label = format!("{} {path}", method.to_uppercase());
            let Some(co) = ci.get(&method) else {
                result.push(format!("method removed: {label}"));
                continue;
            };
            if !op.is_object() {
                continue;
            }
            let empty = json!([]);
            let security = op
                .get("security")
                .or_else(|| base.get("security"))
                .unwrap_or(&empty);
            let cs = co
                .get("security")
                .or_else(|| current.get("security"))
                .unwrap_or(&empty);
            if security != cs {
                result.push(format!("security requirements changed: {label}"));
            }
            if op["operationId"].as_str().is_some_and(|s| !s.is_empty())
                && op["operationId"] != co["operationId"]
            {
                result.push(format!(
                    "operationId changed: {label} '{}' -> '{}'",
                    text(&op["operationId"]),
                    text(&co["operationId"])
                ));
            }
            for field in request_required(current, co).difference(&request_required(base, &op)) {
                result.push(format!(
                    "new required request field: {label} field '{field}'"
                ));
            }
            if op["requestBody"]["required"] != true && co["requestBody"]["required"] == true {
                result.push(format!("request body became required: {label}"));
            }
            let am = object(&op["requestBody"]["content"]);
            let bm = object(&co["requestBody"]["content"]);
            for media in am.keys().filter(|k| !bm.contains_key(*k)) {
                result.push(format!("request media type lost: {label} media '{media}'"));
            }
            let ap = parameters(base, &item, &op);
            let bp = parameters(current, ci, co);
            for (name, location) in ap.keys().filter(|k| !bp.contains_key(*k)) {
                result.push(format!("parameter removed: {label} {location} '{name}'"));
            }
            for ((name, location), p) in &bp {
                let old = ap.get(&(name.clone(), location.clone()));
                if p["required"] == true && old.is_none_or(|o| o["required"] != true) {
                    result.push(format!(
                        "new required parameter: {label} {location} '{name}'"
                    ));
                }
                let Some(old) = old else {
                    continue;
                };
                let at = &old["schema"]["type"];
                let bt = &p["schema"]["type"];
                if !at.is_null() && at != bt {
                    result.push(format!(
                        "parameter type changed: {label} {location} '{name}' '{}' -> '{}'",
                        text(at),
                        text(bt)
                    ));
                }
                let be: Vec<_> = members(&p["schema"], "enum").collect();
                let mut lost: Vec<_> = members(&old["schema"], "enum")
                    .filter(|v| !be.contains(v))
                    .collect();
                lost.sort_by_key(|v| repr(v));
                for v in lost {
                    result.push(format!(
                        "parameter enum value lost: {label} {location} '{name}' value {}",
                        repr(v)
                    ));
                }
            }
            let ar = object(&op["responses"]);
            let br = object(&co["responses"]);
            let ak: BTreeSet<_> = ar.keys().cloned().collect();
            let bk: BTreeSet<_> = br.keys().cloned().collect();
            for status in ak.difference(&bk) {
                result.push(format!("response status lost: {label} status '{status}'"));
            }
            for status in ak.intersection(&bk) {
                result.extend(response_violations(
                    base,
                    current,
                    &ar[status],
                    &br[status],
                    &format!("{label} status '{status}'"),
                ));
            }
        }
    }
    let a = object(&base["components"]["securitySchemes"]);
    let b = object(&current["components"]["securitySchemes"]);
    let ak: BTreeSet<_> = a.keys().cloned().collect();
    let bk: BTreeSet<_> = b.keys().cloned().collect();
    for name in ak.difference(&bk) {
        result.push(format!("securityScheme removed: {name}"));
    }
    for name in ak.intersection(&bk) {
        if a[name] != b[name] {
            result.push(format!("securityScheme changed: {name}"));
        }
    }
    result
}
pub fn operation_ids(spec: &Value) -> BTreeMap<String, Value> {
    let mut result = BTreeMap::new();
    for (path, item) in object(&spec["paths"]) {
        for (method, op) in object(&item) {
            if METHODS.contains(&method.as_str()) && op.is_object() {
                result.insert(
                    format!("{} {path}", method.to_uppercase()),
                    op["operationId"].clone(),
                );
            }
        }
    }
    result
}
pub fn operation_id_drift(
    expected: &BTreeMap<String, Value>,
    observed: &BTreeMap<String, Value>,
) -> Vec<String> {
    let keys: BTreeSet<_> = expected.keys().chain(observed.keys()).collect();
    keys.into_iter()
        .filter(|k| expected.get(*k) != observed.get(*k))
        .map(|k| {
            format!(
                "operationId is process-dependent: {k} '{}' -> '{}'",
                text(expected.get(k).unwrap_or(&Value::Null)),
                text(observed.get(k).unwrap_or(&Value::Null))
            )
        })
        .collect()
}
pub fn stats(spec: &Value) -> Value {
    json!({"paths":spec["paths"].as_object().map_or(0,Map::len),"operations":operation_ids(spec).len()})
}
