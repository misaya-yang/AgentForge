//! Source-compatible document detection over real page samples. PDF page
//! extraction is supplied by the configured native backend, never guessed.
use super::{IngestError, ProcessingMode, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetectionResult {
    pub document_type: String,
    pub recommended_mode: ProcessingMode,
    pub confidence: f64,
    pub file_size: usize,
    pub page_count: Option<usize>,
    pub has_images: bool,
    pub text_coverage: f64,
    pub is_large_file: bool,
    pub sample_pages_checked: usize,
    pub details: Value,
}
#[derive(Clone, Debug)]
pub struct PageSample {
    pub page_index: usize,
    pub text: String,
    pub image_count: usize,
}
#[derive(Clone, Debug)]
pub struct DocumentTypeDetector {
    pub large_file_threshold: usize,
    pub sample_pages: usize,
    pub native_pdf_threshold: f64,
    pub scanned_pdf_threshold: f64,
    pub min_chars_per_page: usize,
    pub max_memory_size: usize,
}
impl Default for DocumentTypeDetector {
    fn default() -> Self {
        Self {
            large_file_threshold: 50 * 1024 * 1024,
            sample_pages: 5,
            native_pdf_threshold: 0.8,
            scanned_pdf_threshold: 0.2,
            min_chars_per_page: 50,
            max_memory_size: 100 * 1024 * 1024,
        }
    }
}
impl DocumentTypeDetector {
    fn result(
        &self,
        kind: &str,
        mode: ProcessingMode,
        confidence: f64,
        size: usize,
    ) -> DetectionResult {
        DetectionResult {
            document_type: kind.into(),
            recommended_mode: mode,
            confidence,
            file_size: size,
            page_count: None,
            has_images: false,
            text_coverage: 0.0,
            is_large_file: size > self.large_file_threshold,
            sample_pages_checked: 0,
            details: json!({}),
        }
    }
    pub fn detect(
        &self,
        bytes: &[u8],
        filename: &str,
        mime: &str,
        pdf_samples: Option<(usize, &[PageSample])>,
    ) -> Result<DetectionResult> {
        if bytes.len() > self.max_memory_size {
            return Err(IngestError::Validation(
                "Content too large for memory processing; use file path instead".into(),
            ));
        }
        let name = filename.trim().to_lowercase();
        let mime = mime.trim().to_lowercase();
        let size = bytes.len();
        if name.ends_with(".pdf") || mime.contains("application/pdf") {
            return self.pdf(size, pdf_samples);
        }
        if name.ends_with(".docx") || mime.contains("officedocument.wordprocessing") {
            return Ok(self.result("docx", ProcessingMode::TextOnly, 0.95, size));
        }
        if name.ends_with(".txt") || mime.contains("text/plain") {
            return Ok(self.result("txt", ProcessingMode::TextOnly, 0.99, size));
        }
        if name.ends_with(".md") || name.ends_with(".markdown") {
            return Ok(self.result("markdown", ProcessingMode::TextOnly, 0.99, size));
        }
        if name.ends_with(".html") || name.ends_with(".htm") || mime.contains("text/html") {
            return Ok(self.result("html", ProcessingMode::TextOnly, 0.95, size));
        }
        if [".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp"]
            .iter()
            .any(|ext| name.ends_with(ext))
        {
            return Ok(self.result("image", ProcessingMode::Scanned, 0.99, size));
        }
        if bytes.starts_with(b"%PDF") {
            return self.pdf(size, pdf_samples);
        }
        Ok(self.result("unknown", ProcessingMode::TextOnly, 0.5, size))
    }
    pub fn sample_indices(total: usize, count: usize) -> Vec<usize> {
        if total <= count {
            return (0..total).collect();
        }
        let first = count.min(3);
        let mut indices: Vec<_> = (0..first).collect();
        let mut remaining = count - first;
        if remaining > 0 {
            let middle = total / 2;
            if !indices.contains(&middle) {
                indices.push(middle);
                remaining -= 1
            }
            for i in 1..=remaining {
                if let Some(index) = total.checked_sub(i) {
                    if !indices.contains(&index) {
                        indices.push(index)
                    }
                }
            }
        }
        indices.sort_unstable();
        indices
    }
    pub fn pdf(
        &self,
        size: usize,
        samples: Option<(usize, &[PageSample])>,
    ) -> Result<DetectionResult> {
        let (total, pages) = samples.ok_or_else(|| {
            IngestError::Parser(
                "PDF detection requires real PyMuPDF-compatible page samples".into(),
            )
        })?;
        let count = self.sample_pages.min(total);
        let indices = Self::sample_indices(total, count);
        let mut text_pages = 0;
        let mut image_pages = 0;
        let mut chars = 0;
        for index in &indices {
            let page = pages
                .iter()
                .find(|p| p.page_index == *index)
                .ok_or_else(|| IngestError::Parser("PDF detector page sample is missing".into()))?;
            let n = page.text.trim().chars().count();
            chars += n;
            if n >= self.min_chars_per_page {
                text_pages += 1
            }
            if page.image_count > 0 {
                image_pages += 1
            }
        }
        let coverage = if count == 0 {
            0.0
        } else {
            text_pages as f64 / count as f64
        };
        let has_images = image_pages > 0;
        let (kind, mode, confidence) = if coverage >= self.native_pdf_threshold {
            (
                "native_pdf",
                if has_images {
                    ProcessingMode::Multimodal
                } else {
                    ProcessingMode::TextOnly
                },
                0.9,
            )
        } else if coverage <= self.scanned_pdf_threshold {
            ("scanned_pdf", ProcessingMode::Scanned, 0.9)
        } else {
            ("mixed_pdf", ProcessingMode::Multimodal, 0.8)
        };
        let mut result = self.result(kind, mode, confidence, size);
        result.page_count = Some(total);
        result.has_images = has_images;
        result.text_coverage = coverage;
        result.sample_pages_checked = count;
        result.details = json!({"pages_with_text":text_pages,"pages_with_images":image_pages,"total_chars_sampled":chars,"sample_indices":indices});
        Ok(result)
    }
}
