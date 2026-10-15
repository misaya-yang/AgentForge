use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{collections::BTreeSet, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    Validation(String),
    Forbidden,
    NotFound,
    Database,
    Provider(u16),
    InvalidProviderResponse,
    VectorStore,
    CollectionAuthority(String),
    GenerationChanged,
    PublicationInProgress,
    DependenciesFailed,
}
impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(m) | Self::CollectionAuthority(m) => f.write_str(m),
            Self::Forbidden => f.write_str("Missing dataset permission"),
            Self::NotFound => f.write_str("Not Found"),
            Self::Database => f.write_str("knowledge database is unavailable"),
            Self::Provider(s) => write!(f, "knowledge provider returned HTTP {s}"),
            Self::InvalidProviderResponse => f.write_str("invalid knowledge provider response"),
            Self::VectorStore => f.write_str("knowledge vector store is unavailable"),
            Self::GenerationChanged => {
                f.write_str("dataset index generation changed during retrieval")
            }
            Self::PublicationInProgress => f.write_str("dataset index publication is in progress"),
            Self::DependenciesFailed => f.write_str("knowledge retrieval dependencies failed"),
        }
    }
}
impl std::error::Error for QueryError {}
impl From<sqlx::Error> for QueryError {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}
pub type Result<T> = std::result::Result<T, QueryError>;
pub fn object() -> Value {
    Value::Object(Map::new())
}
fn yes() -> bool {
    true
}
fn text() -> String {
    "text".into()
}
fn normal() -> String {
    "normal".into()
}
fn user() -> String {
    "user".into()
}
fn private() -> String {
    "private".into()
}

