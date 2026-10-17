//! Offline schema tests plus rendered Kubernetes contracts. No cluster access.
use serde_json::Value;
use std::{fs, path::Path};
#[cfg(test)]
fn yaml(path: &Path) -> Value {
    serde_yaml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}
#[cfg(test)]
fn schema(root: &Path, values: &Value) -> bool {
    let document: Value =
        serde_json::from_str(&fs::read_to_string(root.join("values.schema.json")).unwrap())
            .unwrap();
    jsonschema::validator_for(&document)
        .unwrap()
        .is_valid(values)
}
fn role_key(component: &str) -> Option<&'static str> {
    match component {
        "gateway" => Some("GATEWAY_DSN"),
        "knowledge" => Some("KNOWLEDGE_API_DSN"),
        "knowledge-worker" => Some("KNOWLEDGE_WORKER_DSN"),
        "agent-runtime" => Some("RUNTIME_DSN"),
        "agent-capability-worker" => Some("CAPABILITY_WORKER_DSN"),
        "migration" => Some("MIGRATOR_DSN"),
        _ => None,
    }
}
pub fn render_contract(docs: &[Value]) -> Result<(), String> {
    let mut deployments = std::collections::BTreeSet::new();
    for doc in docs {
        let kind = doc["kind"].as_str().unwrap_or("");
        if ["StatefulSet", "Secret"].contains(&kind) {
            return Err("application release must not own state or credentials".into());
        }
        if !["Deployment", "Job"].contains(&kind) {
            continue;
        }
        let pod = &doc["spec"]["template"]["spec"];
        if pod["automountServiceAccountToken"] != false {
            return Err("service account token".into());
        }
        let component = doc["spec"]["template"]["metadata"]["labels"]
            ["app.kubernetes.io/component"]
            .as_str()
            .unwrap_or("");
        if kind == "Deployment" {
            deployments.insert(component.to_owned());
        }
        for group in ["containers", "initContainers"] {
            for container in pod[group].as_array().into_iter().flatten() {
                let image = container["image"].as_str().ok_or("missing image")?;
                let digest = image.rsplit_once("@sha256:").ok_or("unpinned image")?.1;
                if digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err("invalid digest".into());
                }
                let security = &container["securityContext"];
                if security["readOnlyRootFilesystem"] != true
                    || security["allowPrivilegeEscalation"] != false
                    || security["runAsNonRoot"] != true
                {
                    return Err("container hardening".into());
                }
                if let Some(key) = role_key(component) {
                    if !container["env"].as_array().into_iter().flatten().any(|e| {
                        e["valueFrom"]["secretKeyRef"]["key"] == key
                            && e["valueFrom"]["secretKeyRef"]["name"]
                                .as_str()
                                .is_some_and(|n| !n.is_empty())
                    }) {
                        return Err("missing scoped role DSN".into());
                    }
                }
                if security["capabilities"]["drop"] != serde_json::json!(["ALL"])
                    || security["runAsUser"].as_u64().is_none_or(|uid| uid == 0)
                {
                    return Err("root or capabilities".into());
                }
                if container.get("envFrom").is_some() {
                    return Err("unbounded secret env".into());
                }
                if component == "agent-python-sandbox"
                    && (container.get("env").is_some() || container.get("ports").is_some())
                {
                    return Err("sandbox credentials or TCP".into());
                }
                for env in container["env"].as_array().into_iter().flatten() {
                    if component != "gateway"
                        && env["name"] == "AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE"
                    {
                        return Err("consumer signer".into());
                    }
                    if env["name"].as_str().unwrap_or("").contains("ADMIN_DSN") {
                        return Err("admin identity".into());
                    }
                }
            }
        }
        for volume in pod["volumes"].as_array().into_iter().flatten() {
            if volume.get("hostPath").is_some() {
                return Err("hostPath forbidden".into());
            }
            if component == "agent-python-sandbox" && volume.get("secret").is_some() {
                return Err("sandbox secret volume".into());
            }
        }
    }
    for expected in [
        "gateway",
        "knowledge",
        "knowledge-worker",
        "agent-runtime",
        "agent-capability-worker",
        "frontend",
    ] {
        if !deployments.contains(expected) {
            return Err(format!("missing native unit {expected}"));
        }
    }
    Ok(())
}
pub fn check_rendered(path: &Path) -> Result<(), String> {
    use serde::Deserialize;
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let docs = serde_yaml::Deserializer::from_str(&text)
        .map(|d| Value::deserialize(d).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    render_contract(
        &docs
            .into_iter()
            .filter(|v| !v.is_null())
            .collect::<Vec<_>>(),
    )
}
fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("rendered YAML file required");
    if let Err(error) = check_rendered(Path::new(&path)) {
        eprintln!("chart contract: {error}");
        std::process::exit(1);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> &'static Path {
        {
            static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
            ROOT.get_or_init(|| {
                let base = Path::new(env!("CARGO_MANIFEST_DIR"));
                if base.join("values.schema.json").exists() {
                    base.to_owned()
                } else {
                    base.join("../../../../deploy/helm/ai-gateway")
                }
            })
            .as_path()
        }
    }
    fn fixture() -> Value {
        yaml(&root().join("tests/values-render.yaml"))
    }
    #[test]
    fn complete_public_fixture_is_valid() {
        assert!(schema(root(), &fixture()));
    }
    #[test]
    fn defaults_fail_closed() {
        assert!(!schema(root(), &yaml(&root().join("values.yaml"))));
    }
    #[test]
    fn digest_and_single_owner_negatives() {
        for path in ["gateway", "runtime", "knowledge"] {
            let mut v = fixture();
            v["images"][path]["digest"] = "latest".into();
            assert!(!schema(root(), &v));
        }
        for unit in ["gateway", "agentRuntime", "capabilityWorker"] {
            let mut v = fixture();
            v[unit]["replicaCount"] = 2.into();
            assert!(!schema(root(), &v));
        }
    }
    #[test]
    fn reject_legacy_plaintext_and_unbound_peers() {
        for key in ["langgraph", "secrets", "postgresql"] {
            let mut v = fixture();
            v[key] = serde_json::json!({"password":"bad"});
            assert!(!schema(root(), &v));
        }
        let mut v = fixture();
        v["data"]["qdrant"]["url"] = "http://qdrant:6333".into();
        assert!(!schema(root(), &v));
        let mut v = fixture();
        v["data"]["postgres"]["peer"] = serde_json::json!({});
        assert!(!schema(root(), &v));
    }
    #[test]
    fn incomplete_sandbox_rejected() {
        let mut v = fixture();
        v["sandbox"]["enabled"] = true.into();
        assert!(!schema(root(), &v));
    }
    #[test]
    fn sdk_requires_scoped_tls_and_digest() {
        let mut v = fixture();
        v["sdkWorker"]["enabled"] = true.into();
        assert!(!schema(root(), &v));
        for key in [
            "existingClaim",
            "workerTLSSecret",
            "callbackTLSSecret",
            "credentialsSecret",
        ] {
            v["sdkWorker"][key] = "fixture".into();
        }
        assert!(!schema(root(), &v));
        v["images"]["sdk"]["digest"] = format!("sha256:{}", "b".repeat(64)).into();
        assert!(schema(root(), &v));
        v["sdkWorker"]["retentionSeconds"] = 0.into();
        assert!(!schema(root(), &v));
    }
    #[test]
    fn explicit_sandbox_prerequisites_and_digest() {
        let mut v = fixture();
        v["sandbox"]["enabled"] = true.into();
        v["images"]["sandbox"]["digest"] = "".into();
        for key in [
            "nodeName",
            "runtimeClassName",
            "seccompProfile",
            "socketClaim",
            "workspaceClaim",
            "journalClaim",
            "signingSecret",
            "publicKeyId",
        ] {
            v["sandbox"][key] = "fixture".into();
        }
        v["sandbox"]["publicKeyHex"] = "a".repeat(64).into();
        v["sandbox"]["socketFsGroupMutationDisabled"] = true.into();
        v["sandbox"]["cgroupCSI"] = serde_json::json!({"driver":"fixture.invalid"});
        assert!(!schema(root(), &v));
        v["images"]["sandbox"]["digest"] = format!("sha256:{}", "b".repeat(64)).into();
        assert!(schema(root(), &v));
        v["sandbox"]["cgroupCSI"]["hostPath"] = "/sys/fs/cgroup".into();
        assert!(!schema(root(), &v));
    }
    fn docs() -> Vec<Value> {
        ["gateway","knowledge","knowledge-worker","agent-runtime","agent-capability-worker","frontend"].iter().map(|component| serde_json::json!({"kind":"Deployment","spec":{"template":{"metadata":{"labels":{"app.kubernetes.io/component":component}},"spec":{"automountServiceAccountToken":false,"containers":[{"image":format!("repo@sha256:{}","a".repeat(64)),"securityContext":{"readOnlyRootFilesystem":true,"allowPrivilegeEscalation":false,"runAsNonRoot":true,"runAsUser":1000,"capabilities":{"drop":["ALL"]}},"env":role_key(component).map(|key|vec![serde_json::json!({"name":"SCOPED_DSN","valueFrom":{"secretKeyRef":{"name":"fixture","key":key}}})]).unwrap_or_default()}]}}}})).collect()
    }
    #[test]
    fn rendered_native_graph_contract() {
        assert!(render_contract(&docs()).is_ok());
    }
    #[test]
    fn scoped_identity_and_nonroot_render_negatives() {
        let mut v = docs();
        v[0]["spec"]["template"]["spec"]["containers"][0]["env"][0]["valueFrom"]["secretKeyRef"]
            ["key"] = "MIGRATOR_DSN".into();
        assert_eq!(render_contract(&v).unwrap_err(), "missing scoped role DSN");
        let mut v = docs();
        v[0]["spec"]["template"]["spec"]["containers"][0]["securityContext"]["runAsUser"] =
            0.into();
        assert_eq!(render_contract(&v).unwrap_err(), "root or capabilities");
    }
    #[test]
    fn rendered_mutations_rejected() {
        let mut v = docs();
        v[0]["spec"]["template"]["spec"]["containers"][0]["image"] = "repo:latest".into();
        assert!(render_contract(&v).is_err());
        let mut v = docs();
        v[1]["spec"]["template"]["spec"]["containers"][0]["env"] = serde_json::json!([{"name":"AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE","value":"private"}]);
        assert!(render_contract(&v).is_err());
        let mut v = docs();
        v[0]["spec"]["template"]["spec"]["volumes"] =
            serde_json::json!([{"hostPath":{"path":"/"}}]);
        assert!(render_contract(&v).is_err());
        assert!(render_contract(&docs()[1..]).is_err());
    }
}
