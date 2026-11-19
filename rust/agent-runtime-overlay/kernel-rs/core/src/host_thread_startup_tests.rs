use std::sync::Arc;

use codex_extension_api::ExtensionDataInit;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::HostThreadStartupData;
use codex_extension_api::RequireDurableThreadStore;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ToolDispatchError;
use codex_extension_api::ToolPolicy;
use codex_protocol::ThreadId;
use codex_protocol::dynamic_tools::DynamicToolSpec;

use super::Config;
use super::resolve_host_thread_startup;

struct DurableScope {
    thread_id: ThreadId,
    result: Result<HostThreadStartupData, ToolDispatchError>,
}

impl ThreadLifecycleContributor<Config> for DurableScope {
    fn resolve_host_thread_startup<'a>(
        &'a self,
        thread_id: Option<ThreadId>,
        _parent_thread_id: Option<ThreadId>,
        _forked_from_thread_id: Option<ThreadId>,
    ) -> ExtensionFuture<'a, Result<Option<HostThreadStartupData>, ToolDispatchError>> {
        Box::pin(async move {
            assert_eq!(thread_id, Some(self.thread_id));
            self.result.clone().map(Some)
        })
    }
}

#[tokio::test]
async fn unloaded_parent_resume_uses_durable_scope_without_client_metadata() {
    let child_id = ThreadId::new();
    let tool: DynamicToolSpec = serde_json::from_value(serde_json::json!({
        "type": "function", "name": "approved_read", "description": "Read", "inputSchema": {}
    }))
    .expect("valid native dynamic tool");
    let mut builder = ExtensionRegistryBuilder::<Config>::new();
    builder.thread_lifecycle_contributor(Arc::new(DurableScope {
        thread_id: child_id,
        result: Ok(HostThreadStartupData {
            tool_policy: ToolPolicy {
                allowed_tools: Some(Vec::new()),
                ..Default::default()
            },
            dynamic_tools: vec![tool.clone()],
        }),
    }));
    let mut init = ExtensionDataInit::new();
    let mut tools = Vec::new();
    assert!(
        resolve_host_thread_startup(
            &builder.build(),
            Some(child_id),
            Some(ThreadId::new()),
            None,
            &mut init,
            &mut tools,
        )
        .await
        .expect("persisted scope survives unloaded parent")
    );
    assert_eq!(
        init.get::<ToolPolicy>().expect("policy").allowed_tools,
        Some(Vec::new())
    );
    assert!(init.get::<RequireDurableThreadStore>().is_some());
    assert_eq!(tools, vec![tool]);
}

#[tokio::test]
async fn missing_legacy_or_deleted_scope_cannot_default_to_unrestricted_tools() {
    let child_id = ThreadId::new();
    let mut builder = ExtensionRegistryBuilder::<Config>::new();
    builder.thread_lifecycle_contributor(Arc::new(DurableScope {
        thread_id: child_id,
        result: Err(ToolDispatchError {
            code: "missing_authority".to_owned(),
            message: "durable root authority unavailable".to_owned(),
        }),
    }));
    let mut init = ExtensionDataInit::new();
    let mut tools = Vec::new();
    assert!(
        resolve_host_thread_startup(
            &builder.build(),
            Some(child_id),
            Some(ThreadId::new()),
            None,
            &mut init,
            &mut tools,
        )
        .await
        .is_err()
    );
    assert!(init.get::<ToolPolicy>().is_none());
    assert!(tools.is_empty());
}

#[tokio::test]
async fn standalone_startup_has_no_host_store_or_policy_side_effect() {
    let mut init = ExtensionDataInit::new();
    let mut tools = Vec::new();
    assert!(
        !resolve_host_thread_startup(
            &ExtensionRegistryBuilder::<Config>::new().build(),
            None,
            None,
            None,
            &mut init,
            &mut tools,
        )
        .await
        .expect("native startup")
    );
    assert!(init.get::<ToolPolicy>().is_none());
    assert!(init.get::<RequireDurableThreadStore>().is_none());
}

#[cfg(test)]
mod tool_ceiling_tests {
    use codex_extension_api::ToolName;
    use codex_extension_api::ToolPolicy;

    #[test]
    fn child_cannot_expand_an_empty_parent_ceiling() {
        let mut child = ToolPolicy::default();
        child.restrict_to(&ToolPolicy {
            allowed_tools: Some(Vec::new()),
            require_managed_sandbox: true,
            require_unified_exec: true,
            expose_additional_permissions: false,
        });
        assert_eq!(child.allowed_tools, Some(Vec::new()));
        assert!(child.require_managed_sandbox);
        assert!(child.require_unified_exec);
        assert!(!child.expose_additional_permissions);
    }

    #[test]
    fn child_ceiling_preserves_namespaces_and_narrower_requests() {
        let allowed = ToolName::namespaced("approved", "read");
        let denied = ToolName::namespaced("other", "read");
        let mut child = ToolPolicy {
            allowed_tools: Some(vec![allowed.clone(), denied]),
            ..Default::default()
        };
        child.restrict_to(&ToolPolicy {
            allowed_tools: Some(vec![allowed.clone()]),
            ..Default::default()
        });
        assert_eq!(child.allowed_tools, Some(vec![allowed]));
        child.allowed_tools = Some(Vec::new());
        child.restrict_to(&ToolPolicy::default());
        assert_eq!(child.allowed_tools, Some(Vec::new()));
    }
}
