//! Knowledge domain implementation and explicit process support ports.
//! No Gateway host, policy authority or dispatcher assembly is constructed here.
pub mod migration_knowledge_ingest;
pub mod migration_knowledge_query;
pub mod migration_content;
pub mod migration_adapters;
pub mod observation_redaction;
pub mod host;
pub use ai_platform_service_primitives::{provider_cipher, provider_service};
