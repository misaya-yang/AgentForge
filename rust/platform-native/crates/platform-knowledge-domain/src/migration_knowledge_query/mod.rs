//! Knowledge-owned native query/index implementation migrated from knowledge_service.
//! This module is inert until the host explicitly constructs and mounts its services.
//! PostgreSQL remains serving/ACL authority; Qdrant remains vector ownership authority.
pub mod auth;
pub mod config;
pub mod embedding;

pub mod index;
pub mod lexical;
pub mod lexical_v2;
pub mod parents;
pub mod qa;
mod qa_stream;
pub mod qdrant;
pub mod rerank;
pub mod service;
pub mod store;
pub mod types;
pub use embedding::EmbeddingManager;
pub use index::Indexer;
pub use qdrant::QdrantStore;
pub use service::RetrievalService;
pub use store::QueryStore;
pub use types::{Chunk, Dataset, IndexGeneration, QueryError, UserContext};


pub mod hierarchy;
pub mod lexical_http;
pub mod lexical_lifecycle;
pub mod migration;
pub mod migration_http;
pub mod migration_jobs;

pub mod source_authz;

pub mod retrieval_eval;

pub mod observability;

pub mod ragas;

pub mod pipeline_metrics;

pub mod actor;
pub mod http;
