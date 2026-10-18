use super::{
    embedding::EmbeddingManager,
    lexical,
    lexical_v2::LexicalProfile,
    qdrant::{QdrantStore, id_string},
    store::QueryStore,
    types::{Chunk, Dataset, QueryError, Result},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Vector half of publication. The ingestion owner holds the PostgreSQL lease,
/// persists a negative revision and activates SQL rows only after this returns.
#[derive(Clone)]
pub struct Indexer {
    pub store: QueryStore,
    pub vectors: QdrantStore,
    pub embeddings: EmbeddingManager,
    v2_promotions: super::retrieval_v2::Promotions,
}
impl Indexer {
    pub fn new(store: QueryStore, vectors: QdrantStore, embeddings: EmbeddingManager) -> Self {
        Self {
            store,
            vectors,
            embeddings,
            v2_promotions: super::retrieval_v2::Promotions::from_server_environment(),
        }
    }
    pub(crate) fn contextual_headers_enabled(&self, dataset: &Dataset) -> Result<bool> {
        let profile = super::retrieval_v2::Profile::from_dataset(dataset)?;
        if let Some(profile) = &profile {
            self.v2_promotions.require(profile, dataset)?;
        }
        Ok(profile.as_ref().is_some_and(|p| p.contextual_headers))
    }
    pub async fn index_chunks(&self, dataset: &Dataset, chunks: &[Chunk]) -> Result<Vec<String>> {
        let points = self.prepare_chunks(dataset, chunks).await?;
        self.publish_prepared(dataset, &points).await
    }
    pub async fn prepare_chunks(
        &self,
        dataset: &Dataset,
        chunks: &[Chunk],
    ) -> Result<BTreeMap<String, Vec<Value>>> {
        if chunks.is_empty() {
            return Ok(BTreeMap::new());
        }
        for chunk in chunks {
            if chunk.dataset_id != dataset.dataset_id
                || chunk.document_id.trim().is_empty()
                || chunk.segment_id.trim().is_empty()
            {
                return Err(QueryError::Validation(
                    "index chunk escaped dataset/document scope".into(),
                ));
            }
            if chunk.content_type == "image" {
                return Err(QueryError::Validation(
                    "image chunks require image bytes and the image embedding adapter".into(),
                ));
            }
        }
        let contextual = self.contextual_headers_enabled(dataset)?;
        let resolved = self.embeddings.resolve(dataset).await?;
        let base = dataset
            .collection_name
            .clone()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                QueryError::Validation("dataset collection is required before embedding".into())
            })?;
        let texts: Vec<String> = chunks
            .iter()
            .map(|chunk| {
                if contextual {
                    contextual_embedding_text(chunk)
                } else {
                    embedding_text(chunk)
                }
            })
            .collect();
        let vectors = self
            .embeddings
            .embed_resolved(&resolved, &texts, "document")
            .await?;
        if vectors.len() != chunks.len() {
            return Err(QueryError::InvalidProviderResponse);
        }
        let mut by_collection = BTreeMap::<String, Vec<Value>>::new();
        for ((chunk, text), vector) in chunks.iter().zip(texts).zip(vectors) {
            let collection = match chunk.level {
                1 => format!("{base}_summary"),
                2 => format!("{base}_sections"),
                _ => base.clone(),
            };
            let id = chunk
                .vector_id
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(&chunk.segment_id)
                .to_owned();
            let mut payload_meta = serde_json::Map::new();
            for key in [
                "source_type",
                "citation_text",
                "source_reference",
                "section_title",
                "section_full_path",
                "page_number",
                "chunk_index",
                "paragraph_index",
                "source_document",
                "document_title",
                "madhab",
                "language",
                "docir_v2",
                "section_path",
                "structure",
                "region_crop",
                "caption",
                "vlm_description",
            ] {
                if let Some(v) = chunk.metadata.get(key).filter(|v| !v.is_null()) {
                    payload_meta.insert(key.into(), v.clone());
                }
            }
            let mut payload = json!({"tenant_id":dataset.tenant_id,"dataset_id":dataset.dataset_id,"document_id":chunk.document_id,"segment_id":chunk.segment_id,"position":chunk.position,"text":if contextual { &chunk.text } else { &text },"token_count":chunk.token_count,"source_type":payload_meta.get("source_type").cloned().unwrap_or(json!("unknown")),"language":payload_meta.get("language").cloned().unwrap_or(json!("en")),"citation_text":payload_meta.get("citation_text"),"source_reference":payload_meta.get("source_reference"),"metadata":payload_meta});
            // Structured references are authoritative SQL chunk fields, not a display prefix.
            if chunk.source_reference["schema"] == "docir-publication-ref/v2" {
                payload["source_reference"] = chunk.source_reference.clone();
                payload["metadata"]["docir_v2"] = chunk.source_reference.clone();
            }
            if chunk.level > 0 {
                payload["level"] = json!(chunk.level)
            }
            if let Some(parent) = &chunk.parent_segment_id {
                payload["parent_segment_id"] = json!(parent)
            }
            if chunk.content_type != "text" {
                payload["content_type"] = json!(chunk.content_type)
            }
            if !chunk.enabled {
                payload["enabled"] = json!(false)
            }
            let lexical_text = if contextual { &chunk.text } else { &text };
            let (indices, values) = lexical::sparse_vector(lexical_text, false);
            let (indices, values) = if indices.is_empty() {
                (vec![0], vec![1.0])
            } else {
                (indices, values)
            };
            by_collection.entry(collection).or_default().push(json!({"id":id,"vector":{"":vector,"bm25":{"indices":indices,"values":values}},"payload":payload}));
        }
        Ok(by_collection)
    }
    pub async fn publish_prepared(
        &self,
        dataset: &Dataset,
        prepared: &BTreeMap<String, Vec<Value>>,
    ) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for (collection, points) in prepared {
            let mut scope = dataset.clone();
            scope.collection_name = Some(collection.clone());
            let dimension = points
                .first()
                .and_then(|p| p["vector"][""].as_array())
                .map(Vec::len)
                .or(dataset.embedding_dimension)
                .ok_or(QueryError::InvalidProviderResponse)?;
            self.vectors.ensure_collection(&scope, dimension).await?;
            self.vectors.upsert_index_points(&scope, points).await?;
            for p in points {
                ids.push(id_string(&p["id"]).ok_or(QueryError::VectorStore)?);
            }
        }
        Ok(ids)
    }
    pub async fn snapshot_points(
        &self,
        dataset: &Dataset,
        point_ids: &[String],
    ) -> Result<Vec<Value>> {
        self.vectors.snapshot_points(dataset, point_ids).await
    }
    pub async fn restore_points(&self, dataset: &Dataset, points: &[Value]) -> Result<()> {
        self.vectors.upsert_points(dataset, points).await
    }
    pub async fn delete_points(&self, dataset: &Dataset, point_ids: &[String]) -> Result<()> {
        self.vectors.delete_points(dataset, point_ids).await
    }
    pub async fn persist_rollback_backups(
        &self,
        dataset: &Dataset,
        points: &[Value],
        revision: i64,
    ) -> Result<()> {
        let mut backups = Vec::new();
        for p in points {
            let original = id_string(&p["id"]).ok_or(QueryError::VectorStore)?;
            let payload = &p["payload"];
            let document = payload["document_id"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| {
                    QueryError::Validation(
                        "serving point lacks document identity required for durable rollback"
                            .into(),
                    )
                })?;
            backups.push(json!({"id":backup_id(&dataset.dataset_id,&original),"vector":p["vector"],"payload":{"tenant_id":dataset.tenant_id,"dataset_id":dataset.dataset_id,"document_id":document,"segment_id":payload["segment_id"],"enabled":false,"_kb_index_rollback":{"publication_revision":revision.unsigned_abs(),"original_point_id":p["id"],"payload":payload}}}));
        }
        self.vectors.upsert_points(dataset, &backups).await
    }
    pub async fn load_rollback_backups(&self, dataset: &Dataset) -> Result<Vec<Value>> {
        let mut filter = QdrantStore::filter(dataset, None, false);
        filter["must"]
            .as_array_mut()
            .ok_or(QueryError::VectorStore)?
            .push(json!({"key":"enabled","match":{"value":false}}));
        filter["must_not"] = json!([{"is_empty":{"key":"_kb_index_rollback"}}]);
        let points = self.vectors.scroll(dataset, &filter, true).await?;
        let mut snapshots = Vec::new();
        for p in points {
            let receipt = &p["payload"]["_kb_index_rollback"];
            let original =
                id_string(&receipt["original_point_id"]).ok_or(QueryError::VectorStore)?;
            if id_string(&p["id"]) != Some(backup_id(&dataset.dataset_id, &original))
                || !receipt["payload"].is_object()
                || p["vector"].is_null()
            {
                return Err(QueryError::Validation(
                    "vector rollback backup receipt is malformed".into(),
                ));
            }
            let revision = receipt["publication_revision"]
                .as_u64()
                .ok_or(QueryError::VectorStore)?;
            let current = dataset.content_revision.unwrap_or(0).unsigned_abs();
            if revision < current || revision - current > 1_000_000_000 {
                return Err(QueryError::Validation(
                    "vector rollback backup belongs to another publication".into(),
                ));
            }
            snapshots.push(json!({"id":receipt["original_point_id"],"vector":p["vector"],"payload":receipt["payload"]}));
        }
        Ok(snapshots)
    }
    pub async fn clear_rollback_backups(&self, dataset: &Dataset) -> Result<()> {
        let points = self.load_rollback_backups(dataset).await?;
        let ids: Vec<String> = points
            .iter()
            .filter_map(|p| id_string(&p["id"]))
            .map(|id| backup_id(&dataset.dataset_id, &id))
            .collect();
        self.vectors.delete_points(dataset, &ids).await
    }
}
pub fn backup_id(dataset_id: &str, point_id: &str) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("ai-platform:kb-index-rollback:{dataset_id}:{point_id}").as_bytes(),
    )
    .to_string()
}
pub fn embedding_text(chunk: &Chunk) -> String {
    let prefix = chunk.contextual_prefix.as_deref().unwrap_or("");
    if prefix.is_empty() || chunk.text.starts_with(prefix) {
        chunk.text.clone()
    } else {
        format!("{prefix}\n\n{}", chunk.text)
    }
}

