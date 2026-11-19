//! Dormant legacy Text-First image-segment persistence helpers.
use super::{legacy_sync::LegacySyncService, *};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ImageSegment {
    pub segment_id: String,
    pub document_id: String,
    pub attachment_id: String,
    pub filename: String,
    pub media_type: String,
    pub file_size: i64,
    pub storage_url: String,
    pub vector_id: Option<String>,
    pub context_text: Option<String>,
    pub vlm_description: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Map<String, Value>,
}
impl ImageSegment {
    pub fn searchable_text(&self) -> String {
        match (
            self.vlm_description.as_deref().unwrap_or(""),
            self.context_text.as_deref().unwrap_or(""),
        ) {
            ("", context) => context.to_owned(),
            (description, "") => description.to_owned(),
            (description, context) => format!("{description}\n\n---\n上下文：{context}"),
        }
    }
    fn enriched_metadata(&self, position: i32, vector_stored: bool) -> Value {
        let mut metadata = self.metadata.clone();
        let description = self.vlm_description.as_deref().unwrap_or("");
        metadata.insert("has_vlm_description".into(), json!(!description.is_empty()));
        metadata.insert(
            "vlm_description_length".into(),
            json!(description.chars().count()),
        );
        metadata.insert("vector_stored".into(), json!(vector_stored));
        metadata.insert("source_position".into(), json!(position));
        if let Some(context) = self.context_text.as_ref().filter(|s| !s.is_empty()) {
            metadata
                .entry("context_text".to_owned())
                .or_insert(json!(context));
        }
        Value::Object(metadata)
    }
}
impl LegacySyncService {
    /// Historical explicit image rebuild; remains callable but is not scheduled at startup.
    pub async fn reprocess_document_images(
        &self,
        document_id: &str,
        connection_id: &str,
        page: &Value,
        dataset_id: &str,
        tenant_id: &str,
        binding_id: Option<&str>,
        image_max_size_bytes: Option<u64>,
    ) -> usize {
        let run = async {
            let connection = self.sources.connection(connection_id).await?
                .ok_or_else(||Error::missing("Connection not found"))?;
            let e = super::sync::Execution {
                binding:json!({"binding_id":binding_id,"connection_id":connection_id,"dataset_id":dataset_id,"tenant_id":tenant_id,"sync_images":true}),
                connection,
                actor:query::types::UserContext { tenant_id:tenant_id.into(), user_id:String::new(),user_tier:"normal".into(),user_type:"user".into(),roles:vec!["user".into()],is_authenticated:true },
                task_id:String::new(),
            };
            self.guard(&e).await?;
            let credentials = self.management.credentials(connection_id).await?;
            sqlx::query("DELETE FROM segments WHERE document_id=$1 AND content_type='image'")
                .bind(document_id).execute(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
            let attachments = self.attachments(&e, &credentials, &super::client::scalar(&page["id"])).await?;
            let mut connection = self.pool.acquire().await?;
            let images = self.image_sources_with_cleanup(&e, &credentials, page, document_id, "", &attachments,
                image_max_size_bytes.unwrap_or(3*1024*1024), &mut connection, true).await?;
            drop(connection);
            let receipts = images["extracted_images"].as_array().cloned().unwrap_or_default();
            for (position, receipt) in receipts.iter().enumerate() {
                let metadata = json!({"confluence_attachment_id":receipt["confluence_attachment_id"],
                    "page_id":page["id"],"attachment_updated_at":receipt["attachment_updated_at"],
                    "attachment_file_size":attachments.iter().find(|a|a["id"]==receipt["confluence_attachment_id"]).map(|a|a["fileSize"].clone()).unwrap_or(Value::Null)});
                let segment = ImageSegment {
                    segment_id:uuid::Uuid::new_v4().to_string(), document_id:document_id.into(),
                    attachment_id:text(receipt,"confluence_attachment_id").into(), filename:text(receipt,"filename").into(),
                    media_type:text(receipt,"mime_type").into(), file_size:receipt["size_bytes"].as_i64().unwrap_or(0),
                    storage_url:text(receipt,"storage_url").into(), vector_id:None,
                    context_text:receipt["context_text"].as_str().map(str::to_owned),
                    vlm_description:receipt["vlm_description"].as_str().map(str::to_owned),
                    metadata:metadata.as_object().cloned().unwrap_or_default(),
                };
                self.save_image_segment(&segment,dataset_id,position as i32).await;
            }
            sqlx::query("UPDATE documents SET segment_count=(SELECT COUNT(*) FROM segments WHERE document_id=$1),updated_at=NOW() WHERE document_id=$1")
                .bind(document_id).execute(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
            Ok::<_,Error>(receipts.len())
        }.await;
        run.unwrap_or(0)
    }
    /// The original helper deliberately isolates embedding/vector errors from SQL persistence.
    /// Like the source, a vector ID assigned before a failed upsert is retained.
    pub async fn save_image_segment(
        &self,
        segment: &ImageSegment,
        dataset_id: &str,
        position: i32,
    ) {
        let text = segment.searchable_text();
        let mut vector_id = None;
        if !text.is_empty() {
            let vector_result = async {
                let dataset = self.ingestion.indexer.store.dataset(dataset_id).await
                    .map_err(|_| Error::unavailable("Invalid image embedding dataset"))?;
                if dataset.collection_name.as_deref().unwrap_or("").is_empty() { return Ok::<_, Error>(()); }
                let vectors = self.ingestion.indexer.embeddings.embed_documents(&dataset, std::slice::from_ref(&text)).await
                    .map_err(|_| Error::unavailable("Image text embedding unavailable"))?;
                if let Some(vector) = vectors.first().filter(|v| !v.is_empty()) {
                    vector_id = Some(segment.segment_id.clone());
                    let point = json!({"id":segment.segment_id,"vector":vector,"payload":{
                        "dataset_id":dataset_id,"document_id":segment.document_id,"segment_id":segment.segment_id,
                        "text":text,"content_type":"image","image_filename":segment.filename,
                        "image_url":segment.storage_url,"vlm_description":segment.vlm_description}});
                    self.ingestion.indexer.vectors.upsert_points(&dataset, &[point]).await
                        .map_err(|_| Error::unavailable("Image vector persistence unavailable"))?;
                }
                Ok(())
            }.await;
            if vector_result.is_err() {
                tracing::warn!("legacy Confluence image vector persistence failed");
            }
        }
        let metadata = segment.enriched_metadata(position, vector_id.is_some());
        let outcome = sqlx::query("INSERT INTO segments(segment_id,dataset_id,document_id,position,text,token_count,vector_id,metadata,content_type,image_url,image_attachment_id,image_filename,image_media_type,image_file_size,enabled,status) VALUES($1,$2,$3,$4,$5,0,$6,$7,'image',$8,$9,$10,$11,$12,true,'completed') ON CONFLICT(segment_id) DO UPDATE SET dataset_id=EXCLUDED.dataset_id,document_id=EXCLUDED.document_id,vector_id=EXCLUDED.vector_id,content_type=EXCLUDED.content_type,image_url=EXCLUDED.image_url,image_attachment_id=EXCLUDED.image_attachment_id,image_filename=EXCLUDED.image_filename,image_media_type=EXCLUDED.image_media_type,image_file_size=EXCLUDED.image_file_size,updated_at=NOW()")
            .bind(&segment.segment_id).bind(dataset_id).bind(&segment.document_id)
            .bind(100_000_i32.saturating_add(position)).bind(text).bind(vector_id.or_else(|| segment.vector_id.clone()))
            .bind(metadata).bind(&segment.storage_url).bind(&segment.attachment_id).bind(&segment.filename)
            .bind(&segment.media_type).bind(segment.file_size).execute(ai_platform_knowledge_domain::database::executor(&self.pool)).await;
        if outcome.is_err() {
            tracing::warn!("legacy Confluence image segment persistence failed");
        }
    }

    /// Source receipts, rather than derived segment rows, own attachment freshness.
    pub async fn check_image_updates_needed(
        &self,
        document_id: &str,
        connection_id: &str,
        page_id: &str,
    ) -> bool {
        let check = async {
            let document = self.document(document_id).await?;
            let metadata = &document["metadata"];
            if !complete_receipt(metadata) {
                return Ok::<_, Error>(true);
            }
            let credentials = self.management.credentials(connection_id).await?;
            // This read-only legacy helper has no binding authority requirement.
            let mut attachments = Vec::new();
            let mut cursor = String::new();
            let mut seen = std::collections::BTreeSet::new();
            loop {
                let mut params = vec![("limit", "50")];
                if !cursor.is_empty() {
                    params.push(("cursor", cursor.as_str()));
                }
                let response = self
                    .sources
                    .provider(&credentials, &["pages", page_id, "attachments"], &params)
                    .await?;
                let rows = response["results"]
                    .as_array()
                    .ok_or_else(|| Error::unavailable("Invalid attachment response"))?;
                attachments.extend(
                    rows.iter()
                        .filter(|row| {
                            [
                                "image/png",
                                "image/jpeg",
                                "image/jpg",
                                "image/gif",
                                "image/bmp",
                                "image/webp",
                            ]
                            .contains(&text(row, "mediaType").to_lowercase().as_str())
                        })
                        .cloned(),
                );
                let next = text(&response["_links"], "next");
                if next.is_empty() || rows.len() < 50 {
                    break;
                }
                let url = reqwest::Url::parse("https://pagination.invalid/")
                    .unwrap()
                    .join(next)
                    .map_err(|_| Error::unavailable("Invalid attachment pagination"))?;
                cursor = url
                    .query_pairs()
                    .find(|(k, _)| k == "cursor")
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default();
                if cursor.is_empty() || !seen.insert(cursor.clone()) {
                    return Err(Error::unavailable("Incomplete attachment pagination"));
                }
            }
            Ok(json!(super::publication::attachment_manifest(&attachments))
                != metadata["_confluence_attachment_manifest"])
        }
        .await;
        check.unwrap_or(true)
    }
}
fn complete_receipt(metadata: &Value) -> bool {
    metadata["_confluence_image_source_generation"]["complete"] == true
        && metadata["extracted_images"].is_array()
        && metadata["_confluence_attachment_manifest"].is_array()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_first_context_and_unicode_description_metadata_match_source() {
        let mut segment = ImageSegment {
            vlm_description: Some("中文".into()),
            context_text: Some("context".into()),
            ..Default::default()
        };
        assert_eq!(segment.searchable_text(), "中文\n\n---\n上下文：context");
        segment
            .metadata
            .insert("context_text".into(), json!("retained"));
        let metadata = segment.enriched_metadata(3, true);
        assert_eq!(metadata["vlm_description_length"], 2);
        assert_eq!(metadata["context_text"], "retained");
        assert_eq!(metadata["source_position"], 3);
        assert_eq!(metadata["vector_stored"], true);
    }
    #[test]
    fn empty_and_policy_skipped_receipts_are_complete() {
        assert!(!complete_receipt(&json!({})));
        assert!(complete_receipt(
            &json!({"_confluence_image_source_generation":{"complete":true},"extracted_images":[],"_confluence_attachment_manifest":[]})
        ));
    }
}
