//! The explicitly enabled strong runtime controller/attestor is separate node
//! infrastructure. These narrow exceptions never apply to application pods.
use serde_json::Value;
use std::collections::BTreeSet;
fn pool(docs: &[Value]) -> Result<Option<Value>, String> {
    let mut found = None;
    for d in docs {
        if let Some(raw) = d["data"]["pool.json"].as_str() {
            if found.is_some() {
                return Err("multiple strong pool authorities".into());
            }
            let v: Value = serde_json::from_str(raw).map_err(|_| "strong pool config")?;
            if v["shared_root"]
                .as_str()
                .is_none_or(|s| !s.starts_with('/') || !s.ends_with("/slots") || s.contains(".."))
            {
                return Err("strong pool root".into());
            }
            found = Some(v);
        }
    }
    Ok(found)
}
pub fn worker_control(docs: &[Value], component: &str, pod: &Value, volume: &Value) -> bool {
    let Ok(Some(p)) = pool(docs) else {
        return false;
    };
    let Some(root) = p["shared_root"]
        .as_str()
        .and_then(|s| s.strip_suffix("/slots"))
    else {
        return false;
    };
    component == "agent-capability-worker"
        && volume["name"] == "strong-control"
        && volume["hostPath"]["type"] == "Directory"
        && volume["hostPath"]["path"] == format!("{root}/control")
        && pod["nodeSelector"]["kubernetes.io/hostname"] == p["node_name"]
        && pod["containers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["name"] == "agent-capability-worker")
            .any(|c| {
                c["volumeMounts"].as_array().into_iter().flatten().any(|m| {
                    m["name"] == "strong-control"
                        && m["mountPath"] == "/run/agentforge-sandbox"
                        && m["readOnly"] == true
                }) && c["env"].as_array().into_iter().flatten().any(|e| {
                    e["name"] == "AI_PLATFORM_SANDBOX_SOCKET"
                        && e["value"] == "/run/agentforge-sandbox/control.sock"
                })
            })
}
pub fn check(docs: &[Value]) -> Result<BTreeSet<String>, String> {
    let mut trusted = BTreeSet::new();
    let p = pool(docs)?;
    for d in docs {
        let pod = &d["spec"]["template"]["spec"];
        let labels = &d["spec"]["template"]["metadata"]["labels"];
        let control = labels["agentforge.io/strong-controller"].as_str();
        let attestor = labels["agentforge.io/strong-attestor"].as_str();
        if control.is_none() && attestor.is_none() {
            if d["kind"] == "DaemonSet" {
                return Err("unadmitted daemonset".into());
            }
            continue;
        }
        let p = p
            .as_ref()
            .ok_or("strong pool infrastructure without authority")?;
        let (label, unit, command) = if let Some(v) = control {
            (v, "controller", "ai-platform-sandbox-pool")
        } else {
            (
                attestor.unwrap(),
                "attestor",
                "ai-platform-sandbox-attestor",
            )
        };
        if p["pool"] != label
            || d["metadata"]["name"] != format!("{label}-{unit}")
            || pod["nodeSelector"]["kubernetes.io/hostname"] != p["node_name"]
            || pod["serviceAccountName"] != format!("{label}-{unit}")
            || pod["automountServiceAccountToken"] != true
            || pod["enableServiceLinks"] != false
        {
            return Err("strong pool node/account binding".into());
        }
        let cs = pod["containers"]
            .as_array()
            .ok_or("strong infrastructure container")?;
        if cs.len() != 1
            || cs[0]["name"] != unit
            || cs[0]["command"] != serde_json::json!([command])
            || cs[0].get("env").is_some()
            || cs[0].get("envFrom").is_some()
        {
            return Err("strong infrastructure credential boundary".into());
        }
        let c = &cs[0];
        if c["image"].as_str().is_none_or(|s| {
            s.rsplit_once("@sha256:").is_none_or(|(_, d)| {
                d.len() != 64
                    || !d
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        }) || c["securityContext"]["allowPrivilegeEscalation"] != false
            || c["securityContext"]["readOnlyRootFilesystem"] != true
            || c["securityContext"]["capabilities"]["drop"] != serde_json::json!(["ALL"])
        {
            return Err("strong infrastructure image/hardening".into());
        }
        if unit == "controller" {
            if d["kind"] != "Deployment"
                || d["spec"]["replicas"] != 1
                || d["spec"]["strategy"]["type"] != "Recreate"
                || pod["securityContext"]["runAsUser"] != 10001
                || c["securityContext"]["capabilities"].get("add").is_some()
                || pod.get("hostPID").is_some()
            {
                return Err("strong controller privilege".into());
            }
        } else if d["kind"] != "DaemonSet"
            || pod["hostPID"] != true
            || c["securityContext"]["runAsUser"] != 0
            || c["securityContext"]["capabilities"]["add"]
                != serde_json::json!(["SYS_PTRACE", "DAC_READ_SEARCH"])
        {
            return Err("strong attestor privilege".into());
        }
        let root = p["shared_root"]
            .as_str()
            .unwrap()
            .strip_suffix("/slots")
            .unwrap();
        for v in pod["volumes"].as_array().into_iter().flatten() {
            let name = v["name"].as_str().ok_or("strong volume")?;
            if v.get("hostPath").is_some() {
                let path = v["hostPath"]["path"].as_str().ok_or("strong host path")?;
                let allowed = if unit == "controller" {
                    name == "shared" && path == root
                } else {
                    match name {
                        "slots" => path == format!("{root}/slots"),
                        "hostproc" => path == "/proc",
                        "cri" => v["hostPath"]["type"] == "Socket",
                        "crictl" => v["hostPath"]["type"] == "File",
                        _ => false,
                    }
                };
                if !allowed {
                    return Err("strong host path boundary".into());
                }
            }
            if v.get("secret").is_some()
                && (unit != "attestor" || name != "key" || v["secret"]["defaultMode"] != 256)
            {
                return Err("strong infrastructure secret".into());
            }
        }
        trusted.insert(d["metadata"]["name"].as_str().unwrap().into());
    }
    if let Some(p) = p {
        let name = p["pool"].as_str().ok_or("strong pool name")?;
        if !trusted.contains(&format!("{name}-controller"))
            || !trusted.contains(&format!("{name}-attestor"))
        {
            return Err("incomplete strong pool infrastructure".into());
        }
    }
    Ok(trusted)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn generic_daemonset_cannot_use_infrastructure_exception() {
        assert!(check(&[json!({"kind":"DaemonSet"})]).is_err());
    }
    #[test]
    fn worker_control_exception_needs_exact_authority_node_and_readonly_mount() {
        let p = json!({"data":{"pool.json":r#"{"pool":"test","shared_root":"/private/strong/slots","node_name":"node-a"}"#}});
        let pod = json!({"nodeSelector":{"kubernetes.io/hostname":"node-a"},"containers":[{"name":"agent-capability-worker","env":[{"name":"AI_PLATFORM_SANDBOX_SOCKET","value":"/run/agentforge-sandbox/control.sock"}],"volumeMounts":[{"name":"strong-control","mountPath":"/run/agentforge-sandbox","readOnly":true}]}]});
        let volume = json!({"name":"strong-control","hostPath":{"path":"/private/strong/control","type":"Directory"}});
        assert!(worker_control(
            &[p.clone()],
            "agent-capability-worker",
            &pod,
            &volume
        ));
        assert!(!worker_control(&[p.clone()], "gateway", &pod, &volume));
        let mut wrong = volume;
        wrong["hostPath"]["path"] = "/private/strong".into();
        assert!(!worker_control(
            &[p],
            "agent-capability-worker",
            &pod,
            &wrong
        ));
    }
}
