//! Office capability adapter.
//!
//! This module is deliberately independent from the HTTP service and the
//! worker's registration code.  It is the boundary that the integration layer
//! wires to the authoritative artifact store.  The renderer returns bytes;
//! this adapter is the only place where those bytes become a product artifact.
//! No workspace path, provider credential, Python process, or fabricated URL
//! is accepted here.

use std::sync::Arc;

use ai_platform_capability_contract::canonical_json_hash;
use ai_platform_office::{
    DocumentFormat, DocumentGenerator, DocumentPreview, GenerateDocumentRequest,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

const MAX_SCOPE_FIELD: usize = 255;
const MAX_TOOL_CALL_ID: usize = 160;
const MAX_EXTERNAL_FONTS: usize = 8;
const MAX_EXTERNAL_FONT_BYTES: usize = 64 * 1024 * 1024;
const MAX_EXISTING_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

/// Facts copied from the verified Runtime lease.  They are not read from tool
/// arguments and are included in the artifact idempotency scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfficeExecutionContext {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub run_id: String,
    pub execution_id: String,
    pub tool_call_id: String,
    pub arguments_hash: String,
}

impl OfficeExecutionContext {
    fn validate(&self, arguments: &Value) -> Result<(), OfficeCapabilityError> {
        for value in [
            &self.tenant_id,
            &self.user_id,
            &self.session_id,
            &self.run_id,
            &self.execution_id,
        ] {
            if value.is_empty()
                || value.chars().count() > MAX_SCOPE_FIELD
                || value.chars().any(char::is_control)
            {
                return Err(OfficeCapabilityError::ContextInvalid);
            }
        }
        if self.tool_call_id.is_empty()
            || self.tool_call_id.chars().count() > MAX_TOOL_CALL_ID
            || self.tool_call_id.chars().any(char::is_control)
            || self.arguments_hash.len() != 71
            || !self.arguments_hash.starts_with("sha256:")
        {
            return Err(OfficeCapabilityError::ContextInvalid);
        }
        let calculated =
            canonical_json_hash(arguments).map_err(|_| OfficeCapabilityError::ArgumentsInvalid)?;
        if calculated != self.arguments_hash {
            return Err(OfficeCapabilityError::ArgumentsHashMismatch);
        }
        Ok(())
    }

    /// Stable key used by the authoritative store's unique idempotency
    /// constraint.  It intentionally includes the attempt/tool call and the
    /// canonical argument hash, so a replay cannot create a second artifact.
    pub fn idempotency_key(&self) -> String {
        format!(
            "office:v1:{}:{}:{}:{}:{}:{}:{}",
            self.tenant_id,
            self.user_id,
            self.session_id,
            self.run_id,
            self.execution_id,
            self.tool_call_id,
            self.arguments_hash
        )
    }
}

/// Store-facing request.  The worker supplies bytes and scope; the Gateway or
/// a PG/object-store adapter owns the actual upload and metadata transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfficeArtifactPut {
    pub idempotency_key: String,
    pub arguments_hash: String,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub run_id: String,
    pub execution_id: String,
    pub tool_call_id: String,
    pub artifact_type: String,
    pub format: String,
    pub title: String,
    pub filename: String,
    pub mime_type: String,
    pub sha256: String,
    pub content: Vec<u8>,
    pub metadata: Value,
}

/// The authoritative store must make this operation idempotent.  In
/// particular, an existing key must return the original record without a
/// second object upload.
#[async_trait]
pub trait OfficeArtifactStore: Send + Sync {
    async fn put_idempotent(
        &self,
        request: OfficeArtifactPut,
    ) -> Result<OfficeArtifactRecord, OfficeArtifactStoreError>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfficeArtifactRecord {
    pub artifact_id: String,
    pub download_url: String,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Error)]
pub enum OfficeArtifactStoreError {
    #[error("artifact store unavailable")]
    Unavailable,
    #[error("artifact store commit outcome unknown")]
    OutcomeUnknown,
    #[error("artifact store returned invalid metadata")]
    InvalidMetadata,
    #[error("artifact idempotency conflict")]
    IdempotencyConflict,
}

