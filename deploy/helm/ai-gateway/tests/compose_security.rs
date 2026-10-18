//! Checks the effective Compose JSON, after interpolation/extends/unique-volume merge.
use serde_json::Value;
pub fn check(root: &Value) -> Result<(), String> {
    super::dataset_authorization::compose(root)?;
    let services = root["services"].as_object().ok_or("compose services")?;
    for (service, role, prefix) in [
        ("gateway", "ai_gateway_gateway", "DATA_GATEWAY"),
        ("agent-runtime", "ai_gateway_runtime", "DATA_RUNTIME"),
        (
            "agent-capability-worker",
            "ai_gateway_capability_worker",
            "DATA_CAPABILITY_WORKER",
        ),
        (
            "knowledge-service",
            "ai_gateway_knowledge_api",
            "DATA_KNOWLEDGE",
        ),
        (
            "knowledge-worker",
            "ai_gateway_knowledge_worker",
            "DATA_KNOWLEDGE",
        ),
    ] {
        let s = services.get(service).ok_or("native compose service")?;
        let e = s["environment"].as_object().ok_or("compose environment")?;
        let mode = e
            .get("AI_PLATFORM_WORKLOAD_MTLS_MODE")
            .and_then(Value::as_str)
            .ok_or("compose workload mode")?;
        if !["dual", "required"].contains(&mode) {
            return Err("compose workload mode".into());
        }
        if mode == "required"
            && [
                "AI_PLATFORM_INTERNAL_TOKEN",
                "AI_PLATFORM_INTERNAL_TOKEN_FILE",
                "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN",
                "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN_FILE",
            ]
            .iter()
            .any(|k| e.contains_key(*k))
        {
            return Err("compose required bearer".into());
        }
        if service != "gateway"
            && [
                "AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE",
                "AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET",
                "AI_PLATFORM_CAPABILITY_PROOF_SECRET",
                "AI_PLATFORM_CAPABILITY_PROOF_SECRET_FILE",
            ]
            .iter()
            .any(|k| e.contains_key(*k))
        {
            return Err("compose consumer signer".into());
        }
        for old in [
            "DATABASE_URL",
            "GATEWAY_DATABASE__DSN",
            "KNOWLEDGE_DATABASE__DSN",
            "POSTGRES_PASSWORD",
        ] {
            if e.contains_key(old) {
                return Err("compose inline database fallback".into());
            }
        }
        if e.get(&format!("{prefix}_DSN_FILE"))
            .and_then(Value::as_str)
            .is_none_or(|s| !s.starts_with("/run/data/credentials/"))
        {
            return Err("compose file DSN".into());
        }
        let mounts = s["volumes"].as_array().ok_or("compose mounts")?;
        let data = mounts
            .iter()
            .find(|m| m["target"] == "/run/data/credentials")
            .ok_or("compose role mount")?;
        if data["read_only"] != true
            || data["source"]
                .as_str()
                .is_none_or(|s| !s.ends_with(&format!("/{role}")))
        {
            return Err("compose cross-role credential".into());
        }
        let tls = mounts
            .iter()
            .find(|m| m["target"] == "/run/workload/credentials")
            .ok_or("compose workload mount")?;
        if tls["read_only"] != true
            || tls["source"]
                .as_str()
                .is_none_or(|s| !s.ends_with(&format!("/{service}")))
        {
            return Err("compose cross-service identity".into());
        }
        if mode == "required"
            && mounts.iter().any(|m| {
                m["target"]
                    .as_str()
                    .is_some_and(|s| s.contains("internal-token"))
            })
        {
            return Err("compose retired bearer mount".into());
        }
        let bind = e
            .get("AI_PLATFORM_WORKLOAD_TLS_BIND")
            .and_then(Value::as_str)
            .ok_or("compose private TLS bind")?;
        let port = bind
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u64>().ok())
            .ok_or("compose TLS port")?;
        if s["ports"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|p| p["target"] == port)
        {
            return Err("compose published workload TLS".into());
        }
        if !s["healthcheck"]["test"].is_array() {
            return Err("compose independent health".into());
        }
        if service.starts_with("knowledge")
            && e.keys().any(|k| {
                k == "GATEWAY_ENCRYPTION_KEY"
                    || ([
                        "DASHSCOPE_",
                        "OPENAI_",
                        "ANTHROPIC_",
                        "KNOWLEDGE_EMBEDDINGS__",
                    ]
                    .iter()
                    .any(|p| k.starts_with(p))
                        && (k.ends_with("API_KEY") || k.ends_with("BASE_URL")))
            })
        {
            return Err("compose Knowledge provider credential".into());
        }
    }
    for public in ["frontend", "agent-python-sandbox"] {
        if let Some(s) = services.get(public) {
            if s["environment"]
                .as_object()
                .is_some_and(|e| e.keys().any(|k| k.starts_with("AI_PLATFORM_WORKLOAD_")))
                || s["volumes"].as_array().into_iter().flatten().any(|m| {
                    m["target"]
                        .as_str()
                        .is_some_and(|s| s.contains("/run/workload"))
                })
            {
                return Err("public or sandbox workload credential".into());
            }
        }
    }
    if let Some(sdk) = services.get("sdk-agent-worker") {
        let e = sdk["environment"].as_object().ok_or("SDK env")?;
        if e.keys()
            .any(|k| k.starts_with("OPENAI_") || k.starts_with("ANTHROPIC_"))
        {
            return Err("SDK provider configuration".into());
        }
        if e.get("AI_PLATFORM_WORKLOAD_MTLS_MODE")
            .is_some_and(|m| m == "required")
            && ["SDK_AGENT_WORKER_TOKEN", "SDK_AGENT_WORKER_TOKEN_FILE"]
                .iter()
                .any(|k| e.contains_key(*k))
        {
            return Err("SDK retired bearer".into());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Value {
        let mut services = serde_json::Map::new();
        for (s, r, p) in [
            ("gateway", "ai_gateway_gateway", "DATA_GATEWAY"),
            ("agent-runtime", "ai_gateway_runtime", "DATA_RUNTIME"),
            (
                "agent-capability-worker",
                "ai_gateway_capability_worker",
                "DATA_CAPABILITY_WORKER",
            ),
            (
                "knowledge-service",
                "ai_gateway_knowledge_api",
                "DATA_KNOWLEDGE",
            ),
            (
                "knowledge-worker",
                "ai_gateway_knowledge_worker",
                "DATA_KNOWLEDGE",
            ),
        ] {
            let mut e = serde_json::Map::new();
            e.insert("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), json!("required"));
            let identity_service = if s == "knowledge-service" {
                "knowledge"
            } else {
                s
            };
            e.insert(
                "AI_PLATFORM_WORKLOAD_SPIFFE_ID".into(),
                json!(format!(
                    "spiffe://fixture.internal/service/{identity_service}"
                )),
            );
            if s == "gateway" {
                let peers = "spiffe://fixture.internal/service/knowledge,spiffe://fixture.internal/service/knowledge-worker";
                e.insert(
                    "AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS".into(),
                    json!(peers),
                );
                e.insert(
                    "AI_PLATFORM_WORKLOAD_KNOWLEDGE_CLIENT_IDS".into(),
                    json!(peers),
                );
            }
            e.insert(
                "AI_PLATFORM_WORKLOAD_TLS_BIND".into(),
                json!(format!("{s}-workload:8443")),
            );
            e.insert(
                format!("{p}_DSN_FILE"),
                json!("/run/data/credentials/dsn-control"),
            );
            services.insert(s.into(),json!({"environment":e,"healthcheck":{"test":["CMD","health"]},"volumes":[{"target":"/run/data/credentials","source":format!("/private/{r}"),"read_only":true},{"target":"/run/workload/credentials","source":format!("/private/{s}"),"read_only":true}]}));
        }
        json!({"services":services})
    }
    #[test]
    fn effective_required_graph_is_admitted() {
        assert!(check(&fixture()).is_ok());
    }
    #[test]
    fn role_swap_legacy_env_and_public_tls_fail() {
        let mut v = fixture();
        v["services"]["gateway"]["volumes"][0]["source"] = "/private/ai_gateway_runtime".into();
        assert_eq!(check(&v).unwrap_err(), "compose cross-role credential");
        let mut v = fixture();
        v["services"]["agent-runtime"]["environment"]["AI_PLATFORM_INTERNAL_TOKEN_FILE"] =
            "/old-token".into();
        assert_eq!(check(&v).unwrap_err(), "compose required bearer");
        let mut v = fixture();
        v["services"]["knowledge-service"]["ports"] = json!([{"target":8443,"published":"8443"}]);
        assert_eq!(check(&v).unwrap_err(), "compose published workload TLS");
    }
    #[test]
    fn retired_mount_and_provider_material_fail() {
        let mut v = fixture();
        v["services"]["gateway"]["volumes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"target":"/run/app-secrets/internal-token"}));
        assert_eq!(check(&v).unwrap_err(), "compose retired bearer mount");
        let mut v = fixture();
        v["services"]["knowledge-service"]["environment"]["DASHSCOPE_API_KEY"] = "forbidden".into();
        assert_eq!(
            check(&v).unwrap_err(),
            "compose Knowledge provider credential"
        );
    }
}
