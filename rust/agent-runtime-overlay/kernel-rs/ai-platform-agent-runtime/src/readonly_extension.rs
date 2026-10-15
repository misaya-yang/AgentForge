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
            vec![PromptFragment::new(
                codex_extension_api::PromptSlot::ContextWindow,
                AdditionalContextUserFragment::new(
                    "ai_platform_readonly".to_string(),
                    context.0.clone(),
                )
                .render(),
                codex_extension_api::ContentItemKind("ai_platform.readonly_context".to_string()),
            )]
        })
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
