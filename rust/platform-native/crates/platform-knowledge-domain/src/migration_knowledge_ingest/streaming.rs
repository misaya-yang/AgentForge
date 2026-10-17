//! Batch loader state and bounded scheduling for the canonical streaming PDF
//! path. Real decoder/image bytes come from the explicit native backend.
use super::{IngestError, Result, assets::AssetStorage};
use serde::{Deserialize, Serialize};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};
pub use ai_platform_service_primitives::pdf_ports::streaming::{PdfSource,PageContent,BatchResult,StreamingPdfBackend};
pub type BatchConsumer =
    Arc<dyn Fn(BatchResult) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync>;
#[derive(Clone)]
pub struct StreamingDocumentLoader {
    pub backend: Arc<dyn StreamingPdfBackend>,
    pub batch_size: usize,
    pub extract_images: bool,
    pub extract_images_if_no_text: bool,
    pub max_image_size: usize,
    pub storage: Option<AssetStorage>,
}
impl StreamingDocumentLoader {
    pub fn new(backend: Arc<dyn StreamingPdfBackend>) -> Self {
        Self {
            backend,
            batch_size: 20,
            extract_images: true,
            extract_images_if_no_text: false,
            max_image_size: 1024,
            storage: None,
        }
    }
    pub fn is_large_file(size: u64) -> bool {
        size > 50 * 1024 * 1024
    }
    pub async fn resolve_source(&self, source: &str) -> Result<PdfSource> {
        let path = PathBuf::from(source);
        if path.exists() {
            return Ok(PdfSource::Path(path));
        }
        let bytes = self
            .storage
            .as_ref()
            .ok_or(IngestError::NotFound)?
            .download_bounded(source, 100 * 1024 * 1024)
            .await?;
        Ok(PdfSource::Bytes(Arc::new(bytes)))
    }
    pub async fn get_page_count(&self, source: &PdfSource) -> Result<usize> {
        let backend = self.backend.clone();
        let source = source.clone();
        tokio::task::spawn_blocking(move || backend.page_count(&source))
            .await
            .map_err(|_| IngestError::Parser("streaming PDF count task failed".into()))?
    }
    pub async fn load_page_range(
        &self,
        source: &PdfSource,
        start: usize,
        end: usize,
    ) -> Result<Vec<PageContent>> {
        let backend = self.backend.clone();
        let source = source.clone();
        let extract = self.extract_images;
        let empty = self.extract_images_if_no_text;
        let size = self.max_image_size;
        tokio::task::spawn_blocking(move || {
            let total = backend.page_count(&source)?;
            let mut pages = vec![];
            for page in start.saturating_sub(1)..end.min(total) {
                pages.push(backend.extract_page(&source, page, extract, empty, size)?)
            }
            backend.release_memory();
            Ok(pages)
        })
        .await
        .map_err(|_| IngestError::Parser("streaming PDF batch task failed".into()))?
    }
    pub async fn iter_batches(&self, source: &PdfSource, consume: BatchConsumer) -> Result<()> {
        let total = self.get_page_count(source).await?;
        let size = self.batch_size.clamp(5, 50);
        for (index, start) in (0..total).step_by(size).enumerate() {
            let end = (start + size).min(total);
            let pages = self.load_page_range(source, start + 1, end).await?;
            consume(BatchResult {
                pages,
                batch_index: index,
                start_page: start + 1,
                end_page: end,
                total_pages: total,
                progress: end as f64 / total as f64,
            })
            .await?;
        }
        Ok(())
    }
}

