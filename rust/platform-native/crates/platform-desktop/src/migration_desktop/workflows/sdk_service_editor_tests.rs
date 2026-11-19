use super::*;
use serde_json::json;
fn saved(id: &str) -> serde_json::Value {
    json!({"service_id":id,"connector_config":{},"metadata":{
    "adapter_type":"openai_agents","sdk_agent":{"model":"saved-model","instructions":"Help","max_turns":8},"sdk_provider_key_configured":true}})
}
fn draft() -> SdkServiceEditor {
    SdkServiceEditor {
        id: "sdk-test".into(),
        name: "SDK test".into(),
        adapter: "openai_agents".into(),
        creating: true,
        settings: Settings {
            model: "new-model".into(),
            instructions: "New instructions".into(),
            max_turns: 4,
        },
        key: "fixture-key-123456".into(),
        ..Default::default()
    }
}
#[test]
fn frozen_review_clears_secret_and_only_submits_once() {
    let mut editor = draft();
    editor.prepare_create().unwrap();
    assert!(editor.key.is_empty());
    assert!(editor.take_write().is_none());
    editor.confirmed = true;
    assert_eq!(editor.take_write().unwrap().segments, ["api", "v1", "service-registrations"]);
    assert!(editor.take_write().is_none());
    assert_eq!(editor.recovery_id.as_deref(), Some("sdk-test"));
}
#[test]
fn navigation_discards_review_and_secrets_but_retains_submitted_fence() {
    let mut editor = draft();
    editor.prepare_create().unwrap();
    editor.confirmed = true;
    editor.take_write().unwrap();
    editor.abandon("other-service");
    assert!(editor.review.is_none());
    assert!(editor.key.is_empty());
    assert!(editor.saved.is_none());
    assert_eq!(editor.recovery_id.as_deref(), Some("sdk-test"));
    editor.target = "other-service".into();
    assert!(editor.accept_read(&saved("other-service")).is_err());
    editor.target = "sdk-test".into();
    editor.accept_read(&saved("sdk-test")).unwrap();
    assert!(editor.recovery_id.is_none());
    assert_eq!(editor.id, "sdk-test");
}
#[test]
fn fresh_metadata_is_preserved_without_replacing_the_user_draft() {
    let mut editor = draft();
    editor.creating = false;
    editor.target = "sdk-test".into();
    editor.refresh_for_review = true;
    let mut value = saved("sdk-test");
    value["metadata"]["sdk_tools"] = json!({"capability_ids":["new-tool"]});
    editor.accept_read(&value).unwrap();
    let body = editor.review.unwrap().body.unwrap();
    assert_eq!(
        body["metadata"]["sdk_tools"]["capability_ids"],
        json!(["new-tool"])
    );
    assert_eq!(body["metadata"]["sdk_agent"]["model"], "new-model");
    assert!(editor.key.is_empty());
}
#[test]
fn leaving_section_clears_unsubmitted_configuration_and_review() {
    let mut editor = draft();
    editor.prepare_create().unwrap();
    editor.confirmed = true;
    editor.leave();
    assert!(editor.review.is_none());
    assert!(!editor.confirmed);
    assert!(editor.key.is_empty());
    assert!(editor.saved.is_none());
}
#[test]
fn framework_change_during_review_refresh_is_rejected() {
    let mut editor = draft();
    editor.target = "sdk-test".into();
    editor.refresh_for_review = true;
    let mut value = saved("sdk-test");
    value["metadata"]["adapter_type"] = json!("claude_agent_sdk");
    assert!(editor.accept_read(&value).is_err());
    assert!(editor.review.is_none());
    assert!(editor.key.is_empty());
}
#[test]
fn invalid_edit_keeps_loaded_projection_and_editable_draft() {
    let mut editor = draft();
    editor.creating = false;
    editor.target = "sdk-test".into();
    editor.refresh_for_review = true;
    editor.settings.model.clear();
    assert!(editor.accept_read(&saved("sdk-test")).is_err());
    assert!(editor.saved.is_some());
    assert!(editor.settings.model.is_empty());
    assert!(editor.review.is_none());
}
#[test]
fn only_absence_preflight_allows_new_registration_review() {
    for status in [403, 408, 500] {
        let mut editor = draft();
        editor.checking_absence = true;
        assert!(!editor.finish(JobResult {
            kind: "load_sdk_service".into(),
            result: Err(ApiError::Status(status))
        }));
        assert!(editor.review.is_none());
        assert!(editor.key.is_empty());
    }
    let mut editor = draft();
    editor.checking_absence = true;
    editor.finish(JobResult {
        kind: "load_sdk_service".into(),
        result: Err(ApiError::Status(404)),
    });
    assert!(editor.review.is_some());
    assert!(editor.recovery_id.is_none());
    let mut editor = draft();
    editor.checking_absence = true;
    editor.target = "sdk-test".into();
    editor.finish(JobResult {
        kind: "load_sdk_service".into(),
        result: Ok(saved("sdk-test")),
    });
    assert!(editor.review.is_none());
    assert!(!editor.creating);
    assert!(editor.saved.is_some());
}
#[test]
fn failed_writes_require_read_and_explicit_abandon_never_replay() {
    for status in [400, 403, 404, 405, 408, 409, 500, 501] {
        let mut editor = draft();
        editor.prepare_create().unwrap();
        editor.confirmed = true;
        editor.take_write().unwrap();
        assert!(!editor.finish(JobResult {
            kind: "save_sdk_service".into(),
            result: Err(ApiError::Status(status))
        }));
        if matches!(status, 404 | 405 | 501) { assert!(editor.job.status.contains("unavailable")); }
        if status == 409 { assert!(editor.job.status.contains("conflict")); }
        assert!(editor.take_write().is_none());
        editor.acknowledge_unresolved = true;
        editor.abandon_unresolved();
        assert!(editor.recovery_id.is_some());
        editor.finish(JobResult {
            kind: "load_sdk_service".into(),
            result: Err(ApiError::Status(404)),
        });
        editor.acknowledge_unresolved = false;
        editor.abandon_unresolved();
        assert!(editor.recovery_id.is_some());
        editor.acknowledge_unresolved = true;
        editor.abandon_unresolved();
        assert!(editor.recovery_id.is_none());
        assert_eq!(editor.unresolved, vec!["sdk-test"]);
        assert!(editor.review.is_none());
        assert!(editor.key.is_empty());
        assert!(!editor.creating);
    }
}
#[test]
fn success_acknowledgement_requires_detail_readback() {
    let mut editor = draft();
    editor.prepare_create().unwrap();
    editor.confirmed = true;
    editor.take_write().unwrap();
    assert!(editor.finish(JobResult {
        kind: "save_sdk_service".into(),
        result: Ok(json!({"service_id":"sdk-test","status":"registered"}))
    }));
    assert!(editor.saved.is_none());
    assert!(editor.recovery_id.is_some());
    editor.finish(JobResult {
        kind: "load_sdk_service".into(),
        result: Ok(saved("sdk-test")),
    });
    assert!(editor.saved.is_some());
    assert!(editor.recovery_id.is_none());
    assert!(editor.review.is_none());
}
#[test]
fn malformed_readback_does_not_unlock_unknown_write() {
    let mut editor = draft();
    editor.target = "sdk-test".into();
    editor.recovery_id = Some("sdk-test".into());
    assert!(
        editor
            .accept_read(&json!({"service_id":"sdk-test","status":"registered"}))
            .is_err()
    );
    assert_eq!(editor.recovery_id.as_deref(), Some("sdk-test"));
    assert!(editor.saved.is_none());
}
