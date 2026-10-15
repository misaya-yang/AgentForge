//! Immutable three-level candidate preparation, including real embeddings.
use super::{Chunk, Dataset, IngestError, Result, object, sha256};
use super::{
    chunking::{Chunker, ChunkingConfig, MAX_CHUNK_OUTPUTS},
    summary::{SummaryGenerator, prefix},
};
use crate::migration_knowledge_query::embedding::EmbeddingManager;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug)]
pub struct CandidatePlan {
    pub generation_id: String,
    pub document_id: String,
    pub dataset_id: String,
    pub points_by_collection: BTreeMap<String, Vec<Value>>,
    pub segment_rows: Vec<Chunk>,
    pub object_manifest: Vec<Value>,
    pub content: String,
    pub source_hash: String,
    pub segment_ids: Vec<String>,
    pub summary_row: Option<Value>,
    pub l1_count: usize,
    pub l2_count: usize,
    pub l3_count: usize,
    pub source_kind: String,
    pub total_pages: Option<usize>,
    pub extracted_texts: BTreeMap<usize, String>,
}
pub fn candidate_id(
    dataset: &str,
    document: &str,
    generation: &str,
    level: i32,
    position: i32,
) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("ai-platform:kb-hierarchy:{dataset}:{document}:{generation}:{level}:{position}")
            .as_bytes(),
    )
    .to_string()
}
pub fn public_metadata(value: &Value) -> Value {
    let mut result = object(value);
    if let Some(m) = result.as_object_mut() {
        m.retain(|k, _| {
            !k.starts_with('_')
                && !super::source_metadata_key(k)
                && ![
                    "tenant_id",
                    "dataset_id",
                    "document_id",
                    "segment_id",
                    "position",
                    "level",
                    "text",
                    "content_type",
                    "parent_segment_id",
                    "generation_id",
                    "source_version",
                    "source_hash",
                    "source_type",
                    "source_reference",
                    "source_url",
                    "image_url",
                    "content_hash",
                    "current_version",
                    "version_count",
                    "status",
                    "enabled",
                    "archived",
                    "progress",
                ]
                .contains(&k.as_str())
        })
    }
    result
}
#[derive(Clone)]
pub struct HierarchicalIndexer {
    pub chunker: Chunker,
    pub embeddings: EmbeddingManager,
    pub summary: Option<SummaryGenerator>,
    pub levels: Vec<i32>,
}
impl HierarchicalIndexer {
    pub async fn prepare_document(
        &self,
        dataset: &Dataset,
        document: &str,
        text: &str,
        generation: &str,
        metadata: &Value,
        user_config: Option<&ChunkingConfig>,
        levels: Option<&[i32]>,
    ) -> Result<CandidatePlan> {
        let generation = uuid::Uuid::parse_str(generation)
            .map_err(|_| IngestError::Validation("generation_id must be a durable UUID".into()))?
            .to_string();
        let levels = levels.unwrap_or(&self.levels);
        if document.is_empty()
            || dataset.dataset_id.is_empty()
            || dataset.tenant_id.trim().is_empty()
            || text.trim().is_empty()
        {
            return Err(IngestError::Validation(
                "hierarchical candidate owner and text are required".into(),
            ));
        }
        if levels.is_empty()
            || levels.iter().collect::<BTreeSet<_>>().len() != levels.len()
            || levels.iter().any(|l| !matches!(l, 1 | 2 | 3))
        {
            return Err(IngestError::Validation(
                "hierarchical candidate levels are invalid".into(),
            ));
        }
        let metadata = public_metadata(metadata);
        let resolved = self.embeddings.resolve(dataset).await?;
        let base = dataset
            .collection_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("kb_{}_{}", dataset.dataset_id, resolved.dimension));
        let mut points = BTreeMap::from([
            (base.clone(), vec![]),
            (format!("{base}_sections"), vec![]),
            (format!("{base}_summary"), vec![]),
        ]);
        let mut rows = vec![];
        if levels.contains(&2) || levels.contains(&3) {
            let mut config = ChunkingConfig::from_value(
                &json!({"mode":"hierarchical","parent_chunk_size":6000,"parent_overlap":50,"child_chunk_size":1600,"child_overlap":50,"parent_mode":"fixed","use_token_count":true,"token_limit":400,"parent_token_limit":1500,"child_token_limit":400}),
            )?;
            if let Some(c) = user_config {
                config = c.clone();
                config.mode = "hierarchical".into();
            }
            let chunks = self.chunker.process_document(
                text,
                &config,
                &dataset.dataset_id,
                document,
                "text",
            )?;
            let mut parents = BTreeMap::new();
            let mut l2 = vec![];
            let mut l3 = vec![];
            for mut row in chunks.iter().filter(|c| c.level == 2).cloned() {
                let position = 1_000_000 + l2.len() as i32;
                let id = candidate_id(&dataset.dataset_id, document, &generation, 2, position);
                parents.insert(row.segment_id.clone(), id.clone());
                row.segment_id = id.clone();
                row.vector_id = Some(id);
                row.position = position;
                row.content_type = "section".into();
                let heading = row
                    .metadata
                    .get("heading")
                    .cloned()
                    .or_else(|| row.section_header.clone().map(Value::String))
                    .unwrap_or(Value::Null);
                row.metadata = metadata.clone();
                row.metadata["section_index"] = json!(l2.len());
                row.metadata["heading"] = heading;
                row.metadata["content_type"] = json!("section");
                if levels.contains(&2) && row.text.chars().count() > 500 {
                    if let Some(summary) = &self.summary {
                        row.summary = Some(summary.summarize_section(&row.text).await)
                    }
                }
                l2.push(row)
            }
            for mut row in chunks.into_iter().filter(|c| c.level == 3) {
                row.position = l3.len() as i32;
                row.segment_id =
                    candidate_id(&dataset.dataset_id, document, &generation, 3, row.position);
                row.vector_id = Some(row.segment_id.clone());
                row.parent_segment_id = row
                    .parent_segment_id
                    .as_ref()
                    .and_then(|p| parents.get(p).cloned());
                row.metadata = metadata.clone();
                row.metadata["chunk_index"] = json!(row.position);
                row.metadata["token_count"] = json!(row.token_count);
                row.metadata["content_type"] = json!("text");
                row.metadata["parent_segment_id"] = json!(row.parent_segment_id);
                l3.push(row)
            }
            if levels.contains(&3) {
                rows.extend(l3)
            }
            if levels.contains(&2) {
                rows.extend(l2)
            }
        }
        let mut summary_row = None;
        if levels.contains(&1) {
            if let Some(generator) = &self.summary {
                let summary = generator.summarize_document(text).await;
                let value = summary["summary"]
                    .as_str()
                    .ok_or_else(|| IngestError::Validation("document summary must be text".into()))?
                    .to_owned();
                let id = candidate_id(&dataset.dataset_id, document, &generation, 1, 0);
                let mut meta = metadata.clone();
                meta["content_type"] = json!("document_summary");
                meta["topics"] = summary["topics"].clone();
                summary_row = Some(
                    json!({"document_id":document,"summary":value,"keywords":summary["keywords"],"topics":summary["topics"],"vector_id":id}),
                );
                rows.push(Chunk {
                    segment_id: id.clone(),
                    dataset_id: dataset.dataset_id.clone(),
                    document_id: document.into(),
                    position: 0,
                    level: 1,
                    text: value.clone(),
                    summary: Some(value),
                    keywords: summary["keywords"].clone(),
                    vector_id: Some(id),
                    metadata: meta,
                    content_type: "document_summary".into(),
                    ..Chunk::default()
                });
            }
        }
        if rows.is_empty() || rows.len() > MAX_CHUNK_OUTPUTS {
            return Err(IngestError::Validation(
                "hierarchical candidate output count is invalid".into(),
            ));
        }
        for level in [3, 2, 1] {
            let selected: Vec<_> = rows.iter().filter(|s| s.level == level).collect();
            let texts: Vec<_> = selected
                .iter()
                .map(|s| {
                    if level == 2 {
                        s.summary
                            .clone()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| prefix(&s.text, 2000))
                    } else {
                        s.summary
                            .clone()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| s.text.clone())
                    }
                })
                .collect();
            if texts.is_empty() {
                continue;
            }
            let vectors = self
                .embeddings
                .embed_resolved(&resolved, &texts, "document")
                .await?;
            if vectors.len() != texts.len()
                || vectors
                    .iter()
                    .any(|v| v.len() != resolved.dimension || v.iter().any(|v| !v.is_finite()))
            {
                return Err(IngestError::Index(
                    "hierarchical candidate embedding is incomplete or invalid".into(),
                ));
            }
            for (row, vector) in selected.iter().zip(vectors) {
                let mut payload = if level == 3 {
                    row.metadata.clone()
                } else {
                    json!({})
                };
                for (k, v) in [
                    ("tenant_id", json!(dataset.tenant_id)),
                    ("dataset_id", json!(dataset.dataset_id)),
                    ("document_id", json!(document)),
                    ("segment_id", json!(row.segment_id)),
                    ("level", json!(level)),
                    (
                        "text",
                        json!(if level == 2 {
                            prefix(&row.text, 500)
                        } else {
                            row.text.clone()
                        }),
                    ),
                    ("content_type", json!(row.content_type)),
                ] {
                    payload[k] = v
                }
                if level != 1 {
                    payload["position"] = json!(row.position)
                }
                if level == 3 {
                    payload["parent_segment_id"] = json!(row.parent_segment_id)
                } else {
                    payload["summary"] = json!(row.summary)
                }
                if level == 1 {
                    payload["keywords"] = row.keywords.clone()
                }
                let collection = match level {
                    1 => format!("{base}_summary"),
                    2 => format!("{base}_sections"),
                    _ => base.clone(),
                };
                points
                    .get_mut(&collection)
                    .unwrap()
                    .push(json!({"id":row.segment_id,"vector":{"":vector},"payload":payload}));
            }
        }
        for row in &mut rows {
            row.enabled = false;
            row.status = "indexing".into();
            row.token_count = (row.text.chars().count() / 4) as i32;
        }
        let ids = rows.iter().map(|s| s.segment_id.clone()).collect();
        let l1 = rows.iter().filter(|s| s.level == 1).count();
        let l2 = rows.iter().filter(|s| s.level == 2).count();
        let l3 = rows.iter().filter(|s| s.level == 3).count();
        Ok(CandidatePlan {
            generation_id: generation,
            document_id: document.into(),
            dataset_id: dataset.dataset_id.clone(),
            points_by_collection: points,
            segment_rows: rows,
            object_manifest: vec![],
            content: text.into(),
            source_hash: sha256(text.as_bytes()),
            segment_ids: ids,
            summary_row,
            l1_count: l1,
            l2_count: l2,
            l3_count: l3,
            source_kind: "hierarchy".into(),
            total_pages: None,
            extracted_texts: BTreeMap::new(),
        })
    }
}
