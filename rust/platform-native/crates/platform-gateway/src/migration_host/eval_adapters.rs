//! Real Gateway authorities behind the independent eval service ports.
use crate::{
    knowledge::{
        DocumentSourceReference as GatewayReference, KnowledgeActor as GatewayActor,
        KnowledgeService,
    },
    migration_agent::{Actor, Scope, routes::AgentState},
};
use ai_platform_auth::{api_keys::ApiKeyAuthority, service::Principal};
use ai_platform_gateway_eval::{
    migration_compat::common::{CompatError, CompatUser, object, text},
    ports::*,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};
pub struct KnowledgeAdapter(pub Arc<KnowledgeService>);
struct Reader {
    service: Arc<KnowledgeService>,
    actor: GatewayActor,
}
fn denied() -> CompatError {
    CompatError::new(403, "Knowledge dataset is not accessible")
}
impl KnowledgeAuthority for KnowledgeAdapter {
    fn from_principal(&self, p: &Principal) -> Result<KnowledgeActor, CompatError> {
        Ok(KnowledgeActor(Arc::new(Reader {
            service: self.0.clone(),
            actor: GatewayActor::from_principal(p).map_err(|_| denied())?,
        })))
    }
    fn from_api_key(&self, p: &ApiKeyAuthority) -> Result<KnowledgeActor, CompatError> {
        Ok(KnowledgeActor(Arc::new(Reader {
            service: self.0.clone(),
            actor: GatewayActor::from_api_key(p).map_err(|_| denied())?,
        })))
    }
}
impl KnowledgeReader for Reader {
    fn visible_dataset_ids(&self) -> PortFuture<'_, BTreeSet<String>> {
        Box::pin(async move {
            self.service
                .visible_dataset_ids(&self.actor)
                .await
                .map_err(|_| denied())
        })
    }
    fn authorize_document_sources<'a>(
        &'a self,
        dataset: &'a str,
        refs: &'a [DocumentSourceReference],
    ) -> PortFuture<'a, BTreeSet<DocumentSourceReference>> {
        Box::pin(async move {
            let refs = refs
                .iter()
                .map(|r| GatewayReference {
                    document_id: r.document_id.clone(),
                    source_version: r.source_version,
                    source_hash: r.source_hash.clone(),
                })
                .collect::<Vec<_>>();
            Ok(self
                .service
                .authorize_document_sources(&self.actor, dataset, &refs)
                .await
                .map_err(|_| denied())?
                .into_iter()
                .map(|r| DocumentSourceReference {
                    document_id: r.document_id,
                    source_version: r.source_version,
                    source_hash: r.source_hash,
                })
                .collect())
        })
    }
}
impl AgentPort for AgentState {
    fn resolve_agent_version_candidate<'a>(
        &'a self,
        user: &'a CompatUser,
        agent: &'a str,
        version: &'a str,
    ) -> PortFuture<'a, Value> {
        Box::pin(async move {
            if !user.authenticated || user.user_id.is_empty() {
                return Err(CompatError::new(403, "Authenticated Agent viewer required"));
            }
            let actor = Actor {
                tenant_id: user.tenant_id.clone(),
                user_id: user.user_id.clone(),
                roles: user.roles.clone(),
                tier: user.tier.clone(),
            };
            let resolution = self
                .repository
                .resolve_version(&actor, agent, version)
                .await
                .map_err(|e| CompatError::value(e.status, e.detail))?;
            let snapshot = self
                .build_snapshot(&actor, &resolution, "preview")
                .await
                .map_err(|e| CompatError::value(e.status, e.detail))?;
            let spec = text(&resolution["version"], "spec_hash");
            if snapshot["agent_id"] != agent
                || snapshot["agent_version_id"] != version
                || spec.is_empty()
                || text(&snapshot["fingerprints"], "spec").trim_start_matches("sha256:") != spec
            {
                return Err(CompatError::new(
                    409,
                    "Agent Version snapshot identity mismatch",
                ));
            }
            if text(&snapshot["model"], "id").is_empty()
                || text(&snapshot["model"], "provider").is_empty()
            {
                return Err(CompatError::new(409, "Agent Version model unavailable"));
            }
            let mut datasets = snapshot["knowledge"]["datasets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            datasets.sort();
            Ok(
                json!({"candidate_type":"agent_version","agent_id":agent,"agent_version_id":version,"agent_spec_hash":spec,"agent_runtime_snapshot_hash":crate::migration_agent::spec::hash(&snapshot),"model_id":snapshot["model"]["id"],"provider_id":snapshot["model"]["provider"],"knowledge_dataset_ids":datasets}),
            )
        })
    }
    fn interrupt<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        session: &'a str,
        thread: &'a str,
        turn: &'a str,
    ) -> PortFuture<'a, ()> {
        Box::pin(async move {
            self.control
                .interrupt_turn(
                    &Scope {
                        tenant_id: tenant.into(),
                        user_id: user.into(),
                        session_id: session.into(),
                    },
                    thread,
                    turn,
                    "eval_run_cancelled",
                )
                .await
                .map(|_| ())
                .map_err(|e| CompatError::value(e.status, e.detail))
        })
    }
}

/// Reuses the same workload identity and exact peer pinning as Runtime/Knowledge
/// dispatch, while the eval client retains its scoped HMAC and model authority.
pub struct PrivateTransport;
impl PrivateHttp for PrivateTransport {
    fn execute(
        &self,
        client: reqwest::Client,
        request: reqwest::Request,
    ) -> PortFuture<'_, reqwest::Response> {
        use crate::migration_agent::control::execution_transport::WorkloadSend;
        Box::pin(async move {
            reqwest::RequestBuilder::from_parts(client, request)
                .workload_send()
                .await
                .map_err(|_| CompatError::new(502, "Private eval service unavailable"))
        })
    }
}
