//! Ordinary source re-extraction policy from ingestion_service.py:724-840.
//! Receipt writes, validation, access control and cancellation never fall back.
use super::{IngestError, Result};
pub(super) fn needs_legacy_pdf_ocr(
    used_multimodal: bool,
    text: &str,
    filename: &str,
    mime: &str,
    enabled: bool,
    min_chars: usize,
) -> bool {
    used_multimodal
        && enabled
        && text.trim().chars().count() < min_chars
        && (filename.to_lowercase().ends_with(".pdf") || mime.to_lowercase().contains("pdf"))
}
pub(super) fn settle(
    result: Result<String>,
    receipt_failed: bool,
    stored: &str,
) -> Result<Option<String>> {
    match result {
        Ok(text) => {
            super::parser::require_text_budget(&text)?;
            Ok(
                (!text.is_empty() && text.trim().chars().count() > stored.trim().chars().count())
                    .then_some(text),
            )
        }
        // NotFound here means a missing original object, not a dataset/document
        // authorization lookup. The entire attempt executes after the ACL/lease.
        Err(IngestError::Storage | IngestError::NotFound) if !receipt_failed => Ok(None),
        Err(error) => Err(error),
    }
}
pub(super) fn storage_error(error: crate::migration_content::ContentError) -> IngestError {
    match error.status {
        401 | 403 => IngestError::Permission,
        404 => IngestError::NotFound,
        413 => IngestError::Validation("stored document exceeds byte budget".into()),
        _ => IngestError::Storage,
    }
}
