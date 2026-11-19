//! Path-backed upload admission and bounded PDF publication, matching the service route.
use crate::migration_knowledge_ingest::{
    self as ingest, Actor, IngestError, IngestionService, pdf_splitter::PdfSplitBackend,
};
use axum::{
    Json,
    extract::{FromRequest, Multipart, Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::AsyncWriteExt;
#[derive(Clone)]
pub struct PdfUploadConfig {
    pub threshold: usize,
    pub pages_per_part: usize,
    pub max_pages: usize,
    pub max_output_bytes: u64,
}
impl Default for PdfUploadConfig {
    fn default() -> Self {
        Self {
            threshold: 20 * 1024 * 1024,
            pages_per_part: 500,
            max_pages: 2000,
            max_output_bytes: 96 * 1024 * 1024,
        }
    }
}
#[derive(Clone)]
pub struct UploadState {
    pub service: Arc<IngestionService>,
    pub backend: Option<Arc<dyn PdfSplitBackend>>,
    pub maximum_upload_bytes: usize,
    pub config: PdfUploadConfig,
}
struct Workspace(PathBuf);
impl Workspace {
    fn new() -> std::io::Result<Self> {
        let p = std::env::temp_dir().join(format!("kb_upload_{}", uuid::Uuid::new_v4()));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(&p)?;
        Ok(Self(p))
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Failure(StatusCode, String);
impl From<IngestError> for Failure {
    fn from(e: IngestError) -> Self {
        let code = match e {
            IngestError::Permission => StatusCode::FORBIDDEN,
            IngestError::NotFound => StatusCode::NOT_FOUND,
            IngestError::Validation(_) | IngestError::Parser(_) => StatusCode::BAD_REQUEST,
            IngestError::GenerationChanged | IngestError::LeaseBusy => StatusCode::CONFLICT,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        Self(code, e.to_string())
    }
}
impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "Upload temporary storage unavailable".into(),
        )
    }
}
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"detail":self.1}))).into_response()
    }
}
fn invalid(message: impl Into<String>) -> Failure {
    Failure(StatusCode::BAD_REQUEST, message.into())
}
pub async fn intercept(State(state): State<UploadState>, request: Request, next: Next) -> Response {
    let parts = request.uri().path().split('/').collect::<Vec<_>>();
    if request.method() != Method::POST
        || parts.len() != 7
        || parts[1..4] != ["api", "v1", "knowledge"]
        || parts[5..] != ["documents", "upload"]
    {
        return next.run(request).await;
    }
    let dataset = parts[4].to_owned();
    let Some(actor) = request.extensions().get::<Actor>().cloned() else {
        return Failure::from(IngestError::Permission).into_response();
    };
    match upload(state, actor, dataset, request).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}
