//! Native deployment trust boundary checks. Rendering is offline; no cluster or credentials.
use serde_json::Value;
fn env<'a>(container: &'a Value, name: &str) -> Option<&'a Value> {
    container["env"]
        .as_array()?
        .iter()
        .find(|v| v["name"] == name)
}
fn text<'a>(container: &'a Value, name: &str) -> Option<&'a str> {
    env(container, name)?["value"].as_str()
}
fn file_role(component: &str) -> Option<&'static str> {
    match component {
        "gateway" => Some("DATA_GATEWAY"),
        "agent-runtime" => Some("DATA_RUNTIME"),
        "agent-capability-worker" => Some("DATA_CAPABILITY_WORKER"),
        "knowledge" | "knowledge-worker" => Some("DATA_KNOWLEDGE"),
        _ => None,
    }
}
pub fn workload(pod: &Value, component: &str, container: &Value) -> Result<(), String> {
    let mode = text(container, "AI_PLATFORM_WORKLOAD_MTLS_MODE").unwrap_or("disabled");
    if mode == "disabled" {
        return Ok(());
    }
    if !["dual", "required"].contains(&mode) {
        return Err("invalid workload mode".into());
    }
    let identity = text(container, "AI_PLATFORM_WORKLOAD_SPIFFE_ID").ok_or("workload identity")?;
    if !identity.starts_with("spiffe://") || !identity.ends_with(&format!("/service/{component}")) {
        return Err("workload service binding".into());
    }
    if mode == "required"
        && [
            "AI_PLATFORM_INTERNAL_TOKEN",
            "AI_PLATFORM_INTERNAL_TOKEN_FILE",
            "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN",
            "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN_FILE",
            "SDK_AGENT_WORKER_TOKEN",
            "SDK_AGENT_WORKER_TOKEN_FILE",
        ]
        .iter()
        .any(|key| env(container, key).is_some())
    {
        return Err("required shared bearer".into());
    }
    if component != "gateway"
        && [
            "AI_PLATFORM_CAPABILITY_PROOF_SECRET",
            "AI_PLATFORM_CAPABILITY_PROOF_SECRET_FILE",
            "AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET",
            "AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE",
        ]
        .iter()
        .any(|key| env(container, key).is_some())
    {
        return Err("consumer signing secret".into());
    }
    for key in if ["knowledge", "knowledge-worker"].contains(&component) {
        vec!["AI_PLATFORM_WORKLOAD_ALLOWED_ORIGINS"]
    } else {
        vec![
            "AI_PLATFORM_WORKLOAD_PEERS_JSON",
            "AI_PLATFORM_WORKLOAD_ALLOWED_ORIGINS",
        ]
    } {
        let peers: Value = serde_json::from_str(text(container, key).ok_or("workload origin map")?)
            .map_err(|_| "workload origin map")?;
        let map = peers
            .as_object()
            .filter(|m| !m.is_empty())
            .ok_or("workload origin map")?;
        for (origin, id) in map {
            if !origin.starts_with("https://")
                || origin[8..].contains('/')
                || origin.contains(['@', '?', '#'])
                || !id
                    .as_str()
                    .is_some_and(|v| v.starts_with("spiffe://") && v.contains("/service/"))
            {
                return Err("workload exact authority".into());
            }
        }
    }
    let volumes = pod["volumes"].as_array().ok_or("workload volumes")?;
    if !volumes
        .iter()
        .any(|v| v["name"] == "workload-private" && v["emptyDir"]["medium"] == "Memory")
    {
        return Err("private workload volume".into());
    }
    let input = volumes
        .iter()
        .find(|v| v["name"] == "workload-input")
        .ok_or("workload projection")?;
    let keys = input["secret"]["items"]
        .as_array()
        .ok_or("workload projection")?;
    if keys.len() != 3
        || !["ca.pem", "cert.pem", "key.pem"]
            .iter()
            .all(|k| keys.iter().any(|v| v["key"] == *k))
    {
        return Err("bounded workload projection".into());
    }
    let mounts = container["volumeMounts"]
        .as_array()
        .ok_or("workload mounts")?;
    if mounts.iter().any(|v| v["name"] == "workload-input")
        || !mounts
            .iter()
            .any(|v| v["name"] == "workload-private" && v["readOnly"] == true)
    {
        return Err("workload importer bypass".into());
    }
    for (group, name) in [
        ("initContainers", "workload-import"),
        ("containers", "workload-rotation"),
    ] {
        let importer = pod[group]
            .as_array()
            .and_then(|cs| cs.iter().find(|c| c["name"] == name))
            .ok_or("native workload importer")?;
        if importer["command"] != serde_json::json!(["/bin/sh", "/opt/workload/import.sh"]) {
            return Err("native workload importer".into());
        }
    }
    let probe = &container["readinessProbe"]["httpGet"];
    if probe["scheme"].as_str().is_some_and(|v| v != "HTTP") || probe["path"] != "/health/ready" {
        return Err("independent HTTP health".into());
    }
    Ok(())
}
pub fn check(docs: &[Value]) -> Result<(), String> {
    for doc in docs {
        if !["Deployment", "Job"].contains(&doc["kind"].as_str().unwrap_or("")) {
            continue;
        }
        let pod = &doc["spec"]["template"]["spec"];
        let component =
            doc["spec"]["template"]["metadata"]["labels"]["app.kubernetes.io/component"]
                .as_str()
                .unwrap_or("");
        if ["gateway", "agent-runtime", "agent-capability-worker"].contains(&component)
            && doc["spec"].get("replicas").is_some_and(|n| *n != 1)
        {
            return Err("singleton retirement gate".into());
        }
        for c in pod["containers"].as_array().into_iter().flatten() {
            if c["name"].as_str() != Some(component) {
                continue;
            }
            workload(pod, component, c)?;
            if let Some(prefix) = file_role(component) {
                if env(c, &format!("{prefix}_DSN_FILE")).is_some() {
                    for old in [
                        "DATABASE_URL",
                        "GATEWAY_DATABASE__DSN",
                        "KNOWLEDGE_DATABASE__DSN",
                        "POSTGRES_PASSWORD",
                    ] {
                        if env(c, old).is_some() {
                            return Err("file DSN legacy fallback".into());
                        }
                    }
                    for file in ["dsn", "tls.crt", "tls.key", "ca-bundle.pem"] {
                        if !c["volumeMounts"].as_array().into_iter().flatten().any(|v| {
                            v["name"] == "data-credentials"
                                && v["subPath"] == file
                                && v["readOnly"] == true
                        }) {
                            return Err("regular role database files".into());
                        }
                    }
                }
            }
            if component != "gateway"
                && c["env"].as_array().into_iter().flatten().any(|e| {
                    let k = e["name"].as_str().unwrap_or("");
                    ["OPENAI_", "ANTHROPIC_", "DASHSCOPE_", "GEMINI_", "GOOGLE_"]
                        .iter()
                        .any(|p| k.starts_with(p))
                        && (k.contains("API_KEY") || k.contains("BASE_URL"))
                })
            {
                return Err("consumer provider credential".into());
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Value {
        let env = [
            ("AI_PLATFORM_WORKLOAD_MTLS_MODE", "required"),
            (
                "AI_PLATFORM_WORKLOAD_SPIFFE_ID",
                "spiffe://agentforge.internal/service/agent-runtime",
            ),
            (
                "AI_PLATFORM_WORKLOAD_ALLOWED_ORIGINS",
                r#"{"https://gateway:8443":"spiffe://agentforge.internal/service/gateway"}"#,
            ),
            (
                "AI_PLATFORM_WORKLOAD_PEERS_JSON",
                r#"{"https://gateway:8443":"spiffe://agentforge.internal/service/gateway"}"#,
            ),
        ]
        .iter()
        .map(|(k, v)| json!({"name":k,"value":v}))
        .collect::<Vec<_>>();
        json!({"volumes":[{"name":"workload-private","emptyDir":{"medium":"Memory"}},{"name":"workload-input","secret":{"items":[{"key":"ca.pem"},{"key":"cert.pem"},{"key":"key.pem"}]}}],"initContainers":[{"name":"workload-import","command":["/bin/sh","/opt/workload/import.sh"]}],"containers":[{"name":"workload-rotation","command":["/bin/sh","/opt/workload/import.sh"]},{"name":"agent-runtime","env":env,"readinessProbe":{"httpGet":{"path":"/health/ready","port":"http"}},"volumeMounts":[{"name":"workload-private","readOnly":true}]}]})
    }
    #[test]
    fn required_native_import_and_separate_health_are_admitted() {
        let p = fixture();
        assert!(workload(&p, "agent-runtime", &p["containers"][1]).is_ok());
    }
    #[test]
    fn required_rejects_each_transport_bearer_source() {
        for name in [
            "AI_PLATFORM_INTERNAL_TOKEN",
            "AI_PLATFORM_INTERNAL_TOKEN_FILE",
            "SDK_AGENT_WORKER_TOKEN_FILE",
            "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN",
        ] {
            let mut p = fixture();
            p["containers"][1]["env"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":name,"value":"/private/material"}));
            assert_eq!(
                workload(&p, "agent-runtime", &p["containers"][1]).unwrap_err(),
                "required shared bearer"
            );
        }
    }
    #[test]
    fn symlink_projection_bypass_and_missing_rotator_fail() {
        let mut p = fixture();
        p["containers"][1]["volumeMounts"][0]["name"] = "workload-input".into();
        assert_eq!(
            workload(&p, "agent-runtime", &p["containers"][1]).unwrap_err(),
            "workload importer bypass"
        );
        let mut p = fixture();
        p["initContainers"] = json!([]);
        assert!(workload(&p, "agent-runtime", &p["containers"][1]).is_err());
    }
    #[test]
    fn peer_identity_authority_and_plaintext_probe_cannot_drift() {
        let mut p = fixture();
        p["containers"][1]["env"][1]["value"] =
            "spiffe://agentforge.internal/service/gateway".into();
        assert!(workload(&p, "agent-runtime", &p["containers"][1]).is_err());
        let mut p = fixture();
        p["containers"][1]["env"][2]["value"] =
            r#"{"http://gateway:8080":"spiffe://agentforge.internal/service/gateway"}"#.into();
        assert_eq!(
            workload(&p, "agent-runtime", &p["containers"][1]).unwrap_err(),
            "workload exact authority"
        );
        let mut p = fixture();
        p["containers"][1]["readinessProbe"]["httpGet"]["scheme"] = "HTTPS".into();
        assert_eq!(
            workload(&p, "agent-runtime", &p["containers"][1]).unwrap_err(),
            "independent HTTP health"
        );
    }
}