impl QdrantStore {
    pub async fn create_dataset_collection(
        &self,
        dataset: &Dataset,
        dimension: usize,
    ) -> Result<String> {
        self.ensure_collection_policy(dataset, dimension, false)
            .await
    }
    pub async fn delete_collection(&self, dataset: &Dataset) -> Result<()> {
        self.require_collection_readable(dataset).await?;
        self.delete_collection_scoped(dataset).await
    }
    pub async fn upsert_index_points(&self, dataset: &Dataset, points: &[Value]) -> Result<()> {
        self.upsert_points(dataset, points).await
    }
}

/// Keep the display body immutable. This header participates only in embedding input.
pub fn contextual_embedding_text(chunk: &Chunk) -> String {
    let bounded = |text: &str, count: usize| {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(count)
            .collect::<String>()
    };
    let title = chunk.metadata["document_title"]
        .as_str()
        .or(chunk.metadata["source_document"].as_str())
        .unwrap_or("");
    let path = chunk.metadata["section_path"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .take(6)
                .collect::<Vec<_>>()
                .join(" > ")
        })
        .unwrap_or_else(|| chunk.section_header.clone().unwrap_or_default());
    let mut header = Vec::new();
    for (name, value, limit) in [
        ("Document", title, 256),
        ("Section", path.as_str(), 512),
        (
            "Context",
            chunk.contextual_prefix.as_deref().unwrap_or(""),
            512,
        ),
        (
            "Caption",
            chunk.metadata["caption"].as_str().unwrap_or(""),
            256,
        ),
        (
            "Description",
            chunk.metadata["vlm_description"].as_str().unwrap_or(""),
            512,
        ),
    ] {
        let value = bounded(value, limit);
        if !value.is_empty() {
            header.push(format!("{name}: {value}"));
        }
    }
    if header.is_empty() {
        chunk.text.clone()
    } else {
        format!("{}\n\n{}", header.join("\n"), chunk.text)
    }
}
