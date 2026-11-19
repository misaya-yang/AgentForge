//! Fresh local deployment admission, separate from published-release admission.
//! This gate does not waive existing image/source or business acceptance gates.
use crate::*;
fn volumes(service: &Value) -> Vec<(&str, &str, bool)> {
    service["volumes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            if let Some(v) = v.as_str() {
                let parts: Vec<_> = v.split(':').collect();
                (
                    parts.first().copied().unwrap_or(""),
                    parts.get(1).copied().unwrap_or(""),
                    parts.get(2) == Some(&"ro"),
                )
            } else {
                (
                    v["source"].as_str().unwrap_or(""),
                    v["target"].as_str().unwrap_or(""),
                    v["read_only"] == true,
                )
            }
        })
        .collect()
}
fn admit(value: &Value) -> Result<()> {
    let services = value["services"]
        .as_object()
        .ok_or("fresh services missing")?;
    for optional in [
        "agent-python-sandbox",
        "sdk-agent-worker",
        "database-bootstrap",
        "migrate",
    ] {
        require(
            !services.contains_key(optional),
            "disabled optional or historical migration service in fresh core",
        )?;
    }
    let authority = &value["services"]["database-fresh"];
    require(
        authority["user"] == "70:70" && authority["command"][0] == "fresh-development-init",
        "fresh authority requires independent local Unix operator",
    )?;
    require(
        volumes(authority).contains(&("fresh-postgres-socket", "/var/run/postgresql", true)),
        "fresh authority socket mount missing",
    )?;
    let e = authority["environment"]
        .as_object()
        .ok_or("authority environment missing")?;
    require(
        e.keys()
            .all(|k| !k.ends_with("_DSN") && !k.contains("ADMIN")),
        "authority cannot use admin TCP or inline DSNs",
    )?;
    for role in [
        "MIGRATOR",
        "GATEWAY",
        "RUNTIME",
        "CAPABILITY_WORKER",
        "KNOWLEDGE_API",
        "KNOWLEDGE_WORKER",
    ] {
        let prefix = format!("AI_GATEWAY_DATABASE_{role}");
        let directory = format!("/run/data/authority/{}", role.to_ascii_lowercase());
        for (suffix, file) in [
            ("DSN_FILE", "dsn"),
            ("PASSWORD_FILE", "password"),
            ("TLS_CA_FILE", "ca-bundle.pem"),
            ("TLS_CERT_FILE", "tls.crt"),
            ("TLS_KEY_FILE", "tls.key"),
        ] {
            require(
                e.get(&format!("{prefix}_{suffix}")) == Some(&json!(format!("{directory}/{file}"))),
                "six authority principals require complete assigned private files",
            )?;
        }
        for (suffix, value) in [
            ("EXPECTED_HOST", "postgres"),
            ("MODE", "direct"),
            ("DATABASE_ALIAS", "gateway_dev_fresh"),
            ("PHYSICAL_DATABASE", "gateway_dev_fresh"),
        ] {
            require(
                e.get(&format!("{prefix}_{suffix}")) == Some(&json!(value)),
                "authority direct endpoint and physical database must be explicit",
            )?;
        }
        require(
            e.get(&format!("{prefix}_EXPECTED_ROLE"))
                == Some(&json!(format!("ai_gateway_{}", role.to_ascii_lowercase()))),
            "authority principal must pin its exact role",
        )?;
    }
    require(
        volumes(&value["services"]["files-qdrant-storage"])
            == vec![("qdrant-fresh", "/output", false)],
        "Qdrant storage installer must target only new isolated vector storage",
    )?;
    let pg = &value["services"]["postgres"];
    require(
        volumes(pg).contains(&("postgres", "/var/lib/postgresql/data", false)),
        "existing PostgreSQL volume must be retained",
    )?;
    require(
        volumes(pg).contains(&("fresh-postgres-socket", "/var/run/postgresql", false)),
        "PostgreSQL peer socket must be shareable",
    )?;
    require(
        pg["environment"].get("POSTGRES_PASSWORD").is_none(),
        "PostgreSQL bootstrap password must be file-only",
    )?;
    require(
        pg["command"]
            .as_array()
            .is_some_and(|v| v.iter().any(|s| s == "ssl_min_protocol_version=TLSv1.3")),
        "database verified TLS required",
    )?;
    for (service, role, prefix) in [
        ("gateway", "gateway", "DATA_GATEWAY"),
        ("agent-runtime", "runtime", "DATA_RUNTIME"),
        (
            "agent-capability-worker",
            "capability_worker",
            "DATA_CAPABILITY_WORKER",
        ),
        ("knowledge-service", "knowledge_api", "DATA_KNOWLEDGE"),
        ("knowledge-worker", "knowledge_worker", "DATA_KNOWLEDGE"),
    ] {
        let s = &value["services"][service];
        let e = s["environment"]
            .as_object()
            .ok_or("application environment missing")?;
        require(
            e[&format!("{prefix}_MODE")] == "direct"
                && e[&format!("{prefix}_EXPECTED_HOST")] == "postgres",
            "fresh role must use checked direct database TLS",
        )?;
        require(
            e.keys().all(|k| {
                ![
                    "DATABASE_URL",
                    "POSTGRES_PASSWORD",
                    "GATEWAY_DATABASE__DSN",
                    "KNOWLEDGE_DATABASE__DSN",
                ]
                .contains(&k.as_str())
                    && !(k.starts_with("AI_GATEWAY_DATABASE_") && k.ends_with("_DSN"))
            }),
            "application inline database credential forbidden",
        )?;
        for (suffix, name) in [
            ("DSN_FILE", "dsn-control"),
            ("TLS_CA_FILE", "ca-bundle.pem"),
            ("TLS_CERT_FILE", "tls.crt"),
            ("TLS_KEY_FILE", "tls.key"),
        ] {
            require(
                e.get(&format!("{prefix}_{suffix}"))
                    == Some(&json!(format!("/run/data/credentials/{name}"))),
                "role files must use the assigned private mount",
            )?;
        }
        let name = format!("fresh-data-{role}");
        require(
            volumes(s).contains(&(name.as_str(), "/run/data/credentials", true)),
            "application needs only its own readonly data generation",
        )?;
        require(
            volumes(s)
                .iter()
                .filter(|v| v.1.starts_with("/run/data"))
                .all(|v| v.0 == name && v.1 == "/run/data/credentials" && v.2),
            "application must not mount another database role",
        )?;
        require(
            s["depends_on"]["database-fresh"]["condition"] == "service_completed_successfully",
            "application cannot bypass full schema authority",
        )?;
        require(
            e["AI_PLATFORM_LEASE_TRUST_MODE"] == "verify_only"
                && e["AI_PLATFORM_BROKER_PROOF_MODE"] == "verify_only",
            "fresh lease/proof consumers must not downgrade to shared signing secrets",
        )?;
        for key in [
            "AI_PLATFORM_CAPABILITY_PROOF_SECRET",
            "AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET",
        ] {
            require(
                !e.contains_key(key),
                "shared signing secret must not accompany V3 verifier",
            )?;
        }
        if service != "gateway" {
            require(
                !e.contains_key("AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE"),
                "only Gateway owns platform signer",
            )?;
            require(
                e.keys().all(|k| {
                    ![
                        "OPENAI_API_KEY",
                        "DASHSCOPE_API_KEY",
                        "ANTHROPIC_API_KEY",
                        "DEEPSEEK_API_KEY",
                        "GATEWAY_PROVIDERS__CONFIG",
                    ]
                    .contains(&k.as_str())
                }),
                "provider key outside Gateway",
            )?;
            require(
                !volumes(s).iter().any(|v| v.0 == "fresh-model-gateway"),
                "Gateway private signer volume outside Gateway",
            )?;
        }
    }
    for (service, identity) in [
        ("gateway", "gateway"),
        ("agent-runtime", "agent-runtime"),
        ("agent-capability-worker", "agent-capability-worker"),
        ("knowledge-service", "knowledge"),
        ("knowledge-worker", "knowledge-worker"),
    ] {
        let s = &value["services"][service];
        let e = &s["environment"];
        require(
            e["AI_PLATFORM_WORKLOAD_MTLS_MODE"] == "required"
                && e["AI_PLATFORM_WORKLOAD_CREDENTIALS_DIR"] == "/run/security/workload",
            "core business transport requires workload mTLS",
        )?;
        require(
            e["AI_PLATFORM_WORKLOAD_SPIFFE_ID"]
                == format!("spiffe://agentforge.internal/service/{identity}"),
            "each workload must pin its own exact service identity",
        )?;
        let origins: std::collections::BTreeMap<String, String> = serde_json::from_str(
            e["AI_PLATFORM_WORKLOAD_ALLOWED_ORIGINS"]
                .as_str()
                .ok_or("workload origins required")?,
        )
        .map_err(|_| "workload origins map invalid")?;
        require(
            !origins.is_empty()
                && origins.iter().all(|(origin, id)| {
                    origin.starts_with("https://")
                        && id.starts_with("spiffe://agentforge.internal/service/")
                        && !origin.contains('*')
                        && !id.contains('*')
                }),
            "business origins require exact HTTPS workload identities",
        )?;
        for key in [
            "AI_PLATFORM_INTERNAL_TOKEN",
            "AI_PLATFORM_INTERNAL_TOKEN_FILE",
            "AI_PLATFORM_CAPABILITY_PROOF_SECRET",
            "AI_PLATFORM_CAPABILITY_PROOF_SECRET_FILE",
            "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN",
            "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN_FILE",
            "AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET",
            "AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET_FILE",
        ] {
            require(
                e.get(key).is_none(),
                "required profile must retire all shared transport/signing secrets",
            )?;
        }
        let volume = format!("fresh-security-{service}");
        require(
            volumes(s).contains(&(volume.as_str(), "/run/security", true)),
            "workload/app files require role-only owned readonly volume",
        )?;
        require(
            s["depends_on"][format!("files-security-{service}")]["condition"]
                == "service_completed_successfully",
            "service must wait for its private security installation",
        )?;
    }
    let gateway = &value["services"]["gateway"]["environment"];
    require(
        gateway["AI_PLATFORM_BFF_ORIGINS"]
            .as_str()
            .is_some_and(|v| v.contains("LOCAL_FRESH_BFF_ORIGINS")),
        "exact approved BFF HTTPS origin required",
    )?;
    require(
        gateway["CONSOLE_PUBLIC_BASE_URL"]
            .as_str()
            .is_some_and(|v| v.contains("LOCAL_FRESH_CONSOLE_ORIGIN")),
        "console links and BFF origin must use the same selected HTTPS authority",
    )?;
    require(
        gateway["AI_PLATFORM_API_EDDSA_CONFIG_FILE"] == "/run/security/application/api-config.json",
        "BFF requires Gateway-only API EdDSA signing configuration",
    )?;
    require(
        gateway["AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_RUNTIME_BASE_URL"]
            == "https://gateway-workload:8443/internal/v1/agent-model-plane",
        "Gateway model-plane base must use its exact required TLS origin/path",
    )?;
    let frontend = &value["services"]["frontend"];
    require(
        frontend["ports"].as_array().is_some_and(|v| {
            v.iter()
                .all(|p| p.as_str().is_some_and(|p| p.ends_with(":8443")))
        }),
        "only frontend HTTPS may be published",
    )?;
    require(
        frontend["command"]
            .as_array()
            .is_some_and(|v| v.iter().any(|p| p == "/run/security/current/nginx.conf")),
        "frontend must load the owned HTTPS configuration",
    )?;
    require(
        frontend["environment"]["VITE_API_URL"] == ""
            && frontend["environment"]["VITE_API_BASE_URL"] == "",
        "browser BFF transport must stay same-origin",
    )?;
    let bootstrap = &value["services"]["admin-bootstrap"];
    require(
        bootstrap["user"] == "1000:1000"
            && bootstrap["command"][0]
                == "exec /usr/local/bin/admin-create --fresh-development < /run/initial-admin/input.json",
        "fresh account input must be fixed private stdin, never argv/env",
    )?;
    require(
        bootstrap["depends_on"]["database-fresh"]["condition"] == "service_completed_successfully"
            && value["services"]["gateway"]["depends_on"]["admin-bootstrap"]["condition"]
                == "service_completed_successfully",
        "first account must follow full schema and precede Gateway",
    )?;
    require(
        volumes(bootstrap)
            == vec![
                ("fresh-data-gateway", "/run/data/credentials", true),
                ("fresh-security-administrator", "/run/initial-admin", true),
            ],
        "first account gets only its own role and private input",
    )?;
    for singleton in ["gateway", "agent-runtime"] {
        require(
            value["services"][singleton]["deploy"]["replicas"] == 1,
            "fresh remains singleton until measured HA chaos",
        )?;
    }
    for (name, s) in services {
        if name.starts_with("files-") || name == "qdrant-files" {
            require(
                s["network_mode"] == "none" && s["user"] == "0:0" && s["read_only"] == true,
                "bounded installer must be offline and readonly",
            )?;
            require(
                s["cap_add"] == json!(["CHOWN", "DAC_OVERRIDE", "FOWNER"]),
                "installer capabilities must be bounded",
            )?;
            require(
                !volumes(s)
                    .iter()
                    .any(|v| v.1 == "/var/lib/postgresql/data" || v.0 == "postgres"),
                "installer must never change existing PGDATA",
            )?;
        }
    }
    Ok(())
}
fn admit_sdk(value: &Value) -> Result<()> {
    let worker = &value["services"]["sdk-agent-worker"];
    let e = worker["environment"]
        .as_object()
        .ok_or("SDK environment missing")?;
    require(
        e["AI_PLATFORM_WORKLOAD_MTLS_MODE"] == "required"
            && e["AI_PLATFORM_WORKLOAD_SPIFFE_ID"] == "spiffe://agentforge.internal/service/sdk"
            && e["SDK_AGENT_GATEWAY_URL"] == "https://gateway-workload:8443"
            && e["AI_PLATFORM_WORKLOAD_CREDENTIALS_DIR"] == "/run/security/workload",
        "SDK opt-in requires its own native workload identity",
    )?;
    require(
        !e.contains_key("SDK_AGENT_WORKER_TOKEN")
            && !e
                .keys()
                .any(|k| k.starts_with("SDK_AGENT_WORKER_TLS_") || k.contains("API_KEY")),
        "SDK required transport cannot contain legacy bearer or provider inputs",
    )?;
    require(
        worker.get("ports").is_none()
            && volumes(worker)
                == vec![
                    ("sdk-agent-state-fresh", "/var/lib/sdk-agent", false),
                    ("fresh-security-sdk-agent-worker", "/run/security", true),
                ]
            && worker["depends_on"]["files-security-sdk-agent-worker"]["condition"]
                == "service_completed_successfully",
        "SDK private listener and role-specific installation required",
    )?;
    require(
        value["services"]["gateway"]["environment"]["SDK_AGENT_WORKER_URL"]
            == "https://sdk-agent-worker-workload:8456",
        "Gateway SDK endpoint must match the exact SVID DNS name",
    )?;
    Ok(())
}
pub(super) fn validate(root: &Path) -> Result<Value> {
    let raw = fs_read(root.join("deploy/local/compose.core.yml"))?;
    let value: Value = serde_yaml::from_str(&raw).map_err(|_| "fresh core YAML invalid")?;
    admit(&value)?;
    let sdk: Value =
        serde_yaml::from_str(&fs_read(root.join("deploy/local/compose.core-sdk.yml"))?)
            .map_err(|_| "fresh SDK workload projection invalid")?;
    admit_sdk(&sdk)?;
    let db: Value = serde_yaml::from_str(&fs_read(
        root.join("deploy/local/compose.core-database.yml"),
    )?)
    .map_err(|_| "fresh database projection invalid")?;
    for name in [
        "files-postgres",
        "files-socket",
        "files-authority",
        "postgres",
        "database-fresh",
    ] {
        require(
            db["services"][name] == value["services"][name],
            "database projection drift",
        )?;
    }
    require(
        db["services"].as_object().is_some_and(|v| v.len() == 5),
        "database projection includes unrelated runtime service",
    )?;
    require(
        db["networks"]["data-postgres"] == value["networks"]["data-postgres"],
        "database network projection drift",
    )?;
    Ok(
        json!({"status":"passed","contract":"agentforge-local-fresh/v1","historical_chain_required":false,
        "old_postgres_volume_retained":true,"role_file_mtls":true,"operator_socket_only":true,
        "singleton_until_chaos":true,"runtime_acceptance":false}),
    )
}
fn fs_read(path: PathBuf) -> Result<String> {
    std::fs::read_to_string(path).map_err(|_| "public fresh configuration unavailable".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        serde_yaml::from_str(include_str!("../../../../../deploy/local/compose.core.yml")).unwrap()
    }
    #[test]
    fn full_fresh_contract_is_admitted() {
        admit(&fixture()).unwrap();
    }
    #[test]
    fn optional_sdk_uses_native_required_identity_without_legacy_transport() {
        let sdk: Value = serde_yaml::from_str(include_str!(
            "../../../../../deploy/local/compose.core-sdk.yml"
        ))
        .unwrap();
        admit_sdk(&sdk).unwrap();
        for (key, value) in [
            ("SDK_AGENT_WORKER_TOKEN", "legacy"),
            ("AI_PLATFORM_WORKLOAD_MTLS_MODE", "disabled"),
            ("SDK_AGENT_GATEWAY_URL", "http://gateway:8080"),
        ] {
            let mut bad = sdk.clone();
            bad["services"]["sdk-agent-worker"]["environment"][key] = json!(value);
            assert!(admit_sdk(&bad).is_err());
        }
        let mut bad = sdk;
        bad["services"]["sdk-agent-worker"]["ports"] = json!(["8456:8456"]);
        assert!(admit_sdk(&bad).is_err());
    }
    #[test]
    fn operator_and_role_boundaries_fail_closed() {
        let mut v = fixture();
        v["services"]["database-fresh"]["user"] = json!("1000:1000");
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["database-fresh"]["environment"]["AI_GATEWAY_DATABASE_ADMIN_DSN"] =
            json!("withheld");
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["knowledge-service"]["environment"]["OPENAI_API_KEY"] = json!("withheld");
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["gateway"]["deploy"]["replicas"] = json!(2);
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["agent-runtime"]["environment"]["AI_PLATFORM_LEASE_TRUST_MODE"] =
            json!("legacy");
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["agent-python-sandbox"] = json!({});
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["knowledge-service"]["environment"]["DATA_KNOWLEDGE_DSN_FILE"] =
            json!("/run/data/authority/migrator/dsn");
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["knowledge-service"]["volumes"]
            .as_array_mut()
            .unwrap()
            .push(json!("fresh-data-gateway:/run/data/other:ro"));
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["files-authority"]["network_mode"] = json!("host");
        assert!(admit(&v).is_err());
    }
    #[test]
    fn each_authority_principal_requires_the_complete_real_file_resolver_contract() {
        for role in [
            "MIGRATOR",
            "GATEWAY",
            "RUNTIME",
            "CAPABILITY_WORKER",
            "KNOWLEDGE_API",
            "KNOWLEDGE_WORKER",
        ] {
            for suffix in [
                "DSN_FILE",
                "PASSWORD_FILE",
                "TLS_CA_FILE",
                "TLS_CERT_FILE",
                "TLS_KEY_FILE",
                "EXPECTED_HOST",
                "DATABASE_ALIAS",
                "PHYSICAL_DATABASE",
                "MODE",
                "EXPECTED_ROLE",
            ] {
                let mut v = fixture();
                v["services"]["database-fresh"]["environment"]
                    .as_object_mut()
                    .unwrap()
                    .remove(&format!("AI_GATEWAY_DATABASE_{role}_{suffix}"));
                assert!(admit(&v).is_err(), "{role}/{suffix}");
            }
            for (suffix, bad) in [
                ("EXPECTED_HOST", "external"),
                ("DATABASE_ALIAS", "old_database"),
                ("PHYSICAL_DATABASE", "old_database"),
                ("MODE", "transaction"),
                ("EXPECTED_ROLE", "postgres"),
                ("PASSWORD_FILE", "/run/data/authority/gateway/../password"),
            ] {
                let mut v = fixture();
                v["services"]["database-fresh"]["environment"]
                    [format!("AI_GATEWAY_DATABASE_{role}_{suffix}")] = json!(bad);
                assert!(admit(&v).is_err(), "{role}/{suffix}");
            }
        }
    }
    #[test]
    fn required_transport_browser_origin_and_initial_account_never_downgrade() {
        let mut v = fixture();
        v["services"]["agent-capability-worker"]["environment"]["AI_PLATFORM_WORKLOAD_MTLS_MODE"] =
            json!("disabled");
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["gateway"]["environment"]["AI_PLATFORM_INTERNAL_TOKEN_FILE"] =
            json!("/secret/old-token");
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["knowledge-service"]["environment"]["AI_PLATFORM_WORKLOAD_ALLOWED_ORIGINS"] =
            json!(r#"{"http://gateway:8080":"spiffe://agentforge.internal/service/gateway"}"#);
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["frontend"]["ports"] = json!(["127.0.0.1:8081:8080"]);
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["frontend"]["environment"]["VITE_API_BASE_URL"] =
            json!("http://gateway:8080");
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["admin-bootstrap"]["command"] = json!(["admin-create --password=withheld"]);
        assert!(admit(&v).is_err());
        let mut v = fixture();
        v["services"]["gateway"]["environment"]
            .as_object_mut()
            .unwrap()
            .remove("AI_PLATFORM_API_EDDSA_CONFIG_FILE");
        assert!(admit(&v).is_err());
    }
}
