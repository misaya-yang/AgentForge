//! Legacy image APIs retain their distinct all-or-nothing and best-effort contracts.
use super::{
    Chunk, Dataset, IngestError, IngestionService, KnowledgeWriteStore, Result,
    dataset_ingestion_identity, images::ExtractedImage, string,
};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};
use std::{collections::BTreeSet, time::Duration};
async fn store_image_batch(conn: &mut PgConnection, rows: &[Chunk]) -> Result<()> {
    for row in rows {
        let id = sqlx::query(super::artifact_sql::STORE_IMAGE_SEGMENTS)
            .bind(&row.segment_id)
            .bind(&row.dataset_id)
            .bind(&row.document_id)
            .bind(row.position)
            .bind(&row.text)
            .bind(row.token_count)
            .bind(&row.vector_id)
            .bind(&row.metadata)
            .bind(&row.image_url)
            .bind(&row.image_attachment_id)
            .bind(&row.image_filename)
            .bind(&row.image_media_type)
            .bind(row.image_file_size)
            .fetch_optional(crate::database::connection(&mut *conn))
            .await?;
        if id.is_none() {
            return Err(IngestError::GenerationChanged);
        }
    }
    Ok(())
}
impl IngestionService {
    async fn persist_image_segment_batch(
        &self,
        lease: &mut super::store::DocumentLease,
        dataset: &Dataset,
        points: &[Value],
        rows: &[Chunk],
        identity: &str,
    ) -> Result<()> {
        let ids: Vec<String> = points.iter().map(|p| string(p, "id").to_owned()).collect();
        let snapshots = self.indexer.snapshot_points(dataset, &ids).await?;
        KnowledgeWriteStore::require_identity(
            lease.connection(),
            &dataset.dataset_id,
            Some(identity),
        )
        .await?;
        self.indexer
            .vectors
            .upsert_index_points(dataset, points)
            .await?;
        let result: Result<()> = async {
            let mut tx = lease.connection().begin().await?;
            crate::database::bind_current(&mut tx).await?;
            store_image_batch(&mut tx, rows).await?;
            tx.commit().await?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            let existing: BTreeSet<_> = snapshots.iter().map(|p| string(p, "id")).collect();
            let new: Vec<_> = ids
                .into_iter()
                .filter(|id| !existing.contains(id.as_str()))
                .collect();
            let restore = self.indexer.restore_points(dataset, &snapshots).await;
            let delete = self.indexer.delete_points(dataset, &new).await;
            if restore.is_err() || delete.is_err() {
                return Err(IngestError::Index(
                    "image receipt failed and vector compensation was incomplete".into(),
                ));
            }
            return Err(error);
        }
        Ok(())
    }
    pub async fn embed_images_in_memory(
        &self,
        dataset: &Dataset,
        document: &str,
        images: &[ExtractedImage],
        collection: &str,
        base_position: i32,
        expected_identity: Option<&str>,
    ) -> Result<(usize, Vec<Value>)> {
        if images.is_empty() {
            return Ok((0, vec![]));
        }
        let nonempty = images.iter().filter(|i| !i.content.is_empty()).count();
        if nonempty == 0 {
            return Ok((0, vec![]));
        }
        if nonempty != images.len() {
            return Err(IngestError::Validation(
                "in-memory image receipt is incomplete; refusing a partial image generation".into(),
            ));
        }
        let identity = expected_identity
            .map(str::to_owned)
            .unwrap_or_else(|| dataset_ingestion_identity(&dataset.to_value()));
        let mut scoped = dataset.clone();
        scoped.collection_name = Some(collection.into());
        let mut lease = self
            .store
            .document_lease(&dataset.dataset_id, document)
            .await?;
        KnowledgeWriteStore::require_identity(
            lease.connection(),
            &dataset.dataset_id,
            Some(&identity),
        )
        .await?;
        let mut points = vec![];
        let mut rows = vec![];
        let mut metadata = vec![];
        for (start, batch) in images.chunks(8).enumerate() {
            let start = start * 8;
            let bytes: Vec<_> = batch.iter().map(|i| i.content.clone()).collect();
            let mut vectors = None;
            for retry in 0..3 {
                match self.indexer.embeddings.embed_images(dataset, &bytes).await {
                    Ok(v) if v.len() == batch.len() => {
                        vectors = Some(v);
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        if retry < 2 {
                            tokio::time::sleep(Duration::from_secs(1 << retry)).await
                        }
                    }
                }
            }
            if let Some(vectors) = vectors {
                for (index, (image, vector)) in batch.iter().zip(vectors).enumerate() {
                    if vector.is_empty() {
                        continue;
                    }
                    if vector.iter().any(|v| !v.is_finite()) {
                        return Err(IngestError::Index(
                            "Vectors contain NaN or Infinity values".into(),
                        ));
                    }
                    let index = start + index;
                    let position = base_position + index as i32;
                    let id = super::stable_segment_id(document, "image", position);
                    let page = image
                        .metadata
                        .get("page_number")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let text = if image.context_text.is_empty() {
                        format!("[Image: page {index}]")
                    } else {
                        image.context_text.chars().take(200).collect()
                    };
                    let filename = if image.filename.is_empty() {
                        format!(
                            "image_{index}.{}",
                            image.mime_type.split('/').next_back().unwrap_or("png")
                        )
                    } else {
                        image.filename.clone()
                    };
                    let payload = json!({"tenant_id":dataset.tenant_id,"dataset_id":dataset.dataset_id,"document_id":document,"segment_id":id,"position":position,"text":text,"content_type":"image","image_id":image.image_id,"image_mime_type":image.mime_type,"image_width":image.width,"image_height":image.height,"image_page":page});
                    points.push(json!({"id":id,"vector":{"":vector},"payload":payload}));
                    rows.push(serde_json::from_value(json!({"segment_id":id,"dataset_id":dataset.dataset_id,"document_id":document,"position":position,"text":text,"token_count":0,"vector_id":id,"content_type":"image","image_attachment_id":image.image_id,"image_filename":filename,"image_media_type":image.mime_type,"image_file_size":image.content.len(),"metadata":{"width":image.width,"height":image.height,"page_number":page,"source_location":image.source_location,"source_position":index},"enabled":true,"status":"completed"}))?);
                    metadata.push(json!({"idx":index,"image_id":image.image_id,"segment_id":id,"vector_id":id}));
                }
            }
            KnowledgeWriteStore::update_status(
                lease.connection(),
                document,
                "uploading_images",
                Some(10.0 + (start + batch.len()) as f64 / images.len() as f64 * 50.0),
                None,
            )
            .await?;
        }
        if rows.len() != images.len() {
            return Err(IngestError::Index("in-memory embedding did not produce every required vector; refusing a partial image generation".into()));
        }
        self.persist_image_segment_batch(&mut lease, &scoped, &points, &rows, &identity)
            .await?;
        lease.release().await?;
        Ok((rows.len(), metadata))
    }
    /// Source pdf_image_processor.ExtractedImage shape differs from unified
    /// ExtractedImage by an explicit page_number and 3MiB embeddable cap.
    pub async fn process_document_images(
        &self,
        dataset: &Dataset,
        document: &str,
        images: &[LegacyPdfImage],
        collection: &str,
        base_position: i32,
        expected_identity: Option<&str>,
    ) -> Result<usize> {
        if images.is_empty() {
            return Ok(0);
        }
        let identity = expected_identity
            .map(str::to_owned)
            .unwrap_or_else(|| dataset_ingestion_identity(&dataset.to_value()));
        let mut scoped = dataset.clone();
        scoped.collection_name = Some(collection.into());
        let mut lease = self
            .store
            .document_lease(&dataset.dataset_id, document)
            .await?;
        KnowledgeWriteStore::require_identity(
            lease.connection(),
            &dataset.dataset_id,
            Some(&identity),
        )
        .await?;
        let mut points = vec![];
        let mut rows = vec![];
        for (index, image) in images.iter().enumerate() {
            if !image.embeddable() {
                continue;
            }
            let result:Result<(Value,Chunk)>=async{let vectors=self.indexer.embeddings.embed_images(dataset,&[image.content.clone()]).await?;let vector=vectors.first().filter(|v|!v.is_empty()).ok_or_else(||IngestError::Index("image embedding returned no vector".into()))?;let position=base_position+index as i32;let id=super::stable_segment_id(document,"image",position);let text=if image.context_text.is_empty(){format!("[Image: Page {}]",image.page_number)}else{image.context_text.clone()};let filename=format!("page{}_{}.{}",image.page_number,image.image_id,image.mime_type.split('/').next_back().unwrap_or("png"));let payload=json!({"tenant_id":dataset.tenant_id,"dataset_id":dataset.dataset_id,"document_id":document,"segment_id":id,"position":position,"text":text,"content_type":"image","image_id":image.image_id,"image_mime_type":image.mime_type,"image_width":image.width,"image_height":image.height,"image_page":image.page_number});let key=super::assets::AssetStorage::image_key(&dataset.tenant_id,document,&format!("pdf_{index}"),&filename)?;let url=match self.assets.backend.upload(&key,image.content.clone(),&image.mime_type,Default::default()).await{Ok(url)=>url,Err(_)=>String::new()};let row=serde_json::from_value(json!({"segment_id":id,"dataset_id":dataset.dataset_id,"document_id":document,"position":position,"text":image.context_text,"token_count":0,"vector_id":id,"content_type":"image","image_url":url,"image_attachment_id":image.image_id,"image_filename":filename,"image_media_type":image.mime_type,"image_file_size":image.content.len(),"metadata":{"width":image.width,"height":image.height,"page_number":image.page_number,"source_position":index},"enabled":true,"status":"completed"}))?;Ok((json!({"id":id,"vector":{"":vector},"payload":payload}),row))}.await;
            if let Ok((point, row)) = result {
                points.push(point);
                rows.push(row);
            }
        }
        if !points.is_empty() {
            KnowledgeWriteStore::require_identity(
                lease.connection(),
                &dataset.dataset_id,
                Some(&identity),
            )
            .await?;
            self.indexer
                .vectors
                .upsert_index_points(&scoped, &points)
                .await?;
        }
        // This explicitly legacy API preserves its per-row best-effort source behavior.
        for row in &rows {
            let _ = save_legacy_image(lease.connection(), row).await;
        }
        lease.release().await?;
        Ok(rows.len())
    }
}
pub struct LegacyPdfImage {
    pub image_id: String,
    pub content: Vec<u8>,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub page_number: usize,
    pub context_text: String,
    pub alt_text: String,
    pub metadata: Value,
}
impl LegacyPdfImage {
    pub fn embeddable(&self) -> bool {
        [
            "image/png",
            "image/jpeg",
            "image/jpg",
            "image/bmp",
            "image/webp",
        ]
        .contains(&self.mime_type.as_str())
            && self.content.len() <= 3 * 1024 * 1024
            && self.width >= 50
            && self.height >= 50
    }
}
async fn save_legacy_image(conn: &mut PgConnection, row: &Chunk) -> Result<()> {
    sqlx::query("INSERT INTO knowledge.segments(segment_id,dataset_id,document_id,position,text,token_count,vector_id,metadata,content_type,image_url,image_attachment_id,image_filename,image_media_type,image_file_size,enabled,status) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'image',$9,$10,$11,$12,$13,TRUE,'completed') ON CONFLICT(segment_id) DO UPDATE SET dataset_id=EXCLUDED.dataset_id,document_id=EXCLUDED.document_id,vector_id=EXCLUDED.vector_id,content_type=EXCLUDED.content_type,image_url=EXCLUDED.image_url,image_attachment_id=EXCLUDED.image_attachment_id,image_filename=EXCLUDED.image_filename,image_media_type=EXCLUDED.image_media_type,image_file_size=EXCLUDED.image_file_size,updated_at=NOW()").bind(&row.segment_id).bind(&row.dataset_id).bind(&row.document_id).bind(row.position).bind(&row.text).bind(row.token_count).bind(&row.vector_id).bind(&row.metadata).bind(&row.image_url).bind(&row.image_attachment_id).bind(&row.image_filename).bind(&row.image_media_type).bind(row.image_file_size).execute(crate::database::connection(&mut *conn)).await?;
    Ok(())
}
