use super::*;
const COMPOSE: &[&str] = &[
    "docker-compose.yml",
    "deploy/compose/docker-compose.dev.yml",
    "deploy/compose/docker-compose.build.yml",
    "deploy/compose/docker-compose.kbms.yml",
    "deploy/compose/docker-compose.capability.yml",
];
fn file(s: &Snapshot, path: &str) -> Value {
    match s.files.get(path) {
        Some(b) => json!({"path":path,"exists":true,"bytes":b.len(),"sha256":hash(b)}),
        None => json!({"path":path,"exists":false}),
    }
}
fn compose(s: &Snapshot, paths: &[&str]) -> Result<Value> {
    let mut files = BTreeMap::new();
    for path in paths {
        if let Some(bytes) = s.files.get(*path) {
            let doc: Value = serde_yaml::from_slice(bytes)?;
            let mut services = BTreeMap::new();
            if let Some(rows) = doc["services"].as_object() {
                for (name, v) in rows {
                    services.insert(name.clone(),json!({"image":v["image"],"container_name":v["container_name"],"build":v["build"]}));
                }
            }
            files.insert(*path, services);
        } else {
            files.insert(*path, BTreeMap::new());
        }
    }
    Ok(json!(files))
}
pub(super) fn topology(s: &Snapshot) -> Result<Value> {
    let mut out = s.envelope("service-topology");
    let composition = compose(
        s,
        &[
            "docker-compose.yml",
            "deploy/compose/docker-compose.compact.yml",
            "deploy/compose/docker-compose.full.yml",
            "deploy/compose/docker-compose.scale.yml",
            "deploy/compose/docker-compose.dev.yml",
            "deploy/compose/docker-compose.build.yml",
        ],
    )?;
    let manifest = "src/core/data/service_topology.json";
    let declared = crate::topology::validate_manifest(serde_json::from_str(s.text(manifest)?)?)?;
    let services = declared["services"]
        .as_array()
        .ok_or("topology services missing")?;
    let fields: BTreeSet<_> = [
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
    ]
    .into_iter()
    .collect();
    let groups: BTreeSet<_> = declared["groups"]
        .as_array()
        .ok_or("topology groups missing")?
        .iter()
        .filter_map(|v| v["id"].as_str())
        .collect();
    for service in services {
        require(
            service
                .as_object()
                .ok_or("invalid topology row")?
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == fields,
            "topology service fields differ from exact contract",
        )?;
        require(
            service["bounded_context"]
                .as_str()
                .is_some_and(|g| groups.contains(g)),
            "unknown bounded context",
        )?;
    }
    let ids: BTreeSet<_> = services
        .iter()
        .filter_map(|v| v["service_id"].as_str())
        .collect();
    for name in composition["docker-compose.yml"]
        .as_object()
        .ok_or("base Compose missing")?
        .keys()
    {
        require(
            ids.contains(name.as_str()),
            format!("base Compose service absent from topology: {name}"),
        )?;
    }
    out["topology_schema"] = declared["schema"].clone();
    out["topology_revision"] = declared["revision"].clone();
    for key in ["default_mode", "groups", "compose_modes"] {
        out[key] = declared[key].clone();
    }
    out["manifest_sha256"] = json!(hash(
        s.files.get(manifest).ok_or("topology manifest missing")?
    ));
    out["services"] = json!(services.iter().map(|service| {
        let mut row = service.clone();
        let class = row["scale_support"].as_str().unwrap_or("");
        let mut scale = json!({"class":class,"max_replicas":if class=="single-instance"{Some(1)}else{None},"hpa_allowed":matches!(class,"stateless"|"worker")});
        match row["service_id"].as_str() {Some("gateway")=>scale["helm_values_key"]=json!("gateway"),Some("agent-runtime")=>scale["helm_values_key"]=json!("agentRuntime"),_=>{}}
        row["scale_contract"]=scale; row
    }).collect::<Vec<_>>());
    out["compose_files"] = json!(composition.as_object().unwrap().iter().map(|(path,services)|(path.clone(),json!({"exists":s.files.contains_key(path),"services":services.as_object().unwrap().keys().collect::<Vec<_>>()}))).collect::<BTreeMap<_,_>>());
    out["gateway_routes"] = if let Some(bytes) = s.files.get("sdk/openapi.json") {
        let spec: Value = serde_json::from_slice(bytes)?;
        let paths = spec["paths"].as_object().ok_or("OpenAPI paths missing")?;
        let methods = [
            "get",
            "post",
            "put",
            "delete",
            "patch",
            "options",
            "head",
            "websocket",
        ];
        json!({"available":true,"source":"sdk/openapi.json","sha256":hash(bytes),"path_count":paths.len(),"operation_count":paths.values().filter_map(Value::as_object).flat_map(|m|m.iter()).filter(|(method,op)|methods.contains(&method.as_str())&&op.is_object()).count()})
    } else {
        json!({"available":false,"source":"sdk/openapi.json"})
    };
    let mut routes = vec![];
    let pattern = regex::Regex::new(r#"\.route\s*\(\s*"([^"]+)""#)?;
    for (path, bytes) in &s.files {
        if !path.ends_with(".rs") {
            continue;
        }
        let text = std::str::from_utf8(bytes)?;
        for capture in pattern.captures_iter(text) {
            routes.push(json!({"path":&capture[1],"source":path,"unit":unit(path),"line":text[..capture.get(0).unwrap().start()].bytes().filter(|b|*b==b'\n').count()+1}));
        }
    }
    out["routes"] = json!(routes);
    let knowledge: Vec<_> = routes
        .iter()
        .filter(|r| {
            r["source"]
                .as_str()
                .is_some_and(|p| p.contains("knowledge"))
        })
        .collect();
    out["knowledge_routes"] = json!({"route_file_count":knowledge.iter().filter_map(|r|r["source"].as_str()).collect::<BTreeSet<_>>().len(),"native_literal_route_count":knowledge.len()});
    out["methodology"] = json!([
        "Compose parsed structurally without environment interpolation.",
        "Native literal router declarations are source evidence, not live routing proof.",
        "Dynamic routes remain subject to live OpenAPI contract verification."
    ]);
    Ok(out)
}
pub(super) fn build(s: &Snapshot) -> Result<Value> {
    let mut out = s.envelope("contract-freeze");
    let mut openapi = file(s, "sdk/openapi.json");
    if let Some(bytes) = s.files.get("sdk/openapi.json") {
        let spec: Value = serde_json::from_slice(bytes)?;
        openapi["path_count"] = json!(spec["paths"].as_object().map_or(0, |v| v.len()));
    }
    let overlay = "rust/agent-runtime-overlay/manifest.json";
    let manifest: Value = if let Some(bytes) = s.files.get(overlay) {
        serde_json::from_slice(bytes)?
    } else {
        json!({})
    };
    let (objects, _, _) = data::ddl(s)?;
    let functions: Vec<_> = objects
        .iter()
        .filter(|(_, v)| {
            v["kinds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|k| k == "FUNCTION")
        })
        .map(|(k, _)| k)
        .collect();
    let composition = compose(s, COMPOSE)?;
    let fixtures = [
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/platform_catalog_v1.json",
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/tests/fixtures/mcp_read_request_v1.json",
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/tests/fixtures/attachment_read_request_v1.json",
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/tests/fixtures/attachment_zip_bomb_manifest.json",
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-office/fixtures/en_semantic.json",
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-office/fixtures/zh_semantic.json",
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-office/fixtures/visual_input.json",
    ];
    out["frozen"] = json!({"openapi_snapshot":openapi,"sse_fixture":file(s,"sdk/fixtures/sse_inner_envelopes.json"),"capability_v2":{"authority":file(s,"rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/lib.rs"),"fixtures":fixtures.iter().map(|p|file(s,p)).collect::<Vec<_>>(),"note":"Rust CapabilityDescriptorV2 is the authority; no Python projection is required."},"rust_overlay_manifest":{"path":overlay,"contents":manifest,"sha256":s.files.get(overlay).map(|b|hash(b))},"database_functions":{"function_count":functions.len(),"functions":functions,"canonical_sha256":canonical(&json!({"functions":functions}))?,"derived_from":["database/schema.sql","database/migrations/**"]},"compose_resolution":{"services":composition,"canonical_sha256":canonical(&composition)?,"note":"Offline structural parse retains symbolic environment references. Run make validate-config for rendered resolution."},"image_revisions":{"rust_overlay_identity":manifest,"compose_defaults":"see compose_resolution.services","registry_digest_status":"requires live builder proof"},"ppr00_performance_baseline":{"receipts":(1..=4).map(|n|file(s,&format!("reports/performance/assistant-ttft-baseline-2026-08-28-run{n}.json"))).collect::<Vec<_>>(),"status":"Historical evidence hashed as-is; never rewritten."}});
    out["live_stack_required"] = json!([{"contract":"Live Gateway OpenAPI superset diff","command":null,"status":"native live OpenAPI verification command pending registration","note":"Requires running Gateway; offline digest is not proof."},{"contract":"SDK SSE envelope live agreement","command":"SDK_SSE_CONTRACT_REQUIRE_ALL=1 make sdk-sse-contract"},{"contract":"Runtime/Worker image digests from builder","command":"make agent-runtime-contract / make agent-capability-worker-build-local"},{"contract":"Capability V2 Worker revalidation in live Turn","command":"make agent-runtime-write-gate"},{"contract":"Compose rendered environment resolution","command":"make validate-config"}]);
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_artifact_is_explicit() {
        let s = Snapshot {
            sha: "a".repeat(40),
            tree: "b".repeat(40),
            files: BTreeMap::new(),
        };
        assert_eq!(
            file(&s, "missing"),
            json!({"path":"missing","exists":false})
        );
    }
}
