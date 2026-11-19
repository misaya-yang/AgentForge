//! Native PDF engine ports and redacted errors; no ingestion/parser implementation.
use std::fmt;
use crate::knowledge_types::QueryError;
use axum::{Json,http::StatusCode,response::{IntoResponse,Response}};
use serde_json::json;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestError {
    Validation(String),
    NotFound,
    Permission,
    LeaseBusy,
    // Internal control flow: only the ordinary publication lease failed before
    // any publication mutation. The document owner may CAS its same execution
    // back to waiting; this must never become an error terminal.
    PublicationContended,
    GenerationChanged,
    Database,
    Storage,
    Parser(String),
    Index(String),
    Cancelled,
}
impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(s) | Self::Parser(s) | Self::Index(s) => f.write_str(s),
            Self::NotFound => f.write_str("document or dataset not found"),
            Self::Permission => f.write_str("missing dataset permission"),
            Self::LeaseBusy | Self::PublicationContended => {
                f.write_str("document or dataset index update is already in progress")
            }
            Self::GenerationChanged => f.write_str("document or dataset generation changed; retry"),
            Self::Database => f.write_str("knowledge persistence unavailable"),
            Self::Storage => f.write_str("knowledge source storage unavailable"),
            Self::Cancelled => f.write_str("knowledge ingestion cancelled"),
        }
    }
}
impl std::error::Error for IngestError {}
impl From<sqlx::Error> for IngestError {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}
impl From<std::io::Error> for IngestError {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}
impl From<serde_json::Error> for IngestError {
    fn from(_: serde_json::Error) -> Self {
        Self::Validation("invalid knowledge document shape".into())
    }
}
impl IntoResponse for IngestError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::Permission => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::LeaseBusy | Self::GenerationChanged => StatusCode::CONFLICT,
            Self::Validation(_) | Self::Parser(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        (status, Json(json!({"detail":self.to_string()}))).into_response()
    }
}
impl From<QueryError> for IngestError {
    fn from(e: QueryError) -> Self {
        match e {
            QueryError::Forbidden => Self::Permission,
            QueryError::NotFound => Self::NotFound,
            QueryError::GenerationChanged => Self::GenerationChanged,
            QueryError::Validation(s) => Self::Validation(s),
            _ => Self::Index(e.to_string()),
        }
    }
}
pub type Result<T> = std::result::Result<T, IngestError>;

pub mod parser {
use super::Result;
pub trait PdfTextDocument {
    fn page_count(&self) -> usize;
    fn page_text(&mut self, index: usize) -> Result<String>;
}
pub trait PdfTextBackend: Send + Sync {
    fn open_pypdf(&self, bytes: &[u8], stream_limit: usize) -> Result<Box<dyn PdfTextDocument>>;
    fn open_pymupdf(&self, bytes: &[u8]) -> Result<Box<dyn PdfTextDocument>>;
}

}
pub mod images {
use super::Result;
pub struct PdfImage {
    pub bytes: Vec<u8>,
    pub extension: String,
    pub width: u32,
    pub height: u32,
    pub xref: i64,
}
/// Matching PyMuPDF engine boundary: each page call opens its own document handle.
pub trait PdfImageBackend: Send + Sync {
    fn page_texts(&self, bytes: &[u8]) -> Result<Vec<String>>;
    fn page_images(&self, bytes: &[u8], zero_based_page: usize) -> Result<Vec<PdfImage>>;
}

}
pub mod vision {
use super::Result;
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

}
pub mod pdf_splitter {
use super::Result;
use std::path::Path;
pub trait PdfSplitBackend: Send + Sync {
    fn page_count(&self, source: &Path) -> Result<usize>;
    fn write_pages(
        &self,
        source: &Path,
        start: usize,
        end: usize,
        destination: &Path,
    ) -> Result<()>;
}

}
pub mod streaming {
use super::Result;
use std::{path::PathBuf,sync::Arc};
use serde::{Serialize,Deserialize};
#[derive(Clone, Debug)]
pub enum PdfSource {
    Bytes(Arc<Vec<u8>>),
    Path(PathBuf),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageContent {
    pub page_number: usize,
    pub text: String,
    pub images: Vec<Vec<u8>>,
    pub image_count: usize,
    pub char_count: usize,
}
impl PageContent {
    pub fn new(page_number: usize, text: String, images: Vec<Vec<u8>>) -> Self {
        Self {
            page_number,
            char_count: text.chars().count(),
            image_count: images.len(),
            text,
            images,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchResult {
    pub pages: Vec<PageContent>,
    pub batch_index: usize,
    pub start_page: usize,
    pub end_page: usize,
    pub total_pages: usize,
    pub progress: f64,
}
impl BatchResult {
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
    pub fn total_chars(&self) -> usize {
        self.pages.iter().map(|p| p.char_count).sum()
    }
    pub fn total_images(&self) -> usize {
        self.pages.iter().map(|p| p.image_count).sum()
    }
}
pub trait StreamingPdfBackend: Send + Sync {
    fn page_count(&self, source: &PdfSource) -> Result<usize>;
    fn extract_page(
        &self,
        source: &PdfSource,
        page_index: usize,
        extract_images: bool,
        extract_images_if_no_text: bool,
        max_image_dimension: usize,
    ) -> Result<PageContent>;
    fn release_memory(&self) {}
}

}
