//! Pure projection assertions from test_document_display_status.py.
use ai_platform_knowledge_service::migration_knowledge_ingest::{
    display_status, service::with_display_status,
};
use serde_json::{Value, json};
#[test]
fn internal_and_legacy_states_map_to_public_vocabulary() {
    for (status, expected) in [
        ("waiting", "queuing"),
        ("parsing", "indexing"),
        ("splitting", "indexing"),
        ("indexing", "indexing"),
        ("completed", "available"),
        ("error", "error"),
        ("paused", "paused"),
        ("syncing", "indexing"),
        ("uploading", "indexing"),
        ("uploading_images", "indexing"),
        ("failed", "error"),
    ] {
        assert_eq!(
            display_status(&json!({"status":status,"enabled":true,"archived":false})),
            expected
        );
    }
}
#[test]
fn archived_precedence_and_completed_enabled_default() {
    for status in ["error", "completed", "waiting", "indexing", "failed", ""] {
        assert_eq!(
            display_status(&json!({"status":status,"enabled":true,"archived":true})),
            "archived"
        );
    }
    for (enabled, expected) in [(true, "available"), (false, "disabled")] {
        assert_eq!(
            display_status(&json!({"status":"completed","enabled":enabled,"archived":false})),
            expected
        );
    }
    assert_eq!(
        display_status(&json!({"status":"completed","archived":false})),
        "available"
    );
}
#[test]
fn unknown_nondictionary_and_normalized_statuses_fail_closed() {
    for status in [
        "",
        "uploaded",
        "queued",
        "processing",
        "detecting",
        "segmenting",
        "embedding",
        "embedding_images",
        "associating_images",
        "something-new",
    ] {
        assert_eq!(
            display_status(&json!({"status":status,"enabled":true,"archived":false})),
            "indexing"
        );
    }
    for value in [
        json!({"status":null,"enabled":true}),
        Value::Null,
        json!("completed"),
        json!([]),
    ] {
        assert_eq!(display_status(&value), "indexing");
    }
    assert_eq!(
        display_status(&json!({"status":"  COMPLETED ","enabled":true})),
        "available"
    );
    assert_eq!(display_status(&json!({"status":" Waiting "})), "queuing");
}
#[test]
fn every_state_and_flag_combination_is_in_closed_vocabulary() {
    for status in [
        json!("waiting"),
        json!("parsing"),
        json!("splitting"),
        json!("indexing"),
        json!("completed"),
        json!("error"),
        json!("paused"),
        json!("syncing"),
        json!("uploading"),
        json!("uploading_images"),
        json!("failed"),
        json!(""),
        Value::Null,
        json!("mystery"),
    ] {
        for archived in [true, false] {
            for enabled in [true, false] {
                assert!(
                    [
                        "queuing",
                        "indexing",
                        "paused",
                        "error",
                        "available",
                        "disabled",
                        "archived"
                    ]
                    .contains(&display_status(
                        &json!({"status":status,"enabled":enabled,"archived":archived})
                    ))
                );
            }
        }
    }
}
#[test]
fn projection_preserves_identity_internal_status_and_nondictionaries() {
    let stamped =
        with_display_status(json!({"document_id":"doc-a","status":"waiting","enabled":true}));
    assert_eq!(stamped["display_status"], "queuing");
    assert_eq!(stamped["status"], "waiting");
    assert_eq!(stamped["document_id"], "doc-a");
    for value in [Value::Null, json!("x"), json!([])] {
        assert_eq!(with_display_status(value.clone()), value);
    }
}
#[test]
fn public_error_projection_redacts_secrets_and_keeps_safe_rebuild_action() {
    for archived in [false, true] {
        let stamped = with_display_status(
            json!({"document_id":"doc-safe-diagnostic","status":"error","archived":archived,"error":"provider credential private-secret and /internal/storage/path"}),
        );
        assert_eq!(
            stamped["display_status"],
            if archived { "archived" } else { "error" }
        );
        assert_eq!(stamped["document_id"], "doc-safe-diagnostic");
        let message = stamped["error"].as_str().unwrap();
        assert!(!message.contains("private-secret"));
        assert!(!message.contains("/internal/storage/path"));
        assert!(message.contains("retry manually"));
    }
    let stamped = with_display_status(
        json!({"document_id":"doc-a","status":"error","error":"specialized_rebuild_unavailable"}),
    );
    assert_eq!(stamped["failure_code"], "specialized_rebuild_unavailable");
    assert!(
        stamped["error"]
            .as_str()
            .unwrap()
            .contains("may have partial results")
    );
}
