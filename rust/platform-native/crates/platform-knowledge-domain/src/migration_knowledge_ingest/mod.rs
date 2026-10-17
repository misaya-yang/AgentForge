//! Native knowledge write path. Constructors are inert; caller supplies database,
//! storage, parser executables and the canonical vector indexer explicitly.
//! SQL remains on the existing knowledge schema and retains generation fences.
pub mod admission;
pub mod artifact_sql;
pub mod assets;
pub mod batch_sql;
pub mod batch_upload;
pub mod batches;
pub mod catalog_http;
pub mod chunk_manager;
pub mod chunking;
pub mod dataset_reads;
pub mod dataset_writes;
pub mod detection;
mod diagnostics;
mod display_url;
pub mod document_reads;
pub mod document_writes;
mod docx;
pub mod hierarchy;
pub mod http;
pub mod images;
pub mod inspection_http;
mod iso_datetime;
pub mod legacy_images;
pub mod metadata;
pub mod ocr;
pub mod parser;
pub mod pdf_splitter;
pub mod progress;
pub mod publication;
mod reextraction;
mod request_validation;
pub mod service;
pub mod special;
pub mod special_store;
pub mod sql;
pub mod store;
pub mod streaming;
pub mod structured;
pub mod summary;
pub mod version_reads;
pub mod versions;
pub mod vision;
pub mod vlm;
pub mod worker;

pub use crate::migration_knowledge_query::types::{Chunk, Dataset};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
pub use service::IngestionService;
use sha2::{Digest, Sha256};
use std::fmt;
pub use store::KnowledgeWriteStore;

pub use ai_platform_service_primitives::pdf_ports::{IngestError,Result};

pub use crate::migration_knowledge_query::types::UserContext as Actor;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingMode {
    Auto,
    TextOnly,
    Scanned,
    Multimodal,
}
impl ProcessingMode {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "auto" => Self::Auto,
            "scanned" => Self::Scanned,
            "multimodal" => Self::Multimodal,
            _ => Self::TextOnly,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::TextOnly => "text_only",
            Self::Scanned => "scanned",
            Self::Multimodal => "multimodal",
        }
    }
}
pub fn object(v: &Value) -> Value {
    if v.is_object() {
        v.clone()
    } else if let Some(s) = v.as_str() {
        serde_json::from_str::<Value>(s)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}))
    } else {
        json!({})
    }
}
pub fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn integer(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn stable_segment_id(document_id: &str, content_type: &str, position: i32) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("ai-platform:kb-segment:{document_id}:{content_type}:{position}").as_bytes(),
    )
    .to_string()
}
pub fn stable_index_node_id(document_id: &str, content_type: &str, position: i32) -> String {
    format!("{document_id}::{content_type}::{position}")
}
pub fn dataset_ingestion_identity(dataset: &Value) -> String {
    let mut index = object(&dataset["index_config"]);
    if let Some(m) = index.as_object_mut() {
        for key in ["retrieval", "document_metadata_registry"] {
            m.remove(key);
        }
    }
    // Canonicalize recursively: this workspace enables serde_json preserve_order.
    let payload = json!({"tenant_id":string(dataset,"tenant_id"),"collection_name":string(dataset,"collection_name"),
        "embedding_provider":string(dataset,"embedding_provider"),"embedding_model":string(dataset,"embedding_model"),
        "embedding_dimension":integer(dataset,"embedding_dimension"),"embedding_config":object(&dataset["embedding_config"]),
        "ingestion_index_config":index});
    sha256(canonical_json(&payload).as_bytes())
}
pub fn require_index_writable(dataset: &Value) -> Result<()> {
    if !dataset["index_config"].is_null() && !dataset["index_config"].is_object() {
        return Err(IngestError::Validation(
            "dataset index_config is invalid".into(),
        ));
    }
    let index = object(&dataset["index_config"]);
    chunking::ChunkingConfig::from_value(&index["chunking"])?;
    crate::migration_knowledge_query::lexical_v2::LexicalProfile::from_dataset(
        &serde_json::from_value(dataset.clone())?,
    )?;
    if index["retrieval"].get("_index_deletion_fence").is_some() {
        return Err(IngestError::Validation(
            "dataset index deletion is pending; ingestion is unavailable".into(),
        ));
    }
    if dataset["is_deleted"].as_bool() == Some(true) {
        return Err(IngestError::NotFound);
    }
    Ok(())
}
pub fn require_public_metadata(metadata: &Value) -> Result<()> {
    if !metadata.is_object() {
        return Err(IngestError::Validation(
            "document metadata must be an object".into(),
        ));
    }
    if let Some(key) = metadata
        .as_object()
        .unwrap()
        .keys()
        .find(|key| source_metadata_key(key))
    {
        return Err(IngestError::Validation(format!(
            "metadata key '{key}' is reserved"
        )));
    }
    Ok(())
}
pub fn display_status(document: &Value) -> &'static str {
    if document["archived"].as_bool() == Some(true) {
        return "archived";
    }
    match string(document, "status").trim().to_lowercase().as_str() {
        "error" | "failed" => "error",
        "paused" => "paused",
        "completed" => {
            if document["enabled"].as_bool() == Some(false) {
                "disabled"
            } else {
                "available"
            }
        }
        "waiting" => "queuing",
        _ => "indexing",
    }
}

pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<_, _> = map.iter().collect();
            format!(
                "{{{}}}",
                sorted
                    .into_iter()
                    .map(|(k, v)| format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        canonical_json(v)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

pub const SOURCE_METADATA_KEYS: &[&str] = &[
    "_document_lifecycle_reindex",
    "_document_upload_generation",
    "_document_upload_failed",
    "_confluence_sync_generation",
    "_document_ingest_action",
    "_document_recover_stage",
    "_document_pipeline_execution_id",
    "_document_pending_restore_version",
    "_document_restored_source_version",
    "_special_publication_generation_id",
    "_special_source_manifest",
    "_confluence_image_source_generation",
    "_confluence_attachment_manifest",
    "skipped_confluence_attachments",
    "original_file_key",
    "original_filename",
    "original_mime_type",
    "processing_mode",
    "extracted_images",
    "image_count",
    "images_embedded",
    "embedded_image_count",
    "ocr_processed",
    "structured_parsing",
];
pub fn source_metadata_key(key: &str) -> bool {
    SOURCE_METADATA_KEYS.contains(&key)
}
