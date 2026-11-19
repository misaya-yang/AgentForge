//! Source freshness gate for the two deliberate Compose materializations.
//! No Compose execution, interpolation, env file or credential read is performed.
use serde_json::{Map, Value};
use std::{fs, path::Path};
fn normalize(v: serde_yaml::Value) -> Result<Value, String> {
    use serde_yaml::Value as Y;
    Ok(match v {
        Y::Tagged(t) => normalize(t.value)?,
        Y::Null => Value::Null,
        Y::Bool(b) => b.into(),
        Y::String(s) => s.into(),
        Y::Number(n) => serde_json::from_str(&n.to_string()).map_err(|_| "compose number")?,
        Y::Sequence(xs) => Value::Array(xs.into_iter().map(normalize).collect::<Result<_, _>>()?),
        Y::Mapping(xs) => {
            let mut out = Map::new();
            for (k, v) in xs {
                out.insert(
                    k.as_str().ok_or("compose mapping key")?.into(),
                    normalize(v)?,
                );
            }
            Value::Object(out)
        }
    })
}
fn resolve_merges(value: &mut serde_yaml::Value) -> Result<(), serde_yaml::Error> {
    // Resolve merge sources first: apply_merge visits each map once, so an
    // inherited map containing its own << must be flattened before insertion.
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            for value in mapping.values_mut() {
                resolve_merges(value)?;
            }
        }
        serde_yaml::Value::Sequence(sequence) => {
            for value in sequence {
                resolve_merges(value)?;
            }
        }
        serde_yaml::Value::Tagged(tagged) => resolve_merges(&mut tagged.value)?,
        _ => {}
    }
    value.apply_merge()
}
fn parse(source: &str) -> Result<Value, String> {
    let mut parsed: serde_yaml::Value =
        serde_yaml::from_str(source).map_err(|_| "compose source YAML")?;
    resolve_merges(&mut parsed).map_err(|_| "compose YAML merge")?;
    normalize(parsed)
}
fn read(repo: &Path, path: &str) -> Result<Value, String> {
    parse(&fs::read_to_string(repo.join(path)).map_err(|_| "compose source missing")?)
}

fn merge(a: &mut Value, b: &Value, lists: bool, field: &str) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                if let Some(current) = a.get_mut(k) {
                    merge(current, v, lists, k)
                } else {
                    a.insert(k.clone(), v.clone());
                }
            }
        }
        (Value::Array(a), Value::Array(b))
            if field == "volumes" && b.iter().all(|m| m.get("target").is_some()) =>
        {
            for v in b {
                if let Some(i) = a.iter().position(|m| m["target"] == v["target"]) {
                    a[i] = v.clone()
                } else {
                    a.push(v.clone())
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if lists => {
            for v in b {
                if !a.contains(v) {
                    a.push(v.clone())
                }
            }
        }
        (a, b) => *a = b.clone(),
    }
}
fn public(mut v: Value) -> Value {
    if let Some(m) = v.as_object_mut() {
        m.retain(|k, _| !k.starts_with("x-"));
    }
    v
}
fn difference_path(expected: &Value, actual: &Value, path: &str) -> Option<String> {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            for key in expected.keys().chain(actual.keys()) {
                let child = format!("{path}.{key}");
                match (expected.get(key), actual.get(key)) {
                    (Some(expected), Some(actual)) => {
                        if let Some(path) = difference_path(expected, actual, &child) {
                            return Some(path);
                        }
                    }
                    _ => return Some(child),
                }
            }
            None
        }
        (Value::Array(expected), Value::Array(actual)) => {
            for index in 0..expected.len().max(actual.len()) {
                let child = format!("{path}[{index}]");
                match (expected.get(index), actual.get(index)) {
                    (Some(expected), Some(actual)) => {
                        if let Some(path) = difference_path(expected, actual, &child) {
                            return Some(path);
                        }
                    }
                    _ => return Some(child),
                }
            }
            None
        }
        _ => (expected != actual).then(|| path.to_owned()),
    }
}
pub fn check(repo: &Path) -> Result<(), String> {
    let mut security = read(repo, "deploy/compose/docker-compose.workload-security.yml")?;
    merge(
        &mut security,
        &read(repo, "deploy/compose/docker-compose.data-security.yml")?,
        false,
        "",
    );
    if security != read(repo, "deploy/compose/docker-compose.security.yml")? {
        return Err("canonical security Compose is stale".into());
    }
    let root = read(repo, "docker-compose.yml")?;
    let mut apps = public(read(repo, "deploy/local/compose.apps.yml")?);
    let services = apps["services"]
        .as_object_mut()
        .ok_or("local app services")?;
    for (name, decision) in services.iter_mut() {
        if decision.get("extends").is_some() {
            let mut delta = decision.clone();
            delta.as_object_mut().unwrap().remove("extends");
            let reset_name = delta.get("container_name").is_some_and(Value::is_null);
            let mut expanded = root["services"][name.as_str()].clone();
            // Local volumes/ports explicitly replace root lists (!override).
            for k in ["volumes", "ports"] {
                if delta.get(k).is_some() {
                    expanded
                        .as_object_mut()
                        .ok_or("root app service")?
                        .remove(k);
                }
            }
            merge(&mut expanded, &delta, false, "");
            if reset_name {
                expanded.as_object_mut().unwrap().remove("container_name");
            }
            *decision = expanded;
        }
    }
    let mut full = public(read(repo, "deploy/local/compose.yml")?);
    merge(
        &mut full,
        &public(read(repo, "deploy/local/compose.runtime.yml")?),
        true,
        "",
    );
    merge(&mut full, &apps, true, "");
    let materialized = read(repo, "deploy/local/compose.full.yml")?;
    if let Some(path) = difference_path(&full, &materialized, "$") {
        return Err(format!("canonical local full Compose is stale at {path}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nested_anchor_environments_equal_the_flat_materialization() {
        let nested = parse(
            "base: &base {PROXY: localhost, ROLE: api}\n\
             worker: &worker {<<: *base, ROLE: worker}\n\
             service: {environment: {<<: *worker, STORAGE: local}}\n",
        )
        .unwrap();
        let flat = parse("environment: {PROXY: localhost, ROLE: worker, STORAGE: local}").unwrap();
        assert_eq!(nested["service"], flat);
    }

    #[test]
    fn compose_override_and_reset_tags_preserve_values_after_nested_merges() {
        let parsed = parse(concat!(
            "base: &base {source: old, target: /data}\n",
            "service:\n",
            "  volumes: !override [{<<: *base, source: new}]\n",
            "  ports: !override [8080]\n",
            "  container_name: !reset null\n",
        ))
        .unwrap();
        assert_eq!(
            parsed["service"]["volumes"],
            json!([{"source":"new","target":"/data"}])
        );
        assert_eq!(parsed["service"]["ports"], json!([8080]));
        assert!(parsed["service"]["container_name"].is_null());
    }
}
