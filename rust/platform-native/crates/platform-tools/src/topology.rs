//! Native ARC-06 topology validation. Output/exit contract mirrors the deploy helper.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};
const MODES: &[&str] = &["compact", "full", "scale"];
pub(crate) fn merge(base: &Value, overlay: &Value) -> Value {
    if let (Some(base), Some(overlay)) = (base.as_object(), overlay.as_object()) {
        let mut result = base.clone();
        for (k, v) in overlay {
            result.insert(
                k.clone(),
                result
                    .get(k)
                    .map(|old| merge(old, v))
                    .unwrap_or_else(|| v.clone()),
            );
        }
        Value::Object(result)
    } else {
        overlay.clone()
    }
}
pub(crate) fn manifest(root: &Path, args: &Args) -> Result<Value> {
    let path = args
        .values
        .get("--manifest")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("src/core/data/service_topology.json"));
    validate_manifest(object(&path)?)
}
pub(crate) fn validate_manifest(m: Value) -> Result<Value> {
    require(
        m["schema"] == "ai-gateway/service-topology/v1" && m["default_mode"] == "full",
        "topology schema/default invalid",
    )?;
    let rows = m["services"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("topology services missing")?;
    let mut ids = BTreeSet::new();
    for row in rows {
        let id = row["service_id"].as_str().ok_or("invalid service id")?;
        require(ids.insert(id), "duplicate topology service")?;
        let modes = row["modes"].as_object().ok_or("service modes missing")?;
        require(
            modes.len() == 3 && MODES.iter().all(|s| modes.contains_key(*s)),
            "service must resolve all modes",
        )?;
        for resolution in modes.values() {
            let n = resolution["replicas"].as_u64().ok_or("invalid replicas")?;
            require(
                resolution["present"].as_bool() == Some(n > 0),
                "service presence/replicas mismatch",
            )?;
        }
    }
    let by_id: BTreeMap<_, _> = rows
        .iter()
        .map(|r| (r["service_id"].as_str().unwrap(), r))
        .collect();
    for row in rows {
        for key in ["required_deps", "optional_deps"] {
            if let Some(deps) = row[key].as_array() {
                for dependency in deps {
                    require(
                        dependency.as_str().is_some_and(|s| ids.contains(s)),
                        "unknown service dependency",
                    )?;
                }
            }
        }
    }
    for singleton in ["gateway", "agent-runtime"] {
        let r = by_id.get(singleton).ok_or("required singleton missing")?;
        require(
            r["scale_support"] == "single-instance"
                && MODES.iter().all(|mode| r["modes"][*mode]["replicas"] == 1),
            "singleton replication forbidden",
        )?;
    }
    require(
        by_id
            .get("knowledge-service")
            .is_some_and(|r| r["modes"]["compact"]["runtime_role"] == "all")
            && by_id
                .get("knowledge-worker")
                .is_some_and(|r| r["modes"]["compact"]["present"] == false),
        "compact Knowledge contract invalid",
    )?;
    for worker in ["knowledge-worker", "agent-capability-worker"] {
        require(
            by_id.get(worker).is_some_and(|r| {
                r["scale_support"] == "worker"
                    && r["modes"]["scale"]["replicas"]
                        .as_u64()
                        .is_some_and(|n| n >= 2)
            }),
            "worker scale contract invalid",
        )?;
    }
    for job in ["gateway-init", "migrate"] {
        require(
            by_id.get(job).is_some_and(|r| r["lifecycle"] == "one-shot"),
            "init job must be one-shot",
        )?;
    }
    Ok(m)
}
fn resolve(root: &Path, m: &Value, mode: &str) -> Result<Value> {
    require(MODES.contains(&mode), "unknown topology mode")?;
    let overlay_path = format!("deploy/compose/docker-compose.{mode}.yml");
    let expected = json!(["docker-compose.yml", overlay_path]);
    require(
        m["compose_modes"][mode]["files"] == expected,
        "mode must name base and exact overlay",
    )?;
    let read = |file: &str| -> Result<Value> {
        let v: Value = serde_yaml::from_slice(&std::fs::read(root.join(file))?)?;
        require(v["services"].is_object(), "Compose services missing")?;
        Ok(v["services"].clone())
    };
    let base = read("docker-compose.yml")?;
    let overlay = read(&overlay_path)?;
    let base = base.as_object().unwrap();
    let overlay = overlay.as_object().unwrap();
    require(
        overlay.keys().all(|key| base.contains_key(key)),
        "overlay declares unknown services",
    )?;
    let rows = m["services"].as_array().unwrap();
    let ids: BTreeSet<_> = rows
        .iter()
        .map(|r| r["service_id"].as_str().unwrap())
        .collect();
    require(
        base.keys().all(|key| ids.contains(key.as_str())),
        "Compose service absent from topology",
    )?;
    let merged: BTreeMap<_, _> = base
        .iter()
        .map(|(key, v)| {
            (
                key.clone(),
                overlay
                    .get(key)
                    .map(|o| merge(v, o))
                    .unwrap_or_else(|| v.clone()),
            )
        })
        .collect();
    require(
        merged
            .get("gateway")
            .is_some_and(|s| s["environment"]["AI_PLATFORM_TOPOLOGY_MODE"] == mode),
        "Gateway mode stamp mismatch",
    )?;
    let mut active = vec![];
    for contract in rows {
        let id = contract["service_id"].as_str().unwrap();
        let resolution = &contract["modes"][mode];
        let present = resolution["present"] == true;
        if let Some(service) = merged.get(id) {
            let profiles = service["profiles"]
                .as_array()
                .is_some_and(|a| !a.is_empty());
            if !present {
                require(profiles, format!("{id}:{mode} must be disabled by profile"))?;
                continue;
            }
            require(!profiles, format!("{id}:{mode} active behind profile"))?;
            let replicas = service["deploy"]
                .get("replicas")
                .map(|v| v.as_u64())
                .unwrap_or(Some(1))
                .filter(|n| *n > 0)
                .ok_or("invalid Compose replicas")?;
            require(
                Some(replicas) == resolution["replicas"].as_u64(),
                format!("{id}:{mode} replica mismatch"),
            )?;
            if let Some(role) = resolution.get("runtime_role") {
                require(
                    service["environment"]["KNOWLEDGE_RUNTIME_ROLE"] == *role,
                    "Knowledge runtime role mismatch",
                )?;
            }
            if replicas > 1 && contract["scale_support"] == "worker" {
                require(
                    service["container_name"].is_null(),
                    "scaled worker has fixed container_name",
                )?;
            }
            if contract["lifecycle"] == "one-shot" {
                require(service["restart"] == "no", "one-shot restart must be no")?;
            }
        } else {
            require(
                !present || contract["lifecycle"] == "external",
                format!("{id}:{mode} missing from Compose"),
            )?;
        }
        if present {
            let mut row = json!({"service_id":id,"bounded_context":contract["bounded_context"],"lifecycle":contract["lifecycle"],"scale_support":contract["scale_support"],"replicas":resolution["replicas"]});
            if let Some(role) = resolution.get("runtime_role") {
                row["runtime_role"] = role.clone();
            }
            active.push(row);
        }
    }
    active.sort_by(|a, b| a["service_id"].as_str().cmp(&b["service_id"].as_str()));
    Ok(
        json!({"schema":"ai-gateway/topology-resolution/v1","mode":mode,"topology_revision":m["revision"],"compose_files":expected,"services":active}),
    )
}
pub fn run(args: &Args) -> Result<Value> {
    let root = args.root();
    let m = manifest(&root, args)?;
    let mode = args
        .values
        .get("--mode")
        .map(String::as_str)
        .unwrap_or("full");
    if let Some(scope) = args.values.get("--service-ids") {
        require(
            ["all", "app", "infrastructure"].contains(&scope.as_str()),
            "invalid service scope",
        )?;
        let r = resolve(&root, &m, mode)?;
        let ids: Vec<_> = r["services"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| {
                let infrastructure = row["bounded_context"] == "infrastructure"
                    && row["lifecycle"] == "long-running";
                match scope.as_str() {
                    "infrastructure" => infrastructure,
                    "app" => !infrastructure && row["service_id"] != "gateway-init",
                    _ => true,
                }
            })
            .map(|row| row["service_id"].as_str().unwrap())
            .collect();
        return Ok(json!({"_plain":ids.join(" ")}));
    }
    if let Some(id) = args.values.get("--service-replicas") {
        require(
            m["services"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["service_id"] == *id),
            "unknown service",
        )?;
        let r = resolve(&root, &m, mode)?;
        let n = r["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["service_id"] == *id)
            .and_then(|r| r["replicas"].as_u64())
            .unwrap_or(0);
        return Ok(json!({"_plain":n.to_string()}));
    }
    if args.flag("--check-all") {
        let result: Vec<_> = MODES
            .iter()
            .map(|mode| resolve(&root, &m, mode))
            .collect::<Result<_>>()?;
        Ok(json!(result))
    } else {
        resolve(&root, &m, mode)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deep_merge_replaces_sequences() {
        assert_eq!(
            merge(
                &json!({"a":{"b":1,"c":2},"x":[1]}),
                &json!({"a":{"b":3},"x":[2]})
            ),
            json!({"a":{"b":3,"c":2},"x":[2]})
        );
    }
}
