//! Pure source assertions from test_assistant_model_access_levels.py.
//! Full edge auth/model resolution requires the AgentState integration fixture.
use ai_platform_gateway::migration_agent::{
    Actor, entry::user_can_access_model, entry_request::normalize_v1,
};
use serde_json::json;
#[test]
fn assistant_request_rejects_unbounded_or_multimodal_knowledge_scope() {
    for payload in [
        json!({"message":"hello","kb_dataset_ids":(0..9).map(|i|format!("dataset-{i}")).collect::<Vec<_>>()}),
        json!({"message":"hello","kb_dataset_ids":["dataset-a","dataset-a"]}),
        json!({"message":"hello","kb_dataset_ids":["x".repeat(129)]}),
        json!({"message":"hello","kb_include_images":true}),
    ] {
        assert_eq!(normalize_v1(&payload).unwrap_err().status, 422);
    }
}
#[test]
fn assistant_request_rejects_unresumable_plan_confirmation() {
    assert_eq!(
        normalize_v1(&json!({"message":"hello","confirm_plan":true}))
            .unwrap_err()
            .status,
        422
    );
}
#[test]
fn assistant_request_requires_local_node_device_and_grant_selection() {
    let enabled = normalize_v1(&json!({"message":"hello","os_agent_enabled":true})).unwrap();
    assert!(enabled["local_node_device_id"].is_null());
    assert_eq!(
        normalize_v1(&json!({"message":"hello","local_node_device_id":"device-a"}))
            .unwrap_err()
            .status,
        422
    );
    let selected=normalize_v1(&json!({"message":"analyze authorized files","os_agent_enabled":true,"local_node_device_id":"device-a","local_node_grant_ids":["grant-a"]})).unwrap();
    assert_eq!(selected["local_node_device_id"], "device-a");
    assert_eq!(selected["local_node_grant_ids"], json!(["grant-a"]));
}
#[test]
fn user_model_access_preserves_known_access_rules_and_rejects_unknown_for_every_tier() {
    for (tier, roles, access, expected) in [
        ("normal", vec![], "public", true),
        ("normal", vec![], "premium", false),
        ("premium", vec![], "premium", true),
        ("normal", vec![], "admin", false),
        ("admin", vec![], "admin", true),
        ("normal", vec!["admin".into()], "admin", true),
        ("normal", vec![], "corrupt", false),
        ("admin", vec![], "corrupt", false),
    ] {
        let actor = Actor {
            tenant_id: "tenant-1".into(),
            user_id: "user-1".into(),
            tier: tier.into(),
            roles,
        };
        assert_eq!(
            user_can_access_model(&actor, access),
            expected,
            "{tier} {access}"
        );
    }
}
