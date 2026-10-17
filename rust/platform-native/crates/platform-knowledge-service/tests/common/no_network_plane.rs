//! Inert graph fixture: any model dispatch fails before network I/O. This fixture
//! proves constructor/HTTP-boundary behavior, never model or billing quality.
use ai_platform_knowledge_service::migration_knowledge_query::model_plane::ModelPlaneClient;
use ai_platform_service_primitives::knowledge_model::{
    KnowledgeWorkloadSigner, ModelLease, ServiceModelLeaseVerifier,
};
use std::sync::Arc;
struct Deny;
impl KnowledgeWorkloadSigner for Deny {
    fn sign(&self, _: &str, _: &[u8], _: &str, _: &str) -> Result<String, &'static str> {
        Err("inert_graph_fixture_forbids_model_dispatch")
    }
}
impl ServiceModelLeaseVerifier for Deny {
    fn verify(&self, _: &str, _: &ModelLease) -> Result<(), &'static str> {
        Err("inert_graph_fixture_has_no_model_leases")
    }
}
pub fn client() -> Arc<ModelPlaneClient> {
    Arc::new(
        ModelPlaneClient::for_workload(
            "http://127.0.0.1:1".parse().unwrap(),
            Arc::new(Deny),
            Arc::new(Deny),
        )
        .unwrap(),
    )
}
