//! Vision PDF candidate business logic. The PyMuPDF-equivalent decoding and
//! rasterization boundary is explicit and unavailable by default; no synthetic
//! pages, OCR text, images, or successful embeddings are manufactured.
use super::{
    Chunk, Dataset, IngestError, Result,
    assets::AssetStorage,
    hierarchy::CandidatePlan,
    parser::{DocumentParser, MAX_PAGES, MAX_SOURCE_BYTES, require_text_budget},
    sha256,
};
use crate::migration_knowledge_query::embedding::EmbeddingManager;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Clone, Debug)]
pub struct RenderedPage {
    pub page_index: usize,
    pub png: Vec<u8>,
    pub width: usize,
    pub height: usize,
}
/// Each call must open and close an independent PDF decoder, equivalent to
/// VisionPDFProcessor._pdf_page_count/_render_page_batch. `dpi=None` means the
/// final fitz.Matrix(1,1) fallback; alpha is always disabled.
pub trait PdfRasterBackend: Send + Sync {
    fn page_count(&self, pdf: &[u8]) -> Result<usize>;
    fn render(&self, pdf: &[u8], page: usize, dpi: Option<u32>) -> Result<RenderedPage>;
}
#[derive(Clone)]
pub struct VisionProcessor {
    pub backend: Arc<dyn PdfRasterBackend>,
    pub embeddings: EmbeddingManager,
    pub parser: DocumentParser,
    pub extract_text: bool,
    pub vlm_ocr: Option<super::ocr::VlmOcrService>,
    pub ocr_strategy: String,
    pub render_dpi: u32,
    pub max_image_bytes: usize,
    pub batch_size: usize,
    pub position_offset: i32,
}
impl VisionProcessor {
    pub fn new(
        backend: Arc<dyn PdfRasterBackend>,
        embeddings: EmbeddingManager,
        parser: DocumentParser,
    ) -> Self {
        Self {
            backend,
            embeddings,
            parser,
            extract_text: false,
            vlm_ocr: None,
            ocr_strategy: "hybrid".into(),
            render_dpi: 200,
            max_image_bytes: 3 * 1024 * 1024,
            batch_size: 8,
            position_offset: 1_000_000,
        }
    }
    pub async fn planned_object_keys(
        &self,
        pdf: &[u8],
        tenant: &str,
        document: &str,
        generation: &str,
        page_offset: usize,
    ) -> Result<Vec<String>> {
        let generation = uuid::Uuid::parse_str(generation)
            .map_err(|_| IngestError::Validation("vision generation must be UUID".into()))?;
        if pdf.len() > MAX_SOURCE_BYTES {
            return Err(IngestError::Validation(
                "PDF source exceeds byte budget".into(),
            ));
        }
        let backend = self.backend.clone();
        let bytes = pdf.to_vec();
        let count = tokio::task::spawn_blocking(move || backend.page_count(&bytes))
            .await
            .map_err(|_| IngestError::Parser("PDF count task failed".into()))??;
        if count == 0 || count > MAX_PAGES {
            return Err(IngestError::Validation(
                "PDF page count outside bounded limit".into(),
            ));
        }
        (1..=count)
            .map(|p| {
                let n = p + page_offset;
                AssetStorage::image_key(
                    tenant,
                    document,
                    &format!("page_{n}_g{}", generation.simple()),
                    &format!("page_{n}.png"),
                )
            })
            .collect()
    }
    pub async fn prepare_document(
        &self,
        pdf: &[u8],
        dataset: &Dataset,
        document: &str,
        generation: &str,
        storage: &AssetStorage,
        pinned: Option<&BTreeMap<usize, String>>,
        page_offset: usize,
    ) -> Result<CandidatePlan> {
        let keys = self
            .planned_object_keys(pdf, &dataset.tenant_id, document, generation, page_offset)
            .await?;
        let total = keys.len();
        let generation = uuid::Uuid::parse_str(generation)
            .map_err(|_| IngestError::Validation("vision generation invalid".into()))?
            .to_string();
        let hex = uuid::Uuid::parse_str(&generation)
            .unwrap()
            .simple()
            .to_string();
        if let Some(pinned) = pinned {
            if pinned.keys().copied().collect::<Vec<_>>()
                != ((page_offset + 1)..=(page_offset + total)).collect::<Vec<_>>()
            {
                return Err(IngestError::Validation(
                    "pinned page texts must cover exactly rendered pages".into(),
                ));
            }
            require_text_budget(
                &pinned
                    .iter()
                    .filter(|(_, t)| !t.is_empty())
                    .map(|(n, t)| format!("[Page {n}]\n{t}"))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            )?;
        }
        let mut points = vec![];
        let mut rows = vec![];
        let mut receipts = vec![];
        let mut extracted = BTreeMap::new();
        for start in (0..total).step_by(self.batch_size.max(1)) {
            let stop = (start + self.batch_size.max(1)).min(total);
            let backend = self.backend.clone();
            let bytes = pdf.to_vec();
            let max = self.max_image_bytes;
            let dpi = self.render_dpi;
            let rendered = tokio::task::spawn_blocking(move || {
                let mut rendered = vec![];
                for page in start..stop {
                    let mut found = None;
                    for dpi in [Some(dpi), Some(150), Some(120), None] {
                        if let Ok(image) = backend.render(&bytes, page, dpi) {
                            if image.page_index == page
                                && !image.png.is_empty()
                                && image.png.len() <= max
                                && image.width > 0
                                && image.height > 0
                            {
                                found = Some(image);
                                break;
                            }
                        }
                    }
                    rendered.push(found.ok_or_else(|| {
                        IngestError::Parser(format!(
                            "scanned candidate page {} could not render",
                            page + 1
                        ))
                    })?);
                }
                Ok::<_, IngestError>(rendered)
            })
            .await
            .map_err(|_| IngestError::Parser("PDF render task failed".into()))??;
            let mut images = vec![];
            let mut batch = vec![];
            for image in rendered {
                let number = page_offset + image.page_index + 1;
                let text = if let Some(pinned) = pinned {
                    pinned[&number].clone()
                } else if self.extract_text {
                    super::ocr::ocr_image_auto(
                        &image.png,
                        self.vlm_ocr.as_ref(),
                        &self.parser,
                        &self.ocr_strategy,
                    )
                    .await?
                } else {
                    String::new()
                };
                require_text_budget(&text)?;
                if !text.is_empty() {
                    extracted.insert(number, text.clone());
                }
                let attachment = format!("page_{number}_g{hex}");
                let filename = format!("page_{number}.png");
                let key =
                    AssetStorage::image_key(&dataset.tenant_id, document, &attachment, &filename)?;
                let digest = sha256(&image.png);
                if storage.exists(&key).await? {
                    if sha256(&storage.download_original(&key).await?) != digest {
                        return Err(IngestError::GenerationChanged);
                    }
                } else {
                    storage.put(&key, &image.png, "image/png").await?;
                }
                // Use the same scoped storage authority that wrote the verified
                // object. Cloud signing and local HMAC/expiry stay with storage.
                let url = storage.image_url(&key).await?;
                if url.trim().is_empty()
                    || sha256(&storage.download_original(&key).await?) != digest
                {
                    return Err(IngestError::Storage);
                }
                receipts.push(json!({"tenant_id":dataset.tenant_id,"document_id":document,"attachment_id":attachment,"filename":filename,"storage_url":url,"generation_id":generation,"sha256":digest,"storage_key":key}));
                let position = self.position_offset + page_offset as i32 + image.page_index as i32;
                let id = uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_URL,
                    format!(
                        "ai-platform:kb-image-candidate:{}:{document}:{generation}:{position}",
                        dataset.dataset_id
                    )
                    .as_bytes(),
                )
                .to_string();
                let display = if text.is_empty() {
                    format!("[Page {number}]")
                } else {
                    format!("[Page {number}]\n{text}")
                };
                batch.push((
                    id,
                    position,
                    number,
                    image.width,
                    image.height,
                    url,
                    attachment,
                    filename,
                    image.png.len(),
                    display,
                ));
                images.push(image.png);
            }
            let vectors = self.embeddings.embed_images(dataset, &images).await?;
            if vectors.len() != batch.len() {
                return Err(IngestError::Index(
                    "scanned candidate image embedding incomplete".into(),
                ));
            }
            for (
                (id, position, number, width, height, url, attachment, filename, size, text),
                vector,
            ) in batch.into_iter().zip(vectors)
            {
                if vector.is_empty() || vector.iter().any(|v| !v.is_finite()) {
                    return Err(IngestError::Index(
                        "scanned candidate image embedding invalid".into(),
                    ));
                }
                points.push(json!({"id":id,"vector":{"":vector},"payload":{"tenant_id":dataset.tenant_id,"dataset_id":dataset.dataset_id,"document_id":document,"segment_id":id,"position":position,"text":text,"content_type":"image","image_id":format!("{document}_page_{number}"),"image_mime_type":"image/png","image_width":width,"image_height":height,"image_page":number,"generation_id":generation}}));
                rows.push(Chunk{segment_id:id.clone(),document_id:document.into(),dataset_id:dataset.dataset_id.clone(),position,text,content_type:"image".into(),image_url:Some(url),image_attachment_id:Some(attachment),image_filename:Some(filename),image_media_type:Some("image/png".into()),image_file_size:Some(size as i64),vector_id:Some(id),enabled:false,status:"indexing".into(),metadata:json!({"page_number":number,"width":width,"height":height,"source_position":position-self.position_offset,"generation_id":generation}),..Chunk::default()});
            }
        }
        let content = extracted
            .iter()
            .map(|(n, t)| format!("[Page {n}]\n{t}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        require_text_budget(&content)?;
        let ids = rows.iter().map(|r| r.segment_id.clone()).collect();
        Ok(CandidatePlan {
            generation_id: generation,
            document_id: document.into(),
            dataset_id: dataset.dataset_id.clone(),
            points_by_collection: BTreeMap::from([(
                dataset.collection_name.clone().ok_or_else(|| {
                    IngestError::Validation("scanned dataset collection missing".into())
                })?,
                points,
            )]),
            segment_rows: rows,
            object_manifest: receipts,
            content,
            source_hash: sha256(pdf),
            segment_ids: ids,
            summary_row: None,
            l1_count: 0,
            l2_count: 0,
            l3_count: 0,
            source_kind: "vision".into(),
            total_pages: Some(total),
            extracted_texts: extracted,
        })
    }
}
