//! Offline, dependency-free OpenAPI contract comparison and source profiles.
//! This preserves bounded source diagnostics, not complete schema equivalence.
pub mod gate;
pub mod profiles;

pub mod cli;

pub mod snapshot;

pub mod kb_boundary;
pub mod kb_release;
pub mod provider_contract;
pub mod skills_contract;

pub mod document;

pub mod arc01;

pub use document::stream_schema;
pub mod breaking;
pub mod release;

pub mod validation;

pub mod codegen;
