//! Validate the effective paired native parser inputs, not independent examples.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
type Env = BTreeMap<String, String>;
fn parse(env: &Env, key: &str) -> Result<Option<Value>, String> {
    env.get(key)
        .map(|raw| serde_json::from_str(raw).map_err(|_| format!("invalid {key} JSON")))
        .transpose()
}
fn pins(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
pub fn environments(gateway: &Env, api: &Env, worker: &Env) -> Result<(), String> {
    let mode = gateway
        .get("AI_PLATFORM_WORKLOAD_MTLS_MODE")
        .map(String::as_str)
        .unwrap_or("disabled");
    let configuration = parse(gateway, "GATEWAY_CEDAR_CONFIGURATION")?;
    let clients = [
        parse(api, "KNOWLEDGE_CEDAR_DATASET_CONFIGURATION")?,
        parse(worker, "KNOWLEDGE_CEDAR_DATASET_CONFIGURATION")?,
    ];
    if mode != "disabled" {
        let gateway_id = gateway
            .get("AI_PLATFORM_WORKLOAD_SPIFFE_ID")
            .ok_or("dataset Gateway identity")?;
        let trust = gateway_id
            .strip_suffix("/service/gateway")
            .filter(|v| v.starts_with("spiffe://"))
            .ok_or("dataset Gateway service identity")?;
        let expected_ids = [
            format!("{trust}/service/knowledge"),
            format!("{trust}/service/knowledge-worker"),
        ];
        let identities = [
            api.get("AI_PLATFORM_WORKLOAD_SPIFFE_ID")
                .ok_or("dataset Knowledge API identity")?,
            worker
                .get("AI_PLATFORM_WORKLOAD_SPIFFE_ID")
                .ok_or("dataset Knowledge worker identity")?,
        ];
        if identities[0] != &expected_ids[0] || identities[1] != &expected_ids[1] {
            return Err("dataset independent Knowledge identities".into());
        }
        let expected = identities
            .into_iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let raw = gateway
            .get("AI_PLATFORM_WORKLOAD_KNOWLEDGE_CLIENT_IDS")
            .ok_or("dataset restricted TLS allowlist missing")?;
        let entries = raw.split(',').collect::<Vec<_>>();
        let actual = entries.iter().copied().collect::<BTreeSet<_>>();
        let admitted = gateway
            .get("AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS")
            .ok_or("dataset general TLS allowlist")?
            .split(',')
            .collect::<BTreeSet<_>>();
        if entries.len() != actual.len() || actual != expected || !actual.is_subset(&admitted) {
            return Err(
                "dataset TLS subset must contain only independent Knowledge API/worker identities"
                    .into(),
            );
        }
    }
    let rollout = configuration
        .as_ref()
        .and_then(|c| c["mode"].as_str())
        .unwrap_or("off");
    if rollout == "off" {
        if clients.iter().any(Option::is_some) {
            return Err("dataset Knowledge configured without Gateway PDP".into());
        }
        return Ok(());
    }
    if !["shadow", "enforce"].contains(&rollout) || mode != "required" {
        return Err("dataset PDP requires coherent workload required rollout".into());
    }
    let config = configuration.as_ref().unwrap();
    let resource = &config["resources"];
    if !pins(&resource["expected_policy_sha256"]) || !pins(&resource["expected_matrix_sha256"]) {
        return Err("dataset native resource pins required".into());
    }
    let enforced = resource["enforce"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == "dataset"));
    if (rollout == "enforce") != enforced {
        return Err("dataset Gateway resource rollout mismatch".into());
    }
    let gateway_id = gateway
        .get("AI_PLATFORM_WORKLOAD_SPIFFE_ID")
        .ok_or("dataset Gateway identity")?;
    let mut shared_origin = None;
    for (environment, client) in [api, worker].into_iter().zip(clients.iter()) {
        let client = client.as_ref().ok_or("dataset Knowledge rollout missing")?;
        if client["mode"] != rollout
            || client["expected_policy_sha256"] != resource["expected_policy_sha256"]
            || client["expected_matrix_sha256"] != resource["expected_matrix_sha256"]
        {
            return Err("dataset Gateway/Knowledge modes or resource pins differ".into());
        }
        if client.as_object().is_none_or(|m| {
            m.len() != 4
                || ![
                    "origin",
                    "mode",
                    "expected_policy_sha256",
                    "expected_matrix_sha256",
                ]
                .iter()
                .all(|key| m.contains_key(*key))
        }) {
            return Err("dataset Knowledge parser fields".into());
        }
        if environment
            .get("AI_PLATFORM_WORKLOAD_MTLS_MODE")
            .map(String::as_str)
            != Some("required")
        {
            return Err("dataset Knowledge workload required".into());
        }
        let origin = client["origin"].as_str().ok_or("dataset origin")?;
        if !origin.starts_with("https://") || origin[8..].contains(['/', '@', '?', '#', '\\']) {
            return Err("dataset exact private TLS origin".into());
        }
        let peers = parse(environment, "AI_PLATFORM_WORKLOAD_ALLOWED_ORIGINS")?
            .ok_or("dataset Knowledge workload peer map")?;
        if peers[origin].as_str() != Some(gateway_id.as_str()) {
            return Err("dataset origin must be admitted Gateway TLS peer".into());
        }
        if shared_origin.is_some_and(|old| old != origin) {
            return Err("dataset Knowledge origins differ".into());
        }
        shared_origin = Some(origin);
    }
    Ok(())
}
fn container_env(container: &Value) -> Env {
    container["env"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| Some((v["name"].as_str()?.into(), v["value"].as_str()?.into())))
        .collect()
}
pub fn helm(docs: &[Value]) -> Result<(), String> {
    let mut envs = BTreeMap::new();
    for doc in docs.iter().filter(|doc| doc["kind"] == "Deployment") {
        let component =
            doc["spec"]["template"]["metadata"]["labels"]["app.kubernetes.io/component"]
                .as_str()
                .unwrap_or("");
        if !["gateway", "knowledge", "knowledge-worker"].contains(&component) {
            continue;
        }
        if let Some(container) = doc["spec"]["template"]["spec"]["containers"]
            .as_array()
            .and_then(|cs| cs.iter().find(|c| c["name"] == component))
        {
            envs.insert(component, container_env(container));
        }
    }
    // The owning full deployment checker separately requires all components.
    if let (Some(g), Some(a), Some(w)) = (
        envs.get("gateway"),
        envs.get("knowledge"),
        envs.get("knowledge-worker"),
    ) {
        environments(g, a, w)?;
    }
    Ok(())
}
pub fn compose(root: &Value) -> Result<(), String> {
    let get = |name: &str| {
        root["services"][name]["environment"]
            .as_object()
            .map(|env| {
                env.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.into())))
                    .collect::<Env>()
            })
            .ok_or("dataset Compose service environment")
    };
    environments(
        &get("gateway")?,
        &get("knowledge-service")?,
        &get("knowledge-worker")?,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture(mode: &str) -> [Env; 3] {
        let id = |s: &str| format!("spiffe://example.internal/service/{s}");
        let resource = json!({"enforce":if mode=="enforce"{vec!["dataset"]}else{vec![]},"expected_policy_sha256":"a".repeat(64),"expected_matrix_sha256":"b".repeat(64)});
        let g = Env::from([
            ("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), "required".into()),
            ("AI_PLATFORM_WORKLOAD_SPIFFE_ID".into(), id("gateway")),
            (
                "AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS".into(),
                format!(
                    "{},{},{}",
                    id("knowledge"),
                    id("knowledge-worker"),
                    id("agent-runtime")
                ),
            ),
            (
                "AI_PLATFORM_WORKLOAD_KNOWLEDGE_CLIENT_IDS".into(),
                format!("{},{}", id("knowledge"), id("knowledge-worker")),
            ),
            (
                "GATEWAY_CEDAR_CONFIGURATION".into(),
                json!({"mode":mode,"resources":resource}).to_string(),
            ),
        ]);
        let kn = |service: &str| {
            Env::from([("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(),"required".into()),("AI_PLATFORM_WORKLOAD_SPIFFE_ID".into(),id(service)),("AI_PLATFORM_WORKLOAD_ALLOWED_ORIGINS".into(),json!({"https://gateway-private:8443":id("gateway")}).to_string()),("KNOWLEDGE_CEDAR_DATASET_CONFIGURATION".into(),json!({"origin":"https://gateway-private:8443","mode":mode,"expected_policy_sha256":"a".repeat(64),"expected_matrix_sha256":"b".repeat(64)}).to_string())])
        };
        [g, kn("knowledge"), kn("knowledge-worker")]
    }
    #[test]
    fn native_shadow_and_enforce_rollouts_are_paired() {
        for mode in ["shadow", "enforce"] {
            let [g, a, w] = fixture(mode);
            assert!(environments(&g, &a, &w).is_ok());
        }
        assert!(environments(&Env::new(), &Env::new(), &Env::new()).is_ok());
    }
    #[test]
    fn missing_widened_duplicate_or_impersonated_subset_is_rejected() {
        let [g, a, w] = fixture("enforce");
        for raw in [
            None,
            Some("spiffe://example.internal/service/agent-runtime"),
            Some(
                "spiffe://example.internal/service/knowledge,spiffe://example.internal/service/knowledge",
            ),
            Some("spiffe://example.internal/service/knowledge"),
        ] {
            let mut g = g.clone();
            g.remove("AI_PLATFORM_WORKLOAD_KNOWLEDGE_CLIENT_IDS");
            if let Some(raw) = raw {
                g.insert(
                    "AI_PLATFORM_WORKLOAD_KNOWLEDGE_CLIENT_IDS".into(),
                    raw.into(),
                );
            }
            assert!(environments(&g, &a, &w).is_err());
        }
        let mut w = w.clone();
        w.insert(
            "AI_PLATFORM_WORKLOAD_SPIFFE_ID".into(),
            a["AI_PLATFORM_WORKLOAD_SPIFFE_ID"].clone(),
        );
        assert!(environments(&g, &a, &w).is_err());
        let [mut g, mut a, w] = fixture("enforce");
        let other = "spiffe://other.internal/service/knowledge";
        a.insert("AI_PLATFORM_WORKLOAD_SPIFFE_ID".into(), other.into());
        let peers = format!("{other},spiffe://example.internal/service/knowledge-worker");
        g.insert(
            "AI_PLATFORM_WORKLOAD_KNOWLEDGE_CLIENT_IDS".into(),
            peers.clone(),
        );
        g.insert("AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS".into(), peers);
        assert!(environments(&g, &a, &w).is_err());
    }
    #[test]
    fn one_sided_rollout_mode_pins_origin_and_legacy_are_rejected() {
        let [g, a, w] = fixture("enforce");
        for (field, value) in [
            ("mode", json!("shadow")),
            ("expected_policy_sha256", json!("c".repeat(64))),
            ("expected_matrix_sha256", json!("c".repeat(64))),
            ("origin", json!("http://gateway:8080")),
            ("origin", json!("https://provider.example")),
            ("origin", json!("https://gateway-private:8443/path")),
            ("unexpected", json!(true)),
        ] {
            let mut a = a.clone();
            let mut config: Value =
                serde_json::from_str(&a["KNOWLEDGE_CEDAR_DATASET_CONFIGURATION"]).unwrap();
            config[field] = value;
            a.insert(
                "KNOWLEDGE_CEDAR_DATASET_CONFIGURATION".into(),
                config.to_string(),
            );
            assert!(environments(&g, &a, &w).is_err(), "{field}");
        }
        let mut absent = a.clone();
        absent.remove("KNOWLEDGE_CEDAR_DATASET_CONFIGURATION");
        assert!(environments(&g, &absent, &w).is_err());
        let mut dual = g.clone();
        dual.insert("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), "dual".into());
        assert!(environments(&dual, &a, &w).is_err());
        let mut disabled = g.clone();
        disabled.remove("GATEWAY_CEDAR_CONFIGURATION");
        assert!(environments(&disabled, &a, &w).is_err());
    }
}
