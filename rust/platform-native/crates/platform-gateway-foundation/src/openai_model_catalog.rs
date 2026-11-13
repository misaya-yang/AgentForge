//! Tenant-scoped projection of the existing model/provider catalog into
//! dispatcher-only OpenAI-compatible services. Implementations must return
//! enabled models whose provider configuration is currently usable. Returned
//! service handles may contain provider credentials and must stay inside the
//! in-memory dispatcher path; they are never persisted or exposed by service
//! administration APIs.
use crate::service_ownership::ServiceHandle;
use std::{future::Future, pin::Pin};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenAiModelCatalogUnavailable;

pub type OpenAiModelCatalogFuture<'a> = Pin<
    Box<dyn Future<Output = Result<Vec<ServiceHandle>, OpenAiModelCatalogUnavailable>> + Send + 'a>,
>;

/// Builds a fresh view per request so provider/model CRUD is visible without
/// stale credentials, deleted models, or cached access decisions.
pub trait OpenAiModelCatalog: Send + Sync {
    fn services_for_tenant<'a>(&'a self, tenant_id: &'a str) -> OpenAiModelCatalogFuture<'a>;
}
