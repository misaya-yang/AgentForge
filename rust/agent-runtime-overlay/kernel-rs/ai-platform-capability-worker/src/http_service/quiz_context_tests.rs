//! Exercise the real Runtime-binding serialization → durable dispatch → plugin context seam.
use super::*;
use crate::{
    RuntimeCapabilityBinding,
    quiz_capabilities::{QuizExecutionContext, QuizPersistenceError, QuizPluginBinding},
};
use ai_platform_capability_contract::{ApprovalPolicy, CapabilityEffect};
use serde_json::json;
use std::collections::BTreeSet;

fn fixture() -> (WriteCapabilityContext, Value, QuizPluginBinding) {
    let descriptor = crate::platform_catalog::worker_capability_catalog_with_writes(true).unwrap()
        ["generate_quiz"]
        .clone();
    let scope = CapabilityScopeV2 {
        tenant_id: "tenant-a".into(),
        user_id: "user-a".into(),
        session_id: "session-a".into(),
    };
    let binding = RuntimeCapabilityBinding {
        capability_type: "tool".into(),
        name: "generate_quiz".into(),
        snapshot_id: Uuid::now_v7().to_string(),
        capability_revision: 9,
        capability_id: descriptor.id.clone(),
        capability_version: descriptor.version.clone(),
        schema_hash: descriptor.schema_hash.clone(),
        effect: CapabilityEffect::Write,
        approval_policy: ApprovalPolicy::OnRequest,
        bound_dataset_ids: BTreeSet::from(["granted-a".into(), "granted-b".into()]),
        connector_binding: None,
        memory_policy: None,
    };
    let write = WriteCapabilityContext {
        tenant_id: scope.tenant_id.clone(),
        user_id: scope.user_id.clone(),
        session_id: scope.session_id.clone(),
        execution_id: Uuid::now_v7().to_string(),
        run_id: Uuid::now_v7().to_string(),
        capability_revision: binding.capability_revision,
        bound_dataset_ids: binding.bound_dataset_ids.clone(),
        memory_policy: None,
    };
    let resource =
        super::super::resource_binding_value(&binding, &scope, &write.run_id, &descriptor);
    let plugin = QuizPluginBinding {
        activation_id: Uuid::now_v7().to_string(),
        artifact_sha256: format!("sha256:{}", "a".repeat(64)),
        publisher_id: "reviewed-publisher".into(),
    };
    (write, resource, plugin)
}
#[test]
fn actual_runtime_grants_survive_persisted_binding_and_worker_plugin_dispatch() {
    let (write, resource, plugin) = fixture();
    let persisted: Value = serde_json::from_slice(&serde_json::to_vec(&resource).unwrap()).unwrap();
    let context = quiz_execution_context(&write, &persisted, plugin).unwrap();
    assert_eq!(
        context.bound_dataset_ids,
        BTreeSet::from(["granted-a".into(), "granted-b".into()])
    );
    assert_eq!(context.tenant_id, write.tenant_id);
    let wire = serde_json::to_value(&context).unwrap();
    assert_eq!(
        serde_json::from_value::<QuizExecutionContext>(wire).unwrap(),
        context
    );
}
#[test]
fn dispatch_does_not_accept_truncated_missing_foreign_or_wildcard_dataset_grants() {
    let (write, resource, plugin) = fixture();
    for grant in [
        json!([]),
        json!(["granted-a"]),
        json!(["granted-a", "granted-b", "foreign"]),
        json!(["granted-a", "granted-b", 42]),
        json!(["granted-a", "granted-b", "granted-a"]),
        json!(["*"]),
        json!(null),
    ] {
        let mut corrupt = resource.clone();
        corrupt["bound_dataset_ids"] = grant;
        assert_eq!(
            quiz_execution_context(&write, &corrupt, plugin.clone()),
            Err(QuizPersistenceError::Scope)
        );
    }
    let mut missing = resource.clone();
    missing.as_object_mut().unwrap().remove("bound_dataset_ids");
    assert_eq!(
        quiz_execution_context(&write, &missing, plugin.clone()),
        Err(QuizPersistenceError::Scope)
    );
    let mut foreign = resource.clone();
    foreign["tenant_id"] = json!("tenant-b");
    assert_eq!(
        quiz_execution_context(&write, &foreign, plugin.clone()),
        Err(QuizPersistenceError::Scope)
    );
    let mut zeroed = write.clone();
    zeroed.bound_dataset_ids.clear();
    assert_eq!(
        quiz_execution_context(&zeroed, &resource, plugin),
        Err(QuizPersistenceError::Scope)
    );
}
