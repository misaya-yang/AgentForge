//! Workload transport assembly uses only explicit, private deployment inputs.
use super::BootstrapError;
use ai_platform_auth::workload::WorkloadAuth;
use ai_platform_service_primitives::workload_identity::{Mode, PeerRules, ServiceIdentity};
use std::{collections::BTreeMap, sync::Arc};

pub fn configure_outbound(environment: &BTreeMap<String, String>) -> Result<(), BootstrapError> {
    crate::migration_agent::control::execution_transport::configure_environment(environment)
        .map_err(BootstrapError::Configuration)?;
    ai_platform_capability_contract::broker_proof::configure_environment(environment)
        .map_err(BootstrapError::Configuration)
}
pub async fn configuration(
    environment: &BTreeMap<String, String>,
) -> Result<Option<(crate::workload_tls::Configuration, Arc<WorkloadAuth>)>, BootstrapError> {
    configure_outbound(environment)?;
    let Some(configuration) =
        crate::workload_tls::Configuration::resolve_from_environment(environment)
            .await
            .map_err(BootstrapError::Configuration)?
    else {
        return Ok(None);
    };
    let peers = environment
        .get("AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS")
        .ok_or(BootstrapError::Configuration(
            "workload_peer_allowlist_required",
        ))?
        .split(',')
        .map(ServiceIdentity::parse)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| BootstrapError::Configuration("workload_peer_allowlist_invalid"))?;
    let rules = PeerRules::new(peers)
        .map_err(|_| BootstrapError::Configuration("workload_peer_allowlist_invalid"))?;
    let gate = if environment.contains_key("AI_PLATFORM_INTERNAL_TOKEN_FILE") {
        WorkloadAuth::from_environment(environment, rules)
    } else {
        let mode = Mode::parse(
            environment
                .get("AI_PLATFORM_WORKLOAD_MTLS_MODE")
                .map(String::as_str)
                .unwrap_or("disabled"),
        )
        .map_err(|_| BootstrapError::Configuration("workload_mode_invalid"))?;
        let secret = environment
            .get("AI_PLATFORM_INTERNAL_TOKEN")
            .filter(|v| !v.is_empty())
            .map(|v| v.as_bytes().to_vec());
        WorkloadAuth::from_configuration(mode, secret, rules)
    }
    .map_err(|_| BootstrapError::Configuration("workload_authentication_invalid"))?;
    let knowledge = environment
        .get("AI_PLATFORM_WORKLOAD_KNOWLEDGE_CLIENT_IDS")
        .map(|raw| {
            raw.split(',')
                .map(ServiceIdentity::parse)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()
        .map_err(|_| BootstrapError::Configuration("workload_knowledge_peers_invalid"))?
        .unwrap_or_default();
    let gate = gate
        .with_route_peers(
            dataset_paths().map(|path| ("POST".into(), path.into())),
            knowledge,
        )
        .map_err(|_| BootstrapError::Configuration("workload_knowledge_peers_invalid"))?;
    Ok(Some((configuration, Arc::new(gate))))
}

/// Only actual Gateway internal routes are exposed by the companion listener.
/// Their signed tenant/capability/grant handlers remain the original handlers.
pub fn routes() -> Result<crate::workload_tls::Routes, BootstrapError> {
    crate::workload_tls::Routes::new(
        [
            "/internal/v1/authorization/dataset",
            "/internal/v1/authorization/dataset/batch",
            "/internal/v1/agent-model-plane/responses",
            "/internal/v3/capability-broker/proofs",
            "/internal/v1/sdk-tools/capabilities/admit",
            "/internal/v1/sdk-tools/capabilities/execute",
            "/internal/v1/sdk-tools/knowledge/retrieve",
            "/internal/v1/sdk-tools/knowledge/admit",
            "/internal/v1/sdk-tools/models/lease",
            "/internal/v1/sdk-tools/models/execute",
            "/internal/v1/sdk-tools/models/revocations",
            "/internal/v2/agent-capabilities/catalog",
            "/internal/v2/agent-capabilities/knowledge-actor",
            "/internal/v2/agent-capabilities/mcp/read",
            "/internal/v2/agent-capabilities/confluence/read",
            "/internal/v2/agent-capabilities/confluence/write",
            "/internal/v2/agent-capabilities/office/artifacts",
            "/internal/v2/agent-capabilities/python/artifacts",
            "/internal/v2/agent-capabilities/python/execute",
            "/internal/v2/agent-capabilities/image/generate",
            "/internal/v2/agent-capabilities/attachments/read",
            "/internal/v2/agent-capabilities/web-search",
            "/internal/v2/agent-capabilities/artifacts/{id}/read",
            "/internal/v2/agent-capabilities/local-node/describe",
            "/internal/v2/agent-capabilities/local-node/dispatch",
            "/internal/v2/agent-capabilities/local-node/status",
            "/internal/v2/agent-capabilities/local-node/cancel",
            "/internal/v2/knowledge/model-leases",
            "/internal/v2/knowledge/model-leases/{lease_id}/execute",
        ]
        .into_iter()
        .map(|path| ("POST".into(), path.into())),
    )
    .map_err(BootstrapError::Configuration)
}

fn dataset_paths() -> impl Iterator<Item = &'static str> {
    [
        ai_platform_service_primitives::object_authorization::DATASET_DECISION_PATH,
        "/internal/v1/authorization/dataset/batch",
    ]
    .into_iter()
}

/// Health listeners expose readiness only and receive no business credentials.
/// Required profiles must name these origins explicitly instead of probing a
/// TLS business listener with an unauthenticated generic HTTP client.
pub(super) fn health_origin(
    environment: &BTreeMap<String, String>,
    name: &'static str,
    legacy: Option<&str>,
    required: bool,
) -> Result<Option<String>, BootstrapError> {
    let explicit = environment
        .get(name)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty());
    if !required && explicit.is_none() {
        return Ok(legacy.map(str::to_owned));
    }
    let Some(raw) = explicit else {
        return if required && legacy.is_some() {
            Err(BootstrapError::Configuration(name))
        } else {
            Ok(None)
        };
    };
    let url = reqwest::Url::parse(raw).map_err(|_| BootstrapError::Configuration(name))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(BootstrapError::Configuration(name));
    }
    Ok(Some(url.as_str().trim_end_matches('/').into()))
}

#[cfg(test)]
mod health_origin_tests {
    use super::*;
    #[test]
    fn required_readiness_is_explicit_and_cannot_carry_credentials_or_business_paths() {
        let mut env = BTreeMap::new();
        assert!(health_origin(&env, "HEALTH", Some("https://runtime:8454"), true).is_err());
        assert_eq!(
            health_origin(&env, "HEALTH", Some("http://runtime:8094"), false).unwrap(),
            Some("http://runtime:8094".into())
        );
        assert_eq!(health_origin(&env, "HEALTH", None, true).unwrap(), None);
        env.insert("HEALTH".into(), "http://runtime:8094".into());
        assert_eq!(
            health_origin(&env, "HEALTH", Some("https://runtime:8454"), true).unwrap(),
            Some("http://runtime:8094".into())
        );
        for value in [
            "http://token@runtime:8094",
            "http://runtime:8094/internal",
            "http://runtime:8094/?secret=x",
            "http://runtime:8094/#x",
            "file:///tmp/health",
        ] {
            env.insert("HEALTH".into(), value.into());
            assert!(health_origin(&env, "HEALTH", Some("https://runtime:8454"), true).is_err());
        }
    }
}
