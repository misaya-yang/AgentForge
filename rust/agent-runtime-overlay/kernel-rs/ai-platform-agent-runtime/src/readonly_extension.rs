//! Native Agent extension seam for platform read-only contributors.
//!
//! The request boundary still validates and renders the immutable capability
//! payload. These contributors keep the platform integration on Codex's
//! existing extension API so later Knowledge/MCP implementations do not add a
//! second prompt or tool loop.

use std::sync::Arc;

use codex_context_fragments::AdditionalContextUserFragment;
use codex_extension_api::ContextContributor;
use codex_extension_api::ContextualUserFragment;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::PromptFragment;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolContributor;
use codex_extension_api::ToolExecutor;
use codex_extension_api::TurnInputContributor;

/// Data seeded by a host for one read-only turn. The HTTP adapter also
/// supports direct model input, while this typed value is the native
/// extension path for in-process hosts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadonlyTurnContext(pub String);

/// Install the native Agent contributor seam used by the platform runtime.
pub fn install_readonly_contributors(
    registry: &mut ExtensionRegistryBuilder<codex_core::config::Config>,
) {
    registry.prompt_contributor(Arc::new(ReadonlyContextContributor));
    registry.turn_input_contributor(Arc::new(ReadonlyTurnInputContributor));
    registry.tool_contributor(Arc::new(ReadonlyToolContributor));
}

#[derive(Debug)]
struct ReadonlyContextContributor;

impl ContextContributor for ReadonlyContextContributor {
    fn contribute_thread_context<'a>(
        &'a self,
        _session_store: &'a ExtensionData,
        thread_store: &'a ExtensionData,
    ) -> codex_extension_api::ExtensionFuture<'a, Vec<PromptFragment>> {
        Box::pin(async move {
            let Some(context) = thread_store.get::<ReadonlyTurnContext>() else {
                return Vec::new();
            };
            vec![PromptFragment::from_fragment(
                codex_extension_api::PromptSlot::ContextWindow,
                &ReadonlyContextFragment(AdditionalContextUserFragment::new(
                    "ai_platform_readonly".to_string(),
                    context.0.clone(),
                )),
            )]
        })
    }
}

/// Keep the platform classification and harness attribution with typed content.
struct ReadonlyContextFragment(AdditionalContextUserFragment);

impl ContextualUserFragment for ReadonlyContextFragment {
    fn role(&self) -> &'static str {
        self.0.role()
    }

    fn content_kind(&self) -> codex_extension_api::ContentItemKind {
        codex_extension_api::ContentItemKind("ai_platform.readonly_context".to_string())
    }

    fn markers(&self) -> (&'static str, &'static str) {
        self.0.markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        AdditionalContextUserFragment::type_markers()
    }

    fn body(&self) -> codex_protocol::models::ContentItem {
        self.0.body()
    }
}

#[derive(Debug)]
struct ReadonlyTurnInputContributor;

impl TurnInputContributor for ReadonlyTurnInputContributor {
    fn contribute<'a>(
        &'a self,
        _input: codex_extension_api::TurnInputContext,
        _extension_metrics: Option<Arc<dyn codex_extension_api::ExtensionMetrics>>,
        _session_store: &'a ExtensionData,
        _thread_store: &'a ExtensionData,
        _turn_store: &'a ExtensionData,
    ) -> codex_extension_api::ExtensionFuture<'a, Vec<Box<dyn ContextualUserFragment + Send>>> {
        Box::pin(async { Vec::new() })
    }
}

#[derive(Debug)]
struct ReadonlyToolContributor;

impl ToolContributor for ReadonlyToolContributor {
    fn tools(
        &self,
        _session_store: &ExtensionData,
        _thread_store: &ExtensionData,
    ) -> Vec<Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>> {
        // Tool schemas are supplied only after platform metadata authorization;
        // no write-capable or keyword-routed tool is registered here.
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn typed_readonly_context_preserves_text_kind_and_provenance() {
        let session = ExtensionData::new("session");
        let thread = ExtensionData::new("thread");
        thread.insert(ReadonlyTurnContext("read-only knowledge".into()));
        let fragments = ReadonlyContextContributor
            .contribute_thread_context(&session, &thread)
            .await;
        assert_eq!(fragments.len(), 1);
        let fragment = fragments.into_iter().next().expect("context fragment");
        assert_eq!(
            fragment.slot(),
            codex_extension_api::PromptSlot::ContextWindow
        );
        assert_eq!(fragment.content_kind().0, "ai_platform.readonly_context");
        assert_eq!(
            fragment.content(),
            &AdditionalContextUserFragment::new(
                "ai_platform_readonly".into(),
                "read-only knowledge".into(),
            )
            .render(),
        );
        assert_eq!(
            fragment.into_content().into_parts().2,
            codex_protocol::models::ContentItemMetadata::harness(),
        );
        assert!(ReadonlyToolContributor.tools(&session, &thread).is_empty());
    }

    #[tokio::test]
    async fn absent_readonly_context_does_not_inject_a_fragment() {
        let session = ExtensionData::new("session");
        let thread = ExtensionData::new("thread");
        assert!(
            ReadonlyContextContributor
                .contribute_thread_context(&session, &thread)
                .await
                .is_empty()
        );
    }
}
