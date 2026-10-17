//! Gateway Knowledge boundary: shared wire/signature contracts plus Gateway-owned
//! actor authority, signed remote clients and trace capture. No store/retriever is linked.
pub use ai_platform_gateway_integrations::migration_knowledge_query::{auth,types};
pub use types::{Chunk,Dataset,IndexGeneration,QueryError,UserContext};
pub mod actor;
pub mod rag_trace;