#[derive(Debug, Error)]
pub enum OfficeCapabilityError {
    #[error("office context invalid")]
    ContextInvalid,
    #[error("office arguments invalid")]
    ArgumentsInvalid,
    #[error("office argument hash mismatch")]
    ArgumentsHashMismatch,
    #[error("office rendering failed: {0}")]
    Render(String),
    #[error("office artifact store failed: {0}")]
    Store(#[from] OfficeArtifactStoreError),
    #[error("office controlled font pack invalid")]
    FontPackInvalid,
}

impl OfficeCapabilityError {
    pub fn is_side_effect_unknown(&self) -> bool {
        matches!(
            self,
            Self::Store(
                OfficeArtifactStoreError::Unavailable
                    | OfficeArtifactStoreError::OutcomeUnknown
                    | OfficeArtifactStoreError::InvalidMetadata
            )
        )
    }
}

/// Public-compatible output for `mcp_docgen__generate_document`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerateDocumentOutput {
    pub artifact_id: String,
    pub download_url: String,
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub plan_outline: String,
    pub critic_passed: bool,
}

#[derive(Clone)]
pub struct OfficeCapabilityExecutor {
    generator: DocumentGenerator,
    artifacts: Arc<dyn OfficeArtifactStore>,
    external_fonts: Arc<Vec<Vec<u8>>>,
}

impl OfficeCapabilityExecutor {
    pub fn new(artifacts: Arc<dyn OfficeArtifactStore>) -> Self {
        Self {
            generator: DocumentGenerator::new(),
            artifacts,
            external_fonts: Arc::new(Vec::new()),
        }
    }

    pub fn with_external_fonts(
        mut self,
        fonts: Vec<Vec<u8>>,
    ) -> Result<Self, OfficeCapabilityError> {
        if fonts.len() > MAX_EXTERNAL_FONTS
            || fonts.iter().any(Vec::is_empty)
            || fonts.iter().map(Vec::len).sum::<usize>() > MAX_EXTERNAL_FONT_BYTES
        {
            return Err(OfficeCapabilityError::FontPackInvalid);
        }
        self.external_fonts = Arc::new(fonts);
        Ok(self)
    }

    pub async fn generate_document(
        &self,
        context: &OfficeExecutionContext,
        arguments: Value,
    ) -> Result<GenerateDocumentOutput, OfficeCapabilityError> {
        context.validate(&arguments)?;
        let mut request_arguments = arguments.clone();
        request_arguments
            .as_object_mut()
            .ok_or(OfficeCapabilityError::ArgumentsInvalid)?
            .remove("source_base64");
        let request: GenerateDocumentRequest = serde_json::from_value(request_arguments)
            .map_err(|_| OfficeCapabilityError::ArgumentsInvalid)?;
        let rendered = if request.format == DocumentFormat::Pdf {
            let mut fonts = typst_assets::fonts().collect::<Vec<_>>();
            fonts.extend(self.external_fonts.iter().map(Vec::as_slice));
            self.generator.generate_with_fonts(&request, &fonts)
        } else {
            self.generator.generate(&request)
        }
        .map_err(|error| OfficeCapabilityError::Render(error.to_string()))?;
        let plan_outline = rendered.plan_outline.clone();
        let put = OfficeArtifactPut {
            idempotency_key: context.idempotency_key(),
            arguments_hash: context.arguments_hash.clone(),
            tenant_id: context.tenant_id.clone(),
            user_id: context.user_id.clone(),
            session_id: context.session_id.clone(),
            run_id: context.run_id.clone(),
            execution_id: context.execution_id.clone(),
            tool_call_id: context.tool_call_id.clone(),
            artifact_type: "document".to_owned(),
            format: request.format.extension().to_owned(),
            title: request.title.clone(),
            filename: rendered.filename.clone(),
            mime_type: rendered.mime_type.clone(),
            sha256: rendered.sha256.clone(),
            content: rendered.bytes,
            metadata: json!({
                "schema_version": "ai-platform/office-artifact/v1",
                "source": "generate_document",
                "verifier_status": "not_run",
                "plan_outline": plan_outline.clone(),
            }),
        };
        let record = self.artifacts.put_idempotent(put).await?;
        if record.download_url.is_empty()
            || record.artifact_id.is_empty()
            || record.sha256 != rendered.sha256
            || record.filename != rendered.filename
            || record.size_bytes != rendered.size_bytes as u64
        {
            return Err(OfficeArtifactStoreError::InvalidMetadata.into());
        }
        Ok(GenerateDocumentOutput {
            artifact_id: record.artifact_id,
            download_url: record.download_url,
            filename: record.filename,
            size_bytes: record.size_bytes,
            sha256: record.sha256,
            plan_outline: rendered.plan_outline,
            // The core renderer explicitly reports NotRun; do not claim a
            // critic pass merely because structural rendering succeeded.
            critic_passed: false,
        })
    }