async fn upload(
    s: UploadState,
    actor: Actor,
    dataset: String,
    request: Request,
) -> Result<Value, Failure> {
    if actor.anonymous() {
        return Err(IngestError::Permission.into());
    }
    let d = s
        .service
        .store
        .require_dataset_access(&actor, &dataset, "editor")
        .await?;
    ingest::require_index_writable(&d)?;
    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|_| invalid("Invalid multipart upload"))?;
    let workspace = Arc::new(Workspace::new()?);
    let source = workspace.0.join("source");
    let mut seen = false;
    let mut filename = String::new();
    let mut mime = String::new();
    let mut mode = "auto".to_owned();
    let mut size = 0usize;
    let limit = s.maximum_upload_bytes.clamp(1024 * 1024, 48 * 1024 * 1024);
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| invalid("Invalid multipart upload"))?
    {
        match field.name().unwrap_or("") {
            "processing_mode" => {
                let v = field
                    .text()
                    .await
                    .map_err(|_| invalid("Invalid processing mode"))?;
                if v.len() > 64 {
                    return Err(invalid("Invalid processing mode"));
                }
                mode = if v.is_empty() {
                    "auto".into()
                } else {
                    v.trim().to_lowercase()
                };
            }
            "file" => {
                if seen {
                    return Err(invalid("Only one file is accepted"));
                }
                seen = true;
                filename = field
                    .file_name()
                    .filter(|n| !n.is_empty())
                    .unwrap_or("upload")
                    .to_owned();
                mime = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_owned();
                let mut file = tokio::fs::File::create(&source).await?;
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|_| invalid("Upload stream failed"))?
                {
                    size = size.saturating_add(chunk.len());
                    if size > limit {
                        return Err(Failure(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            format!(
                                "File too large: {:.1}MB exceeds limit of {}MB",
                                size as f64 / 1048576.,
                                limit / 1048576
                            ),
                        ));
                    }
                    file.write_all(&chunk).await?;
                }
                file.flush().await?;
            }
            _ => {}
        }
    }
    if !seen {
        return Err(invalid("file is required"));
    }
    if mode == "auto" {
        mode = "text_only".into()
    }
    if mode != "text_only" {
        return Err(invalid(
            "scanned and multimodal uploads are disabled until the unified multimodal index profile is released",
        ));
    }
    let ext = Path::new(&filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !["pdf", "docx", "txt", "md", "html"].contains(&ext.as_str()) {
        return Err(invalid(format!(
            "Unsupported file type: .{ext}. Allowed: .docx, .html, .md, .pdf, .txt"
        )));
    }
    let mut total = 0;
    let mut parts = Vec::new();
    if ext == "pdf" {
        let backend = s.backend.clone().ok_or_else(|| {
            Failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "PDF decoder unavailable".into(),
            )
        })?;
        let path = source.clone();
        let config = s.config.clone();
        let owner = workspace.clone();
        let split = size > config.threshold;
        let output = tokio::task::spawn_blocking(move || {
            let _owner = owner;
            prepare_pdf(backend.as_ref(), &path, &config, split)
        })
        .await
        .map_err(|_| invalid("Invalid or unreadable PDF"))??;
        total = output.0;
        parts = output.1;
    }
    if !parts.is_empty() {
        let mut documents = Vec::new();
        let stem = Path::new(&filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("upload");
        for (i, (first, last, path)) in parts.iter().enumerate() {
            let name = format!("{stem}_Part_{}_p{first}-{last}.pdf", i + 1);
            let bytes = read_bounded(path, &name, "pdf").await?;
            let doc = publish(
                &s,
                &actor,
                &dataset,
                &name,
                &bytes,
                "application/pdf",
                &mode,
            )
            .await?;
            tokio::fs::remove_file(path).await?;
            documents.push(doc);
        }
        return Ok(
            json!({"status":"split_and_queued","original_filename":filename,"total_pages":total,"parts":documents.len(),"documents":documents}),
        );
    }
    let bytes = read_bounded(&source, &filename, &ext).await?;
    publish(&s, &actor, &dataset, &filename, &bytes, &mime, &mode).await
}
fn prepare_pdf(
    backend: &dyn PdfSplitBackend,
    source: &Path,
    config: &PdfUploadConfig,
    split: bool,
) -> Result<(usize, Vec<(usize, usize, PathBuf)>), Failure> {
    let total = backend
        .page_count(source)
        .map_err(|_| invalid("Invalid or unreadable PDF"))?;
    if total == 0 {
        return Err(invalid("Invalid or unreadable PDF"));
    }
    if total > config.max_pages {
        return Err(Failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "PDF has {total} pages; maximum allowed is {}",
                config.max_pages
            ),
        ));
    }
    let mut parts = Vec::new();
    let mut bytes = 0;
    if split {
        let step = config.pages_per_part.clamp(1, config.max_pages);
        for start in (0..total).step_by(step) {
            let end = (start + step).min(total);
            let path = source.with_file_name(format!("part_{start}.pdf"));
            backend
                .write_pages(source, start, end, &path)
                .map_err(|_| invalid("Invalid or unreadable PDF"))?;
            bytes += std::fs::metadata(&path)?.len();
            if bytes > config.max_output_bytes {
                return Err(Failure(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!(
                        "PDF split output exceeds the cumulative safety limit of {} bytes",
                        config.max_output_bytes
                    ),
                ));
            }
            parts.push((start + 1, end, path));
        }
    }
    Ok((total, parts))
}
async fn read_bounded(path: &Path, name: &str, ext: &str) -> Result<Vec<u8>, Failure> {
    let limit = if ext == "html" {
        ingest::parser::MAX_HTML_BYTES
    } else {
        ingest::parser::MAX_SOURCE_BYTES
    };
    if tokio::fs::metadata(path).await?.len() > limit as u64 {
        return Err(invalid(format!(
            "{name} exceeds the {}MB parser limit for .{ext} files",
            limit / 1048576
        )));
    }
    Ok(tokio::fs::read(path).await?)
}
async fn publish(
    s: &UploadState,
    a: &Actor,
    d: &str,
    name: &str,
    bytes: &[u8],
    mime: &str,
    mode: &str,
) -> Result<Value, Failure> {
    let doc = s
        .service
        .create_document_from_upload(a, d, name, bytes, mime, json!({}), mode)
        .await
        .map_err(|e| match e {
            IngestError::Validation(_) => Failure(
                StatusCode::CONFLICT,
                "Upload outcome is uncertain; check the document list before retrying".into(),
            ),
            other => other.into(),
        })?;
    let id = doc["document_id"].as_str().unwrap_or("");
    if !s
        .service
        .store
        .enqueue(d, id, None, None, None, false)
        .await?
    {
        return Err(IngestError::GenerationChanged.into());
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Pdf(usize);
    impl PdfSplitBackend for Pdf {
        fn page_count(&self, _: &Path) -> ingest::Result<usize> {
            Ok(self.0)
        }
        fn write_pages(
            &self,
            _: &Path,
            start: usize,
            end: usize,
            destination: &Path,
        ) -> ingest::Result<()> {
            std::fs::write(destination, vec![b'x'; (end - start) * 8])?;
            Ok(())
        }
    }
    #[test]
    fn preflight_rejects_page_limit_before_publication() {
        let w = Workspace::new().unwrap();
        let result = prepare_pdf(
            &Pdf(2001),
            &w.0.join("source"),
            &PdfUploadConfig::default(),
            false,
        );
        assert!(matches!(
            result,
            Err(Failure(StatusCode::PAYLOAD_TOO_LARGE, _))
        ));
        assert_eq!(std::fs::read_dir(&w.0).unwrap().count(), 0);
    }
    #[test]
    fn split_ranges_are_one_based_in_response_and_bounded() {
        let w = Workspace::new().unwrap();
        let config = PdfUploadConfig {
            pages_per_part: 2,
            ..Default::default()
        };
        let (total, parts) = prepare_pdf(&Pdf(5), &w.0.join("source"), &config, true)
            .unwrap_or_else(|_| panic!("split"));
        assert_eq!(total, 5);
        assert_eq!(
            parts.iter().map(|(a, b, _)| (*a, *b)).collect::<Vec<_>>(),
            vec![(1, 2), (3, 4), (5, 5)]
        );
        let path = w.0.clone();
        drop(w);
        assert!(!path.exists());
    }
    #[test]
    fn cumulative_limit_covers_all_parts() {
        let w = Workspace::new().unwrap();
        let config = PdfUploadConfig {
            pages_per_part: 2,
            max_output_bytes: 20,
            ..Default::default()
        };
        assert!(matches!(
            prepare_pdf(&Pdf(5), &w.0.join("source"), &config, true),
            Err(Failure(StatusCode::PAYLOAD_TOO_LARGE, _))
        ));
    }
    #[test]
    fn below_threshold_still_probes_pdf_without_writing_parts() {
        let w = Workspace::new().unwrap();
        let (pages, parts) = prepare_pdf(
            &Pdf(10),
            &w.0.join("source"),
            &PdfUploadConfig::default(),
            false,
        )
        .unwrap_or_else(|_| panic!("probe"));
        assert_eq!(pages, 10);
        assert!(parts.is_empty());
        assert!(
            prepare_pdf(
                &Pdf(0),
                &w.0.join("source"),
                &PdfUploadConfig::default(),
                false
            )
            .is_err()
        );
    }
}
