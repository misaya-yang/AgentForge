//! Exact CHR-03 contributor gate, exercising the production native bridge.
use ai_platform_gateway::migration_agent::readonly_capabilities::*;
use serde_json::json;
use std::sync::Arc;
struct Context;
impl ContextContributor for Context {
    fn contribute<'a>(
        &'a self,
        s: &'a RuntimeCapabilityScope,
    ) -> Contribution<'a, Vec<CapabilityItem>> {
        Box::pin(async move {
            Ok(vec![CapabilityItem {
                item_id: "context:agent-spec".into(),
                kind: "context".into(),
                tenant_id: s.tenant_id.clone(),
                capability_revision: s.capability_revision,
                source: "agent-spec".into(),
                payload: json!({"schema_version":"agent-spec/v1"}),
                untrusted: true,
                authority: "non_authoritative".into(),
            }])
        })
    }
}
struct Input;
impl TurnInputContributor for Input {
    fn contribute<'a>(
        &'a self,
        s: &'a RuntimeCapabilityScope,
        t: &'a TurnInput,
    ) -> Contribution<'a, Vec<CapabilityItem>> {
        Box::pin(async move {
            Ok(vec![CapabilityItem {
                item_id: format!("turn-input:{}", t.turn_id),
                kind: "turn_input".into(),
                tenant_id: s.tenant_id.clone(),
                capability_revision: s.capability_revision,
                source: "turn-input".into(),
                payload: json!({"attachment_ids":t.attachment_ids,"knowledge_refs":t.knowledge_refs}),
                untrusted: true,
                authority: "non_authoritative".into(),
            }])
        })
    }
}
fn descriptor(
    s: &RuntimeCapabilityScope,
    name: &str,
    source: &str,
    kind: &str,
    tags: &[&str],
    protocol: &str,
    category: &str,
) -> CapabilityDescriptor {
    CapabilityDescriptor {
        name: name.into(),
        description: "Read authorized content.".into(),
        schema: json!({"type":"object","properties":{}}),
        tenant_id: s.tenant_id.clone(),
        capability_revision: s.capability_revision,
        source: source.into(),
        kind: kind.into(),
        read_only: true,
        tags: tags.iter().map(|s| s.to_string()).collect(),
        protocol: protocol.into(),
        category: category.into(),
        metadata: json!({}),
    }
}
struct Tools;
impl ToolContributor for Tools {
    fn describe<'a>(
        &'a self,
        s: &'a RuntimeCapabilityScope,
    ) -> Contribution<'a, Vec<CapabilityDescriptor>> {
        Box::pin(async move {
            Ok(vec![
                descriptor(
                    s,
                    "knowledge.read",
                    "knowledge",
                    "knowledge",
                    &["read", "retrieval"],
                    "internal",
                    "retrieval",
                ),
                descriptor(
                    s,
                    "office.read",
                    "office",
                    "office_read",
                    &["read", "artifact"],
                    "internal",
                    "retrieval",
                ),
            ])
        })
    }
}
struct Mcp;
impl McpServerContributor for Mcp {
    fn discover<'a>(
        &'a self,
        s: &'a RuntimeCapabilityScope,
    ) -> Contribution<'a, Vec<CapabilityDescriptor>> {
        Box::pin(async move {
            Ok(vec![descriptor(
                s,
                "docs.read",
                "mcp:docs",
                "mcp",
                &["read", "documentation"],
                "mcp",
                "general",
            )])
        })
    }
}
#[tokio::test]
async fn readonly_contributor_gate() {
    let scope = RuntimeCapabilityScope {
        tenant_id: "tenant-gate".into(),
        user_id: "user-gate".into(),
        session_id: "session-gate".into(),
        capability_revision: 11,
        snapshot_id: "snapshot-gate".into(),
    };
    let bridge = ReadonlyCapabilityBridge::new(
        scope.clone(),
        Contributors {
            context: vec![Arc::new(Context)],
            turn_input: vec![Arc::new(Input)],
            tools: vec![Arc::new(Tools)],
            mcp: vec![Arc::new(Mcp)],
            items: vec![],
        },
    )
    .unwrap();
    let context = bridge.collect_context(None).await.unwrap();
    let input = bridge
        .collect_turn_input(
            &TurnInput {
                turn_id: "turn-gate".into(),
                attachment_ids: vec!["attachment-1".into()],
                knowledge_refs: vec!["source-1".into()],
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    let tools = bridge
        .discover_tools(Some(&json!({"tags":["read"]})), None)
        .await
        .unwrap();
    let meta = json!({});
    let items = bridge
        .project_items(
            &[
                bridge
                    .knowledge("source-1", "untrusted retrieval", &meta)
                    .unwrap(),
                bridge.attachment("attachment-1", "blob-1", &meta).unwrap(),
                bridge
                    .citation("citation-1", "source-1", "p.1", &meta)
                    .unwrap(),
                bridge
                    .artifact("artifact-1", "blob-2", "text/plain", &meta)
                    .unwrap(),
                bridge
                    .office_read("artifact-1", "docx", json!({"paragraphs":1}), &meta)
                    .unwrap(),
            ],
            None,
        )
        .await
        .unwrap();
    assert_eq!(context.len(), 1);
    assert_eq!(input[0].payload["knowledge_refs"], json!(["source-1"]));
    assert_eq!(
        tools.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        vec!["knowledge.read", "docs.read", "office.read"]
    );
    assert_eq!(
        items
            .iter()
            .map(|i| i.kind.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "knowledge",
            "attachment",
            "citation",
            "artifact",
            "office_read"
        ]
        .into_iter()
        .collect()
    );
    assert!(
        items.iter().all(|i| i.tenant_id == scope.tenant_id
            && i.capability_revision == scope.capability_revision)
    );
}