/// Pillow-compatible decision: malformed/unsupported images are left unchanged.
pub fn should_resize_image(bytes: &[u8], max_size: usize) -> bool {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .is_some_and(|(w, h)| w as usize > max_size || h as usize > max_size)
}
pub fn resize_image(bytes: &[u8], max_size: usize) -> Vec<u8> {
    let result = (|| -> std::result::Result<Vec<u8>, image::ImageError> {
        let mut image = image::load_from_memory(bytes)?;
        let (width, height) = (image.width(), image.height());
        if width as usize > max_size || height as usize > max_size {
            let ratio = (max_size as f64 / width as f64).min(max_size as f64 / height as f64);
            let new_width = (width as f64 * ratio) as u32;
            let new_height = (height as f64 * ratio) as u32;
            if new_width == 0 || new_height == 0 {
                return Ok(bytes.to_vec());
            }
            image =
                image.resize_exact(new_width, new_height, image::imageops::FilterType::Lanczos3);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png)?;
        Ok(out.into_inner())
    })();
    result.unwrap_or_else(|_| bytes.to_vec())
}
struct Spool {
    root: PathBuf,
}
impl Spool {
    fn new(root: &std::path::Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let root = root.join(format!("knowledge-stream-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root)?;
        Ok(Self { root })
    }
}
impl Drop for Spool {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
pub struct StreamedText {
    pub text: String,
    pub total_pages: usize,
}
impl super::IngestionService {
    /// Worker._process_large_file: source -> private disk spool -> bounded text
    /// spool, preserving page markers and largest-image OCR fallback.
    pub(crate) async fn extract_large_file(
        &self,
        lease: &mut super::store::DocumentLease,
        dataset: &super::Dataset,
        document: &serde_json::Value,
    ) -> Result<StreamedText> {
        use std::io::{Read, Write};
        let id = super::string(document, "document_id");
        let metadata = super::object(&document["metadata"]);
        let key = super::string(&metadata, "original_file_key");
        if key.is_empty() {
            return Err(IngestError::Validation("No original file key found".into()));
        }
        let mut loader = self.streaming.clone().ok_or_else(|| {
            IngestError::Parser("large-file streaming PDF backend is not configured".into())
        })?;
        loader.extract_images = false;
        loader.extract_images_if_no_text = self.streaming_ocr_enabled;
        let spool = Spool::new(&self.parser.config.scratch_root)?;
        let source_path = spool.root.join("source.bin");
        let text_path = spool.root.join("content.txt");
        let info = self
            .assets
            .backend
            .head(key)
            .await
            .map_err(|_| IngestError::Storage)?
            .ok_or(IngestError::NotFound)?;
        let max = usize::try_from(info.size_bytes).map_err(|_| {
            IngestError::Validation("source size exceeds platform address space".into())
        })?;
        self.assets
            .backend
            .download_to_path(key, &source_path, max)
            .await
            .map_err(|_| IngestError::Storage)?;
        let source = PdfSource::Path(source_path);
        let total = loader.get_page_count(&source).await?;
        let size = loader.batch_size.clamp(5, 50);
        let mut chars = 0usize;
        let mut bytes = 0usize;
        std::fs::File::create(&text_path)?;
        for start in (0..total).step_by(size) {
            let end = (start + size).min(total);
            let pages = loader.load_page_range(&source, start + 1, end).await?;
            let mut parts = Vec::new();
            for page in pages {
                if !page.text.trim().is_empty() {
                    parts.push(format!("[Page {}]\n{}", page.page_number, page.text));
                } else if self.streaming_ocr_enabled {
                    if let Some(image) = page.images.iter().max_by_key(|b| b.len()) {
                        let text = super::ocr::ocr_image_auto(
                            image,
                            self.streaming_ocr.as_ref(),
                            &self.parser,
                            &self.streaming_ocr_strategy,
                        )
                        .await
                        .unwrap_or_default();
                        if !text.is_empty() {
                            parts.push(format!("[Page {}]\n{text}", page.page_number));
                        }
                    }
                }
            }
            if !parts.is_empty() {
                let text = format!("{}\n\n", parts.join("\n\n"));
                chars = chars.saturating_add(text.chars().count());
                bytes = bytes.saturating_add(text.len());
                if chars > super::parser::MAX_TEXT_CHARS || bytes > super::parser::MAX_TEXT_BYTES {
                    return Err(IngestError::Validation(
                        "streaming extracted text exceeds ingestion budget".into(),
                    ));
                }
                let path = text_path.clone();
                tokio::task::spawn_blocking(move || {
                    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
                    file.write_all(text.as_bytes())
                })
                .await
                .map_err(|_| IngestError::Storage)??;
            }
            super::store::KnowledgeWriteStore::require_identity(
                lease.connection(),
                &dataset.dataset_id,
                Some(&super::dataset_ingestion_identity(&dataset.to_value())),
            )
            .await?;
            super::store::KnowledgeWriteStore::update_status(
                lease.connection(),
                id,
                "parsing",
                Some((5.0 + end as f64 / total as f64 * 85.0).floor()),
                None,
            )
            .await?;
        }
        let text = tokio::task::spawn_blocking(move || {
            let mut file = std::fs::File::open(text_path)?;
            let mut text = String::new();
            std::io::Read::by_ref(&mut file)
                .take(super::parser::MAX_TEXT_BYTES as u64 + 1)
                .read_to_string(&mut text)?;
            Ok::<_, std::io::Error>(text)
        })
        .await
        .map_err(|_| IngestError::Storage)??;
        super::parser::require_text_budget(&text)?;
        if text.trim().is_empty() {
            return Err(IngestError::Validation(
                "No text extracted from document".into(),
            ));
        }
        drop(spool);
        Ok(StreamedText {
            text,
            total_pages: total,
        })
    }
}
impl super::IngestionService {
    /// File-backed sampling keeps auto detection from materializing a large PDF.
    pub(crate) async fn detect_large_pdf(
        &self,
        key: &str,
    ) -> Result<super::detection::DetectionResult> {
        let mut loader = self.streaming.clone().ok_or_else(|| {
            IngestError::Parser("large PDF detection requires streaming backend".into())
        })?;
        loader.extract_images = true;
        loader.extract_images_if_no_text = true;
        let spool = Spool::new(&self.parser.config.scratch_root)?;
        let path = spool.root.join("detect.pdf");
        let info = self
            .assets
            .backend
            .head(key)
            .await
            .map_err(|_| IngestError::Storage)?
            .ok_or(IngestError::NotFound)?;
        let size = usize::try_from(info.size_bytes)
            .map_err(|_| IngestError::Validation("source size exceeds address space".into()))?;
        self.assets
            .backend
            .download_to_path(key, &path, size)
            .await
            .map_err(|_| IngestError::Storage)?;
        let source = PdfSource::Path(path);
        let total = loader.get_page_count(&source).await?;
        let mut samples = vec![];
        for index in super::detection::DocumentTypeDetector::sample_indices(total, 5) {
            let mut pages = loader
                .load_page_range(&source, index + 1, index + 1)
                .await?;
            let page = pages
                .pop()
                .ok_or_else(|| IngestError::Parser("PDF sampled page is missing".into()))?;
            samples.push(super::detection::PageSample {
                page_index: index,
                text: page.text,
                image_count: page.image_count,
            });
        }
        let result =
            super::detection::DocumentTypeDetector::default().pdf(size, Some((total, &samples)));
        drop(spool);
        result
    }
}