/// Construct only from verified gateway/service identity, never unsigned public headers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserContext {
    pub user_id: String,
    pub tenant_id: String,
    #[serde(default = "normal")]
    pub user_tier: String,
    #[serde(default = "user")]
    pub user_type: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default = "yes")]
    pub is_authenticated: bool,
}
impl UserContext {
    pub fn anonymous(&self) -> bool {
        !self.is_authenticated
            || self.user_id.trim().eq_ignore_ascii_case("anonymous")
            || self.user_tier.eq_ignore_ascii_case("anonymous")
            || ["anonymous", "guest"].contains(&self.user_type.to_ascii_lowercase().as_str())
            || self.roles.iter().any(|r| r.eq_ignore_ascii_case("guest"))
    }
    pub fn tenant_admin(&self) -> bool {
        !self.anonymous()
            && (self.user_tier == "admin"
                || self
                    .roles
                    .iter()
                    .any(|r| r == "admin" || r == "tenant_admin"))
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Dataset {
    pub dataset_id: String,
    pub tenant_id: String,
    pub name: String,
    pub created_by: Option<String>,
    pub owner_user_id: Option<String>,
    #[serde(default = "private")]
    pub visibility: String,
    pub is_deleted: bool,
    pub content_revision: Option<i64>,
    pub collection_name: Option<String>,
    pub embedding_provider: Option<String>,
    pub embedding_model: Option<String>,
    pub embedding_model_version: Option<String>,
    pub embedding_dimension: Option<usize>,
    #[serde(default = "object")]
    pub embedding_config: Value,
    #[serde(default = "object")]
    pub index_config: Value,
    pub needs_reindex: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
impl Dataset {
    pub fn generation(&self) -> Result<IndexGeneration> {
        if self.index_config["retrieval"]
            .get("_index_deletion_fence")
            .is_some()
        {
            return Err(QueryError::Validation(
                "dataset index deletion is pending; retrieval is unavailable".into(),
            ));
        }
        if self.content_revision.is_some_and(|r| r < 0) {
            return Err(QueryError::PublicationInProgress);
        }
        Ok(IndexGeneration {
            tenant_id: self.tenant_id.trim().into(),
            content_revision: self.content_revision,
            collection_name: self
                .collection_name
                .clone()
                .unwrap_or_default()
                .trim()
                .into(),
            embedding_provider: self
                .embedding_provider
                .clone()
                .unwrap_or_default()
                .trim()
                .to_lowercase(),
            embedding_model: self
                .embedding_model
                .clone()
                .unwrap_or_default()
                .trim()
                .to_lowercase(),
            embedding_model_version: self
                .embedding_model_version
                .clone()
                .unwrap_or_default()
                .trim()
                .into(),
            embedding_dimension: self.embedding_dimension.unwrap_or(0),
        })
    }
    pub fn collection(&self) -> Result<&str> {
        self.collection_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                QueryError::Validation(
                    "dataset retrieval requires a persisted Qdrant collection".into(),
                )
            })
    }
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGeneration {
    pub tenant_id: String,
    pub content_revision: Option<i64>,
    pub collection_name: String,
    pub embedding_provider: String,
    pub embedding_model: String,
    pub embedding_model_version: String,
    pub embedding_dimension: usize,
}

/// Canonical SQL `segments` row; unknown nullable columns survive round trips.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Chunk {
    pub segment_id: String,
    pub dataset_id: String,
    pub document_id: String,
    pub position: i32,
    pub level: i32,
    pub parent_segment_id: Option<String>,
    pub summary: Option<String>,
    pub page_start: Option<i32>,
    pub page_end: Option<i32>,
    pub text: String,
    pub token_count: i32,
    pub vector_id: Option<String>,
    #[serde(default = "object")]
    pub metadata: Value,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub status: String,
    pub word_count: i32,
    pub keywords: Value,
    pub answer: Option<String>,
    pub created_by: Option<String>,
    pub content_hash: Option<String>,
    pub source_type: Option<String>,
    pub source_reference: Value,
    pub citation_text: Option<String>,
    pub page_number: Option<i32>,
    pub section_header: Option<String>,
    pub language: Option<String>,
    pub contextual_prefix: Option<String>,
    #[serde(default = "text")]
    pub content_type: String,
    pub index_node_id: Option<String>,
    pub index_node_hash: Option<String>,
    pub image_url: Option<String>,
    pub image_attachment_id: Option<String>,
    pub image_filename: Option<String>,
    pub image_media_type: Option<String>,
    pub image_file_size: Option<i64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
impl Chunk {
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetrieveResult {
    pub segment_id: String,
    pub document_id: String,
    pub score: f64,
    pub text: String,
    pub metadata: Value,
    #[serde(default = "text")]
    pub content_type: String,
    pub image_url: Option<String>,
    pub vlm_description: Option<String>,
    #[serde(default)]
    pub associated_images: Vec<Value>,
}
#[derive(Clone, Debug)]
pub struct Candidate {
    pub segment_id: String,
    pub document_id: String,
    pub text: String,
    pub metadata: Value,
    pub point_id: Option<String>,
    pub dense_score: Option<f64>,
    pub bm25_score: Option<f64>,
    pub fusion_score: Option<f64>,
    pub rerank_score: Option<f64>,
    pub final_score: f64,
    pub sources: BTreeSet<String>,
}
impl Candidate {
    pub fn from_chunk(c: Chunk) -> Self {
        Self {
            segment_id: c.segment_id,
            document_id: c.document_id,
            text: c.text,
            metadata: c.metadata,
            point_id: c.vector_id,
            dense_score: None,
            bm25_score: None,
            fusion_score: None,
            rerank_score: None,
            final_score: 0.0,
            sources: BTreeSet::new(),
        }
    }
}

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use serde_json::json;
impl IntoResponse for QueryError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Validation(_) | Self::CollectionAuthority(_) => StatusCode::BAD_REQUEST,
            Self::GenerationChanged | Self::PublicationInProgress => StatusCode::CONFLICT,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        let message = self.to_string();
        if status == StatusCode::CONFLICT {
            (
                status,
                [("Retry-After", "1")],
                Json(json!({"detail":message})),
            )
                .into_response()
        } else {
            (status, Json(json!({"detail":message}))).into_response()
        }
    }
}
