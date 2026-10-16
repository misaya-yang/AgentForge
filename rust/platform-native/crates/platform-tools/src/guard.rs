//! Live topology inventory and independently pinned singleton Helm contract.
use crate::*;
use std::collections::BTreeSet;
const SINGLE: [(&str, &str); 2] = [("gateway", "gateway"), ("agent-runtime", "agentRuntime")];
pub fn generate(args: &Args) -> Result<Value> {
    let root = args.root();
    let manifest = topology::manifest(&root, args)?;
    let groups: BTreeSet<_> = manifest["groups"]
        .as_array()
        .ok_or("groups missing")?
        .iter()
        .filter_map(|v| v["id"].as_str())
        .collect();
    let required = [
        "service_id",
        "display_name",
        "bounded_context",
        "process_role",
        "lifecycle",
        "exposure",
        "required_deps",
        "optional_deps",
        "state_owner",
        "scale_support",
        "health_contract",
        "image_artifact",
        "modes",
    ];
    let mut services = vec![];
    let mut ids = BTreeSet::new();
    for record in manifest["services"].as_array().unwrap() {
        let fields = record.as_object().ok_or("service must be object")?;
        require(
            fields.len() == required.len() && required.iter().all(|k| fields.contains_key(*k)),
            "exact topology field contract required",
        )?;
        require(
            record["bounded_context"]
                .as_str()
                .is_some_and(|g| groups.contains(g)),
            "unknown bounded context",
        )?;
        let mut record = record.clone();
        let class = record["scale_support"]
            .as_str()
            .ok_or("scale support missing")?;
        let mut contract = json!({"class":class,"max_replicas":if class=="single-instance"{Some(1)}else{None},"hpa_allowed":(["stateless","worker"].contains(&class))});
        let id = record["service_id"].as_str().unwrap().to_owned();
        if let Some((_, key)) = SINGLE.iter().find(|(s, _)| *s == id) {
            contract["helm_values_key"] = json!(key);
        }
        record["scale_contract"] = contract;
        ids.insert(id);
        services.push(record);
    }
    let mut compose = BTreeMap::new();
    for name in [
        "docker-compose.yml",
        "deploy/compose/docker-compose.compact.yml",
        "deploy/compose/docker-compose.full.yml",
        "deploy/compose/docker-compose.scale.yml",
        "deploy/compose/docker-compose.dev.yml",
        "deploy/compose/docker-compose.build.yml",
    ] {
        let path = root.join(name);
        let mut names = vec![];
        if path.is_file() {
            let value: Value = serde_yaml::from_str(&std::fs::read_to_string(&path)?)?;
            if let Some(map) = value["services"].as_object() {
                names.extend(map.keys().cloned());
            }
            if name == "docker-compose.yml" {
                require(
                    names.iter().all(|s| ids.contains(s)),
                    "base Compose services absent from topology",
                )?;
            }
        }
        names.sort();
        compose.insert(name, json!({"exists":path.is_file(),"services":names}));
    }
    let snapshot = root.join("sdk/openapi.json");
    let gateway = if snapshot.is_file() {
        let spec = object(&snapshot)?;
        let paths = spec["paths"].as_object().ok_or("OpenAPI paths missing")?;
        let operations = paths
            .values()
            .filter_map(Value::as_object)
            .map(|methods| {
                methods
                    .iter()
                    .filter(|(name, v)| {
                        [
                            "get",
                            "post",
                            "put",
                            "delete",
                            "patch",
                            "options",
                            "head",
                            "websocket",
                        ]
                        .contains(&name.as_str())
                            && v.is_object()
                    })
                    .count()
            })
            .sum::<usize>();
        json!({"available":true,"source":"sdk/openapi.json","sha256":digest(&snapshot)?,"path_count":paths.len(),"operation_count":operations})
    } else {
        json!({"available":false,"source":"sdk/openapi.json"})
    };
    let knowledge_root = root.join("rust/platform-native/crates/platform-knowledge-service/src");
    let mut route_files = 0;
    let mut registrations = 0;
    let route = regex::Regex::new(r"\.route\s*\(")?;
    for path in walk(&knowledge_root, &[], false)? {
        if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(knowledge_root.join(path))?;
            let count = route.find_iter(&text).count();
            if count > 0 {
                route_files += 1;
                registrations += count;
            }
        }
    }
    let identity = if let Some(sha) = args.values.get("--source-sha") {
        require(
            sha.len() == 40
                && sha
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "invalid source SHA",
        )?;
        sha.clone()
    } else {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["rev-parse", "HEAD"])
            .output()?;
        require(
            output.status.success(),
            "Git source identity unavailable; explicit --source-sha required for restored checkout",
        )?;
        String::from_utf8(output.stdout)?.trim().into()
    };
    let path = args
        .values
        .get("--manifest")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("src/core/data/service_topology.json"));
    let value = json!({"schema":"ai-gateway/baseline/service-topology/v1","baseline_id":"2026-08-post-rag","base_git_sha":identity,"generator":"platform-tools topology-inventory","topology_schema":manifest["schema"],"topology_revision":manifest["revision"],"default_mode":manifest["default_mode"],"groups":manifest["groups"],"services":services,"compose_modes":manifest["compose_modes"],"compose_files":compose,"gateway_routes":gateway,"knowledge_routes":{"route_file_count":route_files,"route_registration_count":registrations,"source_language":"rust","method":"static-route-call-count"},"manifest_sha256":digest(&path)?});
    if let Some(path) = args.values.get("--output") {
        write_json(Path::new(path), &value)?;
    }
    Ok(value)
}
fn mentions(document: &str, service: &str) -> bool {
    let escaped = regex::escape(service);
    regex::Regex::new(&format!(
        r"(?m)^\s*app\.kubernetes\.io/component:\s*{escaped}\s*$|^\s*name:\s*[^\n]*-{escaped}\s*$"
    ))
    .unwrap()
    .is_match(&document.to_lowercase())
}
fn inspect_document(
    document: &str,
    counts: &mut BTreeMap<String, usize>,
    failures: &mut Vec<String>,
) {
    let kind = regex::Regex::new(r"(?m)^kind:\s*([A-Za-z0-9]+)\s*$").unwrap();
    let Some(kind) = kind.captures(document) else {
        return;
    };
    for (service, _) in SINGLE {
        if !mentions(document, service) {
            continue;
        }
        if &kind[1] == "HorizontalPodAutoscaler" {
            failures.push(format!("{service} must not render HPA"));
        }
        if &kind[1] != "Deployment" {
            continue;
        }
        *counts.entry(service.into()).or_default() += 1;
        let replicas = regex::Regex::new(r"(?m)^\s*replicas:\s*([^#\n]+?)\s*$")
            .unwrap()
            .captures_iter(document)
            .map(|c| c[1].to_owned())
            .collect::<Vec<_>>();
        if replicas != ["1"] {
            failures.push(format!("{service} deployment requires literal replicas 1"));
        }
        if !regex::Regex::new(r"(?ms)^\s*strategy:\s*$.*?^\s*type:\s*Recreate\s*$")
            .unwrap()
            .is_match(document)
        {
            failures.push(format!("{service} deployment requires Recreate"));
        }
    }
}
pub fn run(args: &Args) -> Result<Value> {
    let root = args.root();
    if args.flag("--selftest") {
        let mut counts = BTreeMap::new();
        let mut failures = vec![];
        inspect_document(
            "kind: Deployment\nmetadata:\n  name: example-gateway\nspec:\n  replicas: 2\n",
            &mut counts,
            &mut failures,
        );
        require(failures.len() == 2, "singleton guard selftest failed")?;
        return Ok(json!({"result":"pass","selftest":"single-instance-guard"}));
    }
    let generated = generate(args)?;
    let mut failures = vec![];
    let services = generated["services"].as_array().unwrap();
    for (service, key) in SINGLE {
        let row = services
            .iter()
            .find(|v| v["service_id"] == service)
            .ok_or("protected service missing")?;
        let c = &row["scale_contract"];
        if c["class"] != "single-instance"
            || c["max_replicas"] != 1
            || c["hpa_allowed"] != false
            || c["helm_values_key"] != key
        {
            failures.push(format!("{service} topology single-instance contract drift"));
        }
    }
    for row in services {
        let c = &row["scale_contract"];
        if ["stateless", "worker"].iter().any(|s| c["class"] == *s) && c["hpa_allowed"] != true {
            failures.push("scalable class must allow HPA".into());
        }
    }
    // serde_json::Value's visitor overwrites duplicate YAML keys. Parse the
    // YAML mapping first so conflicting scale scalars fail closed.
    let read_values = |relative: &str| -> Result<Value> {
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(root.join(relative))?)?;
        Ok(serde_json::to_value(yaml)?)
    };
    let defaults = read_values("deploy/helm/ai-gateway/values.yaml")?;
    let production = read_values("deploy/helm/ai-gateway/values-production.yaml")?;
    let effective = topology::merge(&defaults, &production);
    let mut facts = BTreeMap::new();
    for (service, key) in SINGLE {
        let present = defaults.get(key).is_some();
        let mut fact = json!({"helm_values_key":key,"present":present});
        if service == "gateway" && !present {
            failures.push("gateway defaults block missing".into());
        }
        if present {
            for (label, values) in [("defaults", &defaults), ("production", &effective)] {
                let c = &values[key];
                let observed = json!({"replicaCount":c["replicaCount"],"autoscaling.enabled":c["autoscaling"]["enabled"],"autoscaling.minReplicas":c["autoscaling"]["minReplicas"],"autoscaling.maxReplicas":c["autoscaling"]["maxReplicas"]});
                if c["replicaCount"] != 1
                    || c["autoscaling"]["enabled"] != false
                    || c["autoscaling"]["minReplicas"] != 1
                    || c["autoscaling"]["maxReplicas"] != 1
                {
                    failures.push(format!(
                        "{service} {label} must pin replicas/min/max 1 and autoscaling false"
                    ));
                }
                fact[label] = observed;
            }
        }
        facts.insert(service, fact);
    }
    let directory = root.join("deploy/helm/ai-gateway/templates");
    let mut scanned = vec![];
    let mut counts = BTreeMap::new();
    let split = regex::Regex::new(r"(?m)^---\s*$")?;
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if !path.extension().is_some_and(|e| e == "yaml" || e == "yml") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        scanned.push(path.strip_prefix(&root)?.to_string_lossy().into_owned());
        for (_, key) in SINGLE {
            if text.contains(&format!(".Values.{key}.replicaCount"))
                || text.contains(&format!(".Values.{key}.autoscaling"))
            {
                failures.push(format!("template must not honor {key} scaling values"));
            }
        }
        for document in split.split(&text) {
            inspect_document(document, &mut counts, &mut failures);
        }
    }
    if counts.get("gateway") != Some(&1) {
        failures.push("gateway requires exactly one protected Helm Deployment".into());
    }
    scanned.sort();
    let value = json!({"gate":"single-instance-guard","tier":"L0-static","protected_services":["agent-runtime","gateway"],"values":facts,"templates_scanned":scanned,"failures":failures,"result":if failures.is_empty(){"pass"}else{"fail"}});
    write_json(
        &args
            .values
            .get("--evidence-out")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("tmp/gate-evidence/single-instance-guard.json")),
        &value,
    )?;
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn component_suffix_does_not_confuse_frontend() {
        assert!(!mentions(
            "metadata:\n  name: ai-gateway-frontend\n",
            "gateway"
        ));
        assert!(mentions("metadata:\n  name: app-gateway\n", "gateway"));
    }
    #[test]
    fn literal_singleton_recreate_passes() {
        let mut c = BTreeMap::new();
        let mut f = vec![];
        inspect_document(
            "kind: Deployment\nmetadata:\n  name: app-gateway\nspec:\n  replicas: 1\n  strategy:\n    type: Recreate\n",
            &mut c,
            &mut f,
        );
        assert!(f.is_empty());
        assert_eq!(c["gateway"], 1);
    }
}
