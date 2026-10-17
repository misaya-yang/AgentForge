//! Public LLM surfaces bind only operator-selected existing service policies.
use super::{BootstrapError, graph::GatewayGraph};
use crate::llm_api::{LlmApi, LlmBindings, LlmRouteBinding, LlmRoutingBindings};
use serde::Deserialize;
use std::collections::BTreeMap;

const FILE: &str = "AI_PLATFORM_LLM_API_BINDINGS_FILE";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: String,
    chat_service: String,
    embeddings_service: String,
    messages_service: String,
    #[serde(default)]
    routing: Vec<LlmRouteBinding>,
}
pub(super) struct Configuration(LlmBindings, LlmRoutingBindings);
impl Configuration {
    pub(super) fn from_environment(
        environment: &BTreeMap<String, String>,
        api_key_header: &str,
    ) -> Result<Option<Self>, BootstrapError> {
        let Some(path) = environment.get(FILE) else {
            return Ok(None);
        };
        Self::parse(&super::auth::key_file(path, false)?, api_key_header).map(Some)
    }
    fn parse(raw: &str, api_key_header: &str) -> Result<Self, BootstrapError> {
        let invalid = || BootstrapError::Configuration(FILE);
        let document: Document = serde_json::from_str(raw).map_err(|_| invalid())?;
        let header =
            axum::http::HeaderName::from_bytes(api_key_header.as_bytes()).map_err(|_| invalid())?;
        if document.schema != "agentforge/llm-api-bindings/v1"
            || header == axum::http::header::AUTHORIZATION
            || [
                &document.chat_service,
                &document.embeddings_service,
                &document.messages_service,
            ]
            .iter()
            .any(|id| {
                id.is_empty()
                    || id.len() > 255
                    || id.trim() != id.as_str()
                    || id.chars().any(char::is_control)
            })
        {
            return Err(invalid());
        }
        let routing = LlmRoutingBindings::new(document.routing).map_err(|_| invalid())?;
        Ok(Self(
            LlmBindings {
                chat_service: document.chat_service,
                embeddings_service: document.embeddings_service,
                messages_service: document.messages_service,
                api_key_header: header,
            },
            routing,
        ))
    }
    pub(super) fn router(self, graph: &GatewayGraph) -> Result<axum::Router, BootstrapError> {
        let api = LlmApi::new_with_routing(
            graph.admission.clone(),
            graph.pools.data.clone(),
            graph.core.preflight.clone(),
            graph.core.dispatch_admission.clone(),
            graph.core.capacity.clone(),
            graph.models.providers.clone(),
            self.0,
            self.1,
        )
        .map_err(|_| BootstrapError::Construction("public_llm_api"))?;
        let policies = crate::migration_host::routing_policy_v1::RoutingPolicyService::new(
            graph.pools.data.clone(),
        );
        Ok(crate::llm_api::router(api)
            .merge(crate::migration_host::routing_policy_v1::http::router(
                policies,
                graph.admission.clone(),
            ))
            .merge(crate::llm_api::guardrails::router(
                crate::llm_api::guardrails::GuardrailService::new(graph.pools.data.clone()),
                graph.admission.clone(),
            )))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bindings_are_explicit_policy_references_and_never_use_bearer_as_api_key_header() {
        assert!(
            Configuration::from_environment(&BTreeMap::new(), "x-api-key")
                .unwrap()
                .is_none()
        );
        let good = serde_json::json!({"schema":"agentforge/llm-api-bindings/v1","chat_service":"existing-chat","embeddings_service":"existing-embedding","messages_service":"existing-messages"});
        assert!(Configuration::parse(&good.to_string(), "x-api-key").is_ok());
        assert!(Configuration::parse(&good.to_string(), "authorization").is_err());
        let mut unknown = good.clone();
        unknown["provider_key"] = serde_json::json!("forbidden");
        assert!(Configuration::parse(&unknown.to_string(), "x-api-key").is_err());
        for value in ["", " wrong", "wrong\n"] {
            let mut bad = good.clone();
            bad["chat_service"] = serde_json::json!(value);
            assert!(Configuration::parse(&bad.to_string(), "x-api-key").is_err());
        }
    }
}
