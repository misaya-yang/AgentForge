//! U-DATA-02: inspect rendered public deployment configuration, never env files.
use crate::*;

fn expected(service: &str) -> Option<&'static str> {
    match service {
        "gateway" => Some("gateway"),
        "knowledge-service" => Some("knowledge_api"),
        "knowledge-worker" => Some("knowledge_worker"),
        "agent-runtime" => Some("runtime"),
        "agent-capability-worker" => Some("capability_worker"),
        "migrate" => Some("migrator"),
        _ => None,
    }
}

fn verify_user(user: &str, prefix: &str, principal: &str) -> Result<()> {
    require(
        user == format!("{prefix}{principal}"),
        "database identity is not the assigned scoped service role",
    )
}

fn inspect_env(env: &Value, prefix: &str, principal: &str) -> Result<()> {
    let env = env
        .as_object()
        .ok_or("rendered database environment must be a map")?;
    require(
        !env.contains_key("AI_GATEWAY_DATABASE_ADMIN_DSN"),
        "application/migrator must not receive admin DSN",
    )?;
    let mut identities = 0;
    for (key, value) in env {
        if key == "POSTGRES_USER" {
            verify_user(value.as_str().unwrap_or_default(), prefix, principal)?;
            identities += 1;
        }
        if key.ends_with("DSN") || ["DATABASE_URL", "DATABASE_URI"].contains(&key.as_str()) {
            let raw = value.as_str().ok_or("database DSN must be a string")?;
            if raw.is_empty() {
                continue;
            }
            let url = reqwest::Url::parse(raw)
                .map_err(|_| "database DSN is malformed; value withheld")?;
            require(
                ["postgresql", "postgres"].contains(&url.scheme()),
                "unsupported database DSN scheme",
            )?;
            verify_user(url.username(), prefix, principal)?;
            identities += 1;
        }
        if key == "POSTGRES_PASSWORD" {
            require(
                env.contains_key("POSTGRES_USER"),
                "orphan bootstrap password in application configuration",
            )?;
        }
    }
    require(
        identities > 0,
        "service has no explicit scoped database identity",
    )
}

pub fn check_compose(config: &Value, prefix: &str) -> Result<usize> {
    require(
        regex::Regex::new(r"^[a-z][a-z0-9_]{0,20}_$")?.is_match(prefix),
        "invalid role prefix",
    )?;
    let services = config["services"]
        .as_object()
        .ok_or("rendered Compose services missing")?;
    let mut checked = 0;
    for (service, unit) in services {
        if let Some(principal) = expected(service) {
            inspect_env(&unit["environment"], prefix, principal)?;
            checked += 1;
        }
    }
    require(checked > 0, "no database service identities checked")?;
    Ok(checked)
}

/// Validate Secret key isolation in rendered Kubernetes workloads, without
/// fetching Secret payloads. The process guard proves the actual login role.
pub fn check_kubernetes(documents: &[Value]) -> Result<usize> {
    let mut checked = 0;
    for document in documents {
        let component = document["metadata"]["labels"]["app.kubernetes.io/component"]
            .as_str()
            .unwrap_or_default();
        let key = match component {
            "gateway" => "GATEWAY_DSN".to_string(),
            "knowledge" => "KNOWLEDGE_API_DSN".to_string(),
            "knowledge-worker" => "KNOWLEDGE_WORKER_DSN".to_string(),
            "migration" => "MIGRATOR_DSN".to_string(),
            name if name.starts_with("langgraph-") => format!(
                "LANGGRAPH_{}_DSN",
                name.trim_start_matches("langgraph-").to_ascii_uppercase()
            ),
            _ => continue,
        };
        let spec = &document["spec"]["template"]["spec"];
        if !spec.is_object() {
            continue;
        }
        for group in ["initContainers", "containers"] {
            for container in spec[group].as_array().into_iter().flatten() {
                let mut found = false;
                for env in container["env"].as_array().into_iter().flatten() {
                    let name = env["name"].as_str().unwrap_or_default();
                    require(
                        ![
                            "POSTGRES_USER",
                            "POSTGRES_PASSWORD",
                            "AI_GATEWAY_DATABASE_ADMIN_DSN",
                        ]
                        .contains(&name),
                        "workload receives bootstrap database inputs",
                    )?;
                    if name.ends_with("DSN") || ["DATABASE_URL", "DATABASE_URI"].contains(&name) {
                        let reference = &env["valueFrom"]["secretKeyRef"];
                        require(
                            env.get("value").is_none()
                                && reference["key"] == key
                                && reference["name"]
                                    .as_str()
                                    .is_some_and(|name| !name.is_empty()),
                            "workload DSN must reference its assigned scoped Secret key",
                        )?;
                        found = true;
                    }
                }
                for source in container["envFrom"].as_array().into_iter().flatten() {
                    require(
                        source.get("secretRef").is_none(),
                        "application must not import an entire credential Secret",
                    )?;
                }
                require(
                    found,
                    "database workload container has no scoped DSN reference",
                )?;
                checked += 1;
            }
        }
    }
    require(
        checked > 0,
        "no Kubernetes database workload identities checked",
    )?;
    Ok(checked)
}

