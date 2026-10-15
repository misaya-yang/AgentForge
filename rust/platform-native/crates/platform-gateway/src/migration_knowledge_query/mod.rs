//! Knowledge-owned native query/index implementation migrated from knowledge_service.
//! This module is inert until the host explicitly constructs and mounts its services.
//! PostgreSQL remains serving/ACL authority; Qdrant remains vector ownership authority.
pub use ai_platform_gateway_integrations::migration_knowledge_query::auth;
pub use ai_platform_gateway_integrations::migration_knowledge_query::config;
pub use ai_platform_gateway_integrations::migration_knowledge_query::embedding;
pub mod http;
pub use ai_platform_gateway_integrations::migration_knowledge_query::index;
pub use ai_platform_gateway_integrations::migration_knowledge_query::lexical;
pub use ai_platform_gateway_integrations::migration_knowledge_query::lexical_v2;
pub use ai_platform_gateway_integrations::migration_knowledge_query::parents;
pub use ai_platform_gateway_integrations::migration_knowledge_query::qa;
pub use ai_platform_gateway_integrations::migration_knowledge_query::qdrant;
pub use ai_platform_gateway_integrations::migration_knowledge_query::rerank;
pub use ai_platform_gateway_integrations::migration_knowledge_query::service;
pub use ai_platform_gateway_integrations::migration_knowledge_query::store;
pub use ai_platform_gateway_integrations::migration_knowledge_query::types;
pub use embedding::EmbeddingManager;
pub use index::Indexer;
pub use qdrant::QdrantStore;
pub use service::RetrievalService;
pub use store::QueryStore;
pub use types::{Chunk, Dataset, IndexGeneration, QueryError, UserContext};

pub mod actor;
pub use ai_platform_gateway_integrations::migration_knowledge_query::hierarchy;
pub use ai_platform_gateway_integrations::migration_knowledge_query::lexical_http;
pub use ai_platform_gateway_integrations::migration_knowledge_query::lexical_lifecycle;
pub use ai_platform_gateway_integrations::migration_knowledge_query::migration;
pub use ai_platform_gateway_integrations::migration_knowledge_query::migration_http;
pub use ai_platform_gateway_integrations::migration_knowledge_query::migration_jobs;
pub mod rag_trace;
pub use ai_platform_gateway_integrations::migration_knowledge_query::source_authz;

pub use ai_platform_gateway_integrations::migration_knowledge_query::retrieval_eval;

pub use ai_platform_gateway_integrations::migration_knowledge_query::observability;

pub use ai_platform_gateway_integrations::migration_knowledge_query::ragas;
