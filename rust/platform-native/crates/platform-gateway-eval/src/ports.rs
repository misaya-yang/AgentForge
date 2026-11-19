//! Verified cross-domain ports. Callers supply current authenticated authority;
//! public identity headers cannot construct a Knowledge actor.
use crate::migration_compat::common::{CompatError, CompatUser};
use ai_platform_auth::{api_keys::ApiKeyAuthority, service::Principal};
use serde_json::Value;
use std::{collections::BTreeSet, future::Future, pin::Pin, sync::Arc};
pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CompatError>> + Send + 'a>>;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DocumentSourceReference {
    pub document_id: String,
    pub source_version: i64,
    pub source_hash: String,
}
#[derive(Clone)]
pub struct KnowledgeActor(pub Arc<dyn KnowledgeReader>);
pub trait KnowledgeReader: Send + Sync {
    fn visible_dataset_ids(&self) -> PortFuture<'_, BTreeSet<String>>;
    fn authorize_document_sources<'a>(
        &'a self,
        dataset: &'a str,
        refs: &'a [DocumentSourceReference],
    ) -> PortFuture<'a, BTreeSet<DocumentSourceReference>>;
}
pub trait KnowledgeAuthority: Send + Sync {
    fn from_principal(&self, principal: &Principal) -> Result<KnowledgeActor, CompatError>;
    fn from_api_key(&self, key: &ApiKeyAuthority) -> Result<KnowledgeActor, CompatError>;
}
pub trait AgentPort: Send + Sync {
    fn resolve_agent_version_candidate<'a>(
        &'a self,
        user: &'a CompatUser,
        agent: &'a str,
        version: &'a str,
    ) -> PortFuture<'a, Value>;
    fn interrupt<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        session: &'a str,
        thread: &'a str,
        turn: &'a str,
    ) -> PortFuture<'a, ()>;
}

/// The host supplies its authenticated private transport; eval never owns a
/// workload identity or recovers a retired transport bearer from signing keys.
pub trait PrivateHttp: Send + Sync {
    fn execute(
        &self,
        client: reqwest::Client,
        request: reqwest::Request,
    ) -> PortFuture<'_, reqwest::Response>;
}