/// --config is rendered Compose JSON/YAML provided by CI, not a private env.
/// Values never appear in errors or receipts.
pub fn run(args: &Args) -> Result<Value> {
    if args.flag("--matrix") {
        return crate::compose_identity::matrix(&args.root());
    }
    let prefix = args
        .values
        .get("--role-prefix")
        .map(String::as_str)
        .unwrap_or("ai_gateway_");
    let count = if args.values.get("--format").map(String::as_str) == Some("kubernetes") {
        use serde::Deserialize;
        let text = std::fs::read_to_string(args.get("--config")?)?;
        let documents = serde_yaml::Deserializer::from_str(&text)
            .map(Value::deserialize)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| "rendered Kubernetes YAML is malformed; values withheld")?;
        check_kubernetes(&documents)?
    } else {
        let config: Value = serde_yaml::from_slice(&std::fs::read(args.get("--config")?)?)
            .map_err(|_| "rendered Compose YAML is malformed; values withheld")?;
        check_compose(&config, prefix)?
    };
    Ok(
        json!({"result":"pass", "command":"database-identity-gate", "services_checked":count, "values_withheld":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kubernetes_rejects_bootstrap_and_other_role_secret_references() {
        let good = json!({"metadata":{"labels":{"app.kubernetes.io/component":"gateway"}},
            "spec":{"template":{"spec":{"containers":[{"env":[{"name":"GATEWAY_DATABASE__DSN",
                "valueFrom":{"secretKeyRef":{"name":"scoped-db","key":"GATEWAY_DSN"}}}]}]}}}});
        assert!(check_kubernetes(std::slice::from_ref(&good)).is_ok());
        let mut bad = good.clone();
        bad["spec"]["template"]["spec"]["containers"][0]["env"][0]["valueFrom"]["secretKeyRef"]["key"] =
            "MIGRATOR_DSN".into();
        assert!(check_kubernetes(&[bad]).is_err());
        let mut bad = good.clone();
        bad["spec"]["template"]["spec"]["containers"][0]["env"][0] =
            json!({"name":"POSTGRES_USER","value":"postgres"});
        assert!(check_kubernetes(&[bad]).is_err());
        let mut bad = good;
        bad["spec"]["template"]["spec"]["containers"][0]["envFrom"] =
            json!([{"secretRef":{"name":"bootstrap"}}]);
        assert!(check_kubernetes(&[bad]).is_err());
    }

    #[test]
    fn own_role_is_required_for_split_and_dsn_configuration() {
        for service in [
            "gateway",
            "knowledge-service",
            "knowledge-worker",
            "agent-runtime",
            "agent-capability-worker",
            "migrate",
        ] {
            let principal = expected(service).unwrap();
            for bad in [
                "postgres",
                "ai_gateway_owner",
                "foreign_gateway",
                "ai_gateway_migrator",
                "ai_gateway_knowledge_api",
            ] {
                if bad == format!("ai_gateway_{principal}") {
                    continue;
                }
                let split = json!({"POSTGRES_USER":bad,"POSTGRES_PASSWORD":"fixture-not-a-secret"});
                assert!(inspect_env(&split, "ai_gateway_", principal).is_err());
                let dsn = json!({"DATABASE_URL":format!("postgresql://{bad}:fixture@postgres/db")});
                assert!(inspect_env(&dsn, "ai_gateway_", principal).is_err());
            }
            assert!(
                inspect_env(
                    &json!({"POSTGRES_USER":format!("ai_gateway_{principal}")}),
                    "ai_gateway_",
                    principal
                )
                .is_ok()
            );
            assert!(inspect_env(&json!({"DATABASE_URL":format!("postgresql://ai_gateway_{principal}:fixture@postgres/db")}), "ai_gateway_", principal).is_ok());
        }
    }
    #[test]
    fn missing_unresolved_and_admin_side_channels_are_rejected_without_secret_echo() {
        for env in [
            json!({}),
            json!({"DATABASE_URL":"${SECRET_DSN}"}),
            json!({"POSTGRES_USER":"ai_gateway_gateway","AI_GATEWAY_DATABASE_ADMIN_DSN":"private-sentinel"}),
            json!({"POSTGRES_PASSWORD":"private-sentinel"}),
        ] {
            let err = inspect_env(&env, "ai_gateway_", "gateway")
                .unwrap_err()
                .to_string();
            assert!(!err.contains("private-sentinel"));
        }
        assert!(check_compose(&json!({"services":{}}), "ai_gateway_").is_err());
    }
}
