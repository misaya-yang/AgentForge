pub use ai_platform_service_primitives::content::attachments::*;
#[cfg(test)]
mod shared_pdf_contract_tests {
    use super::super::ContentError;
    use super::*;
    struct Strict;
    impl PdfAttachmentParser for Strict {
        fn extract_strict(
            &self,
            raw: &[u8],
            max_pages: usize,
        ) -> Result<(String, usize), ContentError> {
            if raw != b"synthetic-pdf" || max_pages == 0 {
                return Err(ContentError::new(422, "PDF cannot be safely parsed"));
            }
            Ok(("fixture text".into(), 1))
        }
    }
    #[test]
    fn domain_public_path_is_the_neutral_pdf_trait_with_same_error_type() {
        let domain: Box<dyn PdfAttachmentParser> = Box::new(Strict);
        let shared: Box<dyn ai_platform_service_primitives::content::PdfAttachmentParser> = domain;
        assert_eq!(
            shared.extract_strict(b"synthetic-pdf", 1).unwrap(),
            ("fixture text".into(), 1)
        );
        let error: ai_platform_service_primitives::content::ContentError =
            shared.extract_strict(b"malformed", 1).unwrap_err();
        assert_eq!(error.status, 422);
        assert!(shared.extract_strict(b"synthetic-pdf", 0).is_err());
    }
}
