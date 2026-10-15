//! Internal protocol/client compilation boundary, composed by the existing host.
pub use ai_platform_gateway_foundation::{dispatcher, generic_rest_adapter, metrics_redaction, protocol, service_ownership, service_registry};
pub(crate) use ai_platform_gateway_foundation::python_json::python_json;
pub mod migration_adapters;
pub mod migration_local_node;
pub mod chatgpt_plan;

pub mod observation_redaction;
pub use ai_platform_gateway_foundation::{provider_cipher, provider_service};
pub mod migration_content;
pub mod migration_knowledge_query;
pub mod migration_knowledge_ingest;