    /// Losslessly edits a bounded existing OOXML package. The core copies all
    /// non-edited parts, so unknown relationships/media/custom XML are not
    /// silently discarded.
    pub async fn modify_existing_document(
        &self,
        context: &OfficeExecutionContext,
        format: DocumentFormat,
        source: &[u8],
        arguments: Value,
    ) -> Result<GenerateDocumentOutput, OfficeCapabilityError> {
        context.validate(&arguments)?;
        if source.is_empty() || source.len() > MAX_EXISTING_DOCUMENT_BYTES {
            return Err(OfficeCapabilityError::ArgumentsInvalid);
        }
        let request: GenerateDocumentRequest = serde_json::from_value(arguments.clone())
            .map_err(|_| OfficeCapabilityError::ArgumentsInvalid)?;
        let rendered = self
            .generator
            .modify_existing(format, source, &request)
            .map_err(|error| OfficeCapabilityError::Render(error.to_string()))?;
        let plan_outline = rendered.plan_outline.clone();
        let put = OfficeArtifactPut {
            idempotency_key: format!("{}:modify", context.idempotency_key()),
            arguments_hash: context.arguments_hash.clone(),
            tenant_id: context.tenant_id.clone(),
            user_id: context.user_id.clone(),
            session_id: context.session_id.clone(),
            run_id: context.run_id.clone(),
            execution_id: context.execution_id.clone(),
            tool_call_id: context.tool_call_id.clone(),
            artifact_type: "document".to_owned(),
            format: format.extension().to_owned(),
            title: request.title.clone(),
            filename: rendered.filename.clone(),
            mime_type: rendered.mime_type.clone(),
            sha256: rendered.sha256.clone(),
            content: rendered.bytes,
            metadata: json!({
                "schema_version": "ai-platform/office-artifact/v1",
                "source": "modify_existing_document",
                "verifier_status": "not_run",
                "plan_outline": plan_outline.clone(),
            }),
        };
        let record = self.artifacts.put_idempotent(put).await?;
        if record.download_url.is_empty()
            || record.artifact_id.is_empty()
            || record.sha256 != rendered.sha256
            || record.filename != rendered.filename
            || record.size_bytes != rendered.size_bytes as u64
        {
            return Err(OfficeArtifactStoreError::InvalidMetadata.into());
        }
        Ok(GenerateDocumentOutput {
            artifact_id: record.artifact_id,
            download_url: record.download_url,
            filename: record.filename,
            size_bytes: record.size_bytes,
            sha256: record.sha256,
            plan_outline,
            critic_passed: false,
        })
    }

