//! Standalone OCR helpers corresponding to knowledge/ocr_utils.py. These do not
//! enable whole-PDF OCR in the deliberately bounded text-only parser API.
use crate::migration_knowledge_ingest::{
    ocr::{VlmOcrService, ocr_image_auto},
    parser::{DocumentParser, MAX_PAGES, MAX_SOURCE_BYTES, ParserConfig},
    vision::PdfRasterBackend,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[derive(Clone, Debug)]
pub struct OcrConfig {
    pub languages: String,
    pub dpi: u32,
    pub timeout_seconds: u64,
}
impl Default for OcrConfig {
    fn default() -> Self {
        Self::new("eng+ara", 200, 60)
    }
}
impl OcrConfig {
    pub fn new(languages: &str, dpi: i64, timeout_seconds: i64) -> Self {
        let languages = languages.trim();
        const ALLOWED: &[&str] = &[
            "eng", "ara", "chi_sim", "chi_tra", "fra", "deu", "spa", "rus", "jpn", "kor", "por",
            "ita", "nld", "tur", "vie", "tha",
        ];
        let valid = !languages.is_empty()
            && languages
                .split('+')
                .all(|part| ALLOWED.contains(&part.trim()));
        Self {
            languages: if valid { languages } else { "eng+ara" }.into(),
            dpi: dpi.clamp(72, 1200) as u32,
            timeout_seconds: timeout_seconds.clamp(1, 300) as u64,
        }
    }
    pub fn apply(&self, mut parser: ParserConfig) -> DocumentParser {
        parser.ocr_languages = self.languages.clone();
        parser.ocr_dpi = self.dpi;
        parser.timeout = Duration::from_secs(self.timeout_seconds);
        DocumentParser::new(parser)
    }
}
/// The utility contract is best-effort empty text on missing engines/timeouts.
/// The ingestion parser's stricter Result-returning method remains unchanged.
pub fn ocr_image_bytes(bytes: &[u8], parser: &DocumentParser, fallback_to_eng: bool) -> String {
    parser
        .ocr_image_bytes(bytes, fallback_to_eng)
        .unwrap_or_default()
}
pub async fn ocr_image_bytes_auto(
    bytes: &[u8],
    vlm: Option<&VlmOcrService>,
    parser: &DocumentParser,
    strategy: &str,
) -> String {
    ocr_image_auto(bytes, vlm, parser, strategy)
        .await
        .unwrap_or_default()
}
/// Bounded native rendering and OCR; completion order never changes page order.
/// Rendering and Tesseract run off the async executor. Each worker owns its
/// decoder invocation and temporary files, including cancellation cleanup.
pub async fn ocr_pdf_bytes(
    content: Arc<[u8]>,
    backend: Arc<dyn PdfRasterBackend>,
    parser: DocumentParser,
    max_workers: usize,
    max_pending_factor: usize,
) -> String {
    if content.len() > MAX_SOURCE_BYTES || parser.config.tesseract.is_none() {
        return String::new();
    }
    let count_backend = backend.clone();
    let count_content = content.clone();
    let count =
        match tokio::task::spawn_blocking(move || count_backend.page_count(&count_content)).await {
            Ok(Ok(count)) if count <= MAX_PAGES => count,
            _ => return String::new(),
        };
    let workers = max_workers.clamp(1, 4);
    let pending = workers
        .saturating_mul(max_pending_factor.max(1))
        .min(MAX_PAGES);
    let permits = Arc::new(tokio::sync::Semaphore::new(workers));
    let mut tasks: tokio::task::JoinSet<(usize, String)> = tokio::task::JoinSet::new();
    let mut parts = BTreeMap::new();
    for page in 0..count {
        while tasks.len() >= pending {
            if let Some(Ok((page, text))) = tasks.join_next().await {
                if !text.is_empty() {
                    parts.insert(page, text);
                }
            }
        }
        let backend = backend.clone();
        let content = content.clone();
        let parser = parser.clone();
        let permits = permits.clone();
        tasks.spawn(async move {
            let Ok(permit) = permits.acquire_owned().await else {
                return (page, String::new());
            };
            let text = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let Ok(image) = backend.render(&content, page, Some(parser.config.ocr_dpi)) else {
                    return String::new();
                };
                ocr_image_bytes(&image.png, &parser, true)
            })
            .await
            .unwrap_or_default();
            (page, text)
        });
    }
    while let Some(result) = tasks.join_next().await {
        if let Ok((page, text)) = result {
            if !text.is_empty() {
                parts.insert(page, text);
            }
        }
    }
    parts.into_values().collect::<Vec<_>>().join("\n\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn language_whitelist_and_source_clamps() {
        let c = OcrConfig::new(" eng+ fra ", 0, 999);
        assert_eq!(c.languages, "eng+ fra");
        assert_eq!(c.dpi, 72);
        assert_eq!(c.timeout_seconds, 300);
        assert_eq!(OcrConfig::new("eng;id", 200, 60).languages, "eng+ara");
        assert_eq!(OcrConfig::new("", 200, 60).languages, "eng+ara");
        assert_eq!(OcrConfig::new("chi_sim+eng", 9999, -1).timeout_seconds, 1);
    }
    #[tokio::test]
    async fn absent_optional_ocr_returns_empty_for_all_strategies() {
        let parser = DocumentParser::new(ParserConfig::new(std::env::temp_dir()));
        for strategy in ["tesseract", "vlm", "hybrid", "unknown"] {
            assert!(
                ocr_image_bytes_auto(b"image", None, &parser, strategy)
                    .await
                    .is_empty()
            );
        }
    }
}
