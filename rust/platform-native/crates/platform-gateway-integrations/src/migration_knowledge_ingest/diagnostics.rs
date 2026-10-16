//! Safe, advisory lifecycle guidance derived only from durable document state.
//! This never authorizes a write or asserts that an old index is still active.
use super::string;
use serde_json::{Value, json};

pub(super) const SAFE_PROCESSING_ERROR: &str =
    "Document processing failed. Check the source file and retry manually.";

pub(super) fn processing_diagnostics(document: &Value) -> Value {
    let status = string(document, "status").trim().to_lowercase();
    let metadata = &document["metadata"];
    // Report evidence, not a guessed failure cause. Timestamps are reset when
    // a new generation is queued; embedding currently shares the indexing stage.
    let last_stage = match status.as_str() {
        "uploading" => Some("uploading"),
        "waiting" => Some("waiting"),
        "parsing" => Some("parsing"),
        "splitting" => Some("splitting"),
        "indexing" => Some("indexing"),
        "completed" => Some("completed"),
        _ => [
            ("indexing_started_at", "indexing"),
            ("splitting_started_at", "splitting"),
            ("parsing_started_at", "parsing"),
        ]
        .into_iter()
        .filter_map(|(field, stage)| {
            document[field]
                .as_str()
                .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp.trim()).ok())
                .map(|stamp| (stamp, stage))
        })
        // Legacy rows can retain timestamps from an earlier attempt. Prefer
        // actual chronology, not the nominal pipeline order; invalid stamps
        // are not evidence. Offset-aware parsing compares the same instant.
        .max_by_key(|(stamp, _)| *stamp)
        .map(|(_, stage)| stage),
    };
    let (next_action, blocked_reason) = if document["archived"] == true {
        ("restore_document", Some("archived"))
    } else if document["enabled"] == false {
        ("enable_document", Some("disabled"))
    } else if metadata.get("_document_upload_failed").is_some() {
        ("replace_source", Some("upload_failed"))
    } else if metadata.get("_document_upload_generation").is_some() {
        ("inspect_upload", Some("upload_incomplete"))
    } else if metadata.get("_confluence_sync_generation").is_some() {
        ("inspect_source_sync", Some("source_sync_pending"))
    } else if metadata.get("_document_lifecycle_reindex").is_some() {
        ("inspect_lifecycle", Some("lifecycle_transition"))
    } else if metadata.get("_special_publication_generation_id").is_some() {
        ("inspect_execution", Some("publication_pending"))
    } else {
        match status.as_str() {
            "error" => ("retry_document", None),
            "completed" => ("none", None),
            "waiting" | "parsing" | "splitting" | "indexing" => ("wait", Some("processing")),
            // Unknown/legacy states must not promise an enqueue the store rejects.
            _ => ("inspect_document", Some("unsupported_state")),
        }
    };
    json!({
        "last_recorded_stage": last_stage,
        "next_action": next_action,
        "retry_blocked_reason": blocked_reason,
        "retry_requires_editor": true
    })
}

pub(super) fn sanitize_processing_error(value: &mut Value) {
    const SPECIALIZED_REBUILD_UNAVAILABLE: &str = "specialized_rebuild_unavailable";
    if value.get("error").and_then(Value::as_str) == Some(SPECIALIZED_REBUILD_UNAVAILABLE) {
        value["failure_code"] = json!(SPECIALIZED_REBUILD_UNAVAILABLE);
    }
    if let Some(error) = value.get("error") {
        let populated = match error {
            Value::Null => false,
            Value::Bool(value) => *value,
            Value::String(value) => !value.is_empty(),
            Value::Number(value) => value.as_f64() != Some(0.0),
            Value::Array(value) => !value.is_empty(),
            Value::Object(value) => !value.is_empty(),
        };
        value["error"] = if populated {
            if value.get("failure_code").and_then(Value::as_str)
                == Some(SPECIALIZED_REBUILD_UNAVAILABLE)
            {
                json!(
                    "This processing path may have partial results and cannot be replayed safely. Ask an administrator to inspect it before retrying."
                )
            } else {
                json!(SAFE_PROCESSING_ERROR)
            }
        } else {
            Value::Null
        };
    }
}