    pub fn preview_existing_document(
        &self,
        format: DocumentFormat,
        source: &[u8],
    ) -> Result<DocumentPreview, OfficeCapabilityError> {
        if source.is_empty() || source.len() > MAX_EXISTING_DOCUMENT_BYTES {
            return Err(OfficeCapabilityError::ArgumentsInvalid);
        }
        self.generator
            .preview(format, source)
            .map_err(|error| OfficeCapabilityError::Render(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryStore {
        requests: Mutex<Vec<OfficeArtifactPut>>,
    }

    #[async_trait]
    impl OfficeArtifactStore for MemoryStore {
        async fn put_idempotent(
            &self,
            request: OfficeArtifactPut,
        ) -> Result<OfficeArtifactRecord, OfficeArtifactStoreError> {
            let mut requests = self.requests.lock().unwrap();
            if let Some(previous) = requests
                .iter()
                .find(|item| item.idempotency_key == request.idempotency_key)
            {
                if previous.sha256 != request.sha256 {
                    return Err(OfficeArtifactStoreError::IdempotencyConflict);
                }
            } else {
                requests.push(request.clone());
            }
            Ok(OfficeArtifactRecord {
                artifact_id: "art_test".to_owned(),
                download_url: "/api/v1/assistant/artifacts/art_test/download".to_owned(),
                filename: request.filename,
                mime_type: request.mime_type,
                size_bytes: request.content.len() as u64,
                sha256: request.sha256,
            })
        }
    }

    fn arguments() -> Value {
        json!({
            "format": "docx",
            "title": "Meeting notes",
            "goal": "A concise record",
            "body_markdown": "# Notes\n\n- One item",
            "locale": "en-US",
            "design_system": "enterprise"
        })
    }

    fn context(arguments: &Value) -> OfficeExecutionContext {
        OfficeExecutionContext {
            tenant_id: "tenant".to_owned(),
            user_id: "user".to_owned(),
            session_id: "session".to_owned(),
            run_id: "run".to_owned(),
            execution_id: "execution".to_owned(),
            tool_call_id: "call".to_owned(),
            arguments_hash: canonical_json_hash(arguments).unwrap(),
        }
    }

    #[tokio::test]
    async fn renders_and_persists_public_output_without_fake_url() {
        let store = Arc::new(MemoryStore::default());
        let executor = OfficeCapabilityExecutor::new(store.clone());
        let args = arguments();
        let output = executor
            .generate_document(&context(&args), args)
            .await
            .unwrap();
        assert_eq!(output.artifact_id, "art_test");
        assert!(output.download_url.starts_with('/'));
        assert!(!output.critic_passed);
        assert_eq!(store.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn replay_uses_store_idempotency_key() {
        let store = Arc::new(MemoryStore::default());
        let executor = OfficeCapabilityExecutor::new(store.clone());
        let args = arguments();
        let ctx = context(&args);
        executor
            .generate_document(&ctx, args.clone())
            .await
            .unwrap();
        executor.generate_document(&ctx, args).await.unwrap();
        assert_eq!(store.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn pdf_uses_the_controlled_embedded_font_pack() {
        let store = Arc::new(MemoryStore::default());
        let executor = OfficeCapabilityExecutor::new(store);
        let mut args = arguments();
        args["format"] = json!("pdf");
        let output = executor
            .generate_document(&context(&args), args)
            .await
            .expect("embedded font pack renders English PDF");
        assert!(output.filename.ends_with(".pdf"));
        assert!(!output.critic_passed);
    }

    #[tokio::test]
    async fn rejects_argument_hash_mismatch_before_render_or_store() {
        let store = Arc::new(MemoryStore::default());
        let executor = OfficeCapabilityExecutor::new(store.clone());
        let args = arguments();
        let mut ctx = context(&args);
        ctx.arguments_hash =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_owned();
        assert!(matches!(
            executor.generate_document(&ctx, args).await,
            Err(OfficeCapabilityError::ArgumentsHashMismatch)
        ));
        assert!(store.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_store_metadata_is_side_effect_unknown() {
        struct BadStore;
        #[async_trait]
        impl OfficeArtifactStore for BadStore {
            async fn put_idempotent(
                &self,
                request: OfficeArtifactPut,
            ) -> Result<OfficeArtifactRecord, OfficeArtifactStoreError> {
                Ok(OfficeArtifactRecord {
                    artifact_id: "art".to_owned(),
                    download_url: String::new(),
                    filename: request.filename,
                    mime_type: request.mime_type,
                    size_bytes: request.content.len() as u64,
                    sha256: request.sha256,
                })
            }
        }
        let executor = OfficeCapabilityExecutor::new(Arc::new(BadStore));
        let args = arguments();
        let error = executor
            .generate_document(&context(&args), args)
            .await
            .unwrap_err();
        assert!(error.is_side_effect_unknown());
    }
}
