use super::*;

fn required(effect: &str) -> Value {
    serde_json::to_value(AssistantTurnEventV1::new("approval_required", json!({
        "approval_id":Uuid::from_u128(1), "run_id":Uuid::from_u128(2),
        "thread_id":Uuid::from_u128(3), "session_id":"session-a",
        "tool_call_id":"call-a", "tool_name":"dynamic_alias",
        "effect":effect,
        "arguments_hash":ai_platform_capability_contract::canonical_json_hash(&json!({"value":1})).unwrap(),
    }))).unwrap()
}

fn validate(value: &Value, tool: &str, arguments: Value) -> ThreadStoreResult<()> {
    validate_required(
        value,
        Uuid::from_u128(1),
        Uuid::from_u128(2),
        Uuid::from_u128(3),
        "session-a",
        "call-a",
        tool,
        &arguments,
    )
}

fn decision(approved: bool, reason: Option<&str>) -> AssistantTurnEventV1 {
    decision_event(
        Uuid::from_u128(1),
        Uuid::from_u128(2),
        Uuid::from_u128(3),
        "session-a",
        "call-a",
        approved,
        reason,
    )
}

#[test]
fn dynamic_required_preserves_write_and_unknown_binding() {
    for effect in ["write", "unknown"] {
        assert!(validate(&required(effect), "capability-id", json!({"value":1})).is_ok());
    }
}

#[test]
fn native_or_unclassified_required_never_becomes_dynamic() {
    let original = required("write");
    for tool in ["command_execution", "file_change"] {
        assert!(validate(&original, tool, json!({"value":1})).is_err());
    }
    for effect in ["read", "", "Write"] {
        assert!(validate(&required(effect), "capability-id", json!({"value":1})).is_err());
    }
    for field in ["effect", "arguments_hash"] {
        let mut missing = original.clone();
        missing["data"].as_object_mut().unwrap().remove(field);
        assert!(validate(&missing, "capability-id", json!({"value":1})).is_err());
    }
}

#[test]
fn dynamic_required_rejects_every_changed_original_identity() {
    let original = required("write");
    for field in [
        "approval_id",
        "run_id",
        "thread_id",
        "session_id",
        "tool_call_id",
        "arguments_hash",
    ] {
        let mut changed = original.clone();
        changed["data"][field] = json!("different");
        assert!(
            validate(&changed, "capability-id", json!({"value":1})).is_err(),
            "{field}"
        );
    }
    let mut changed = original;
    changed["event_type"] = json!("approval_result");
    assert!(validate(&changed, "capability-id", json!({"value":1})).is_err());
}

#[test]
fn dynamic_required_binds_canonical_arguments_without_renewing_identity() {
    let mut value = required("write");
    let arguments = json!({"z":2,"a":{"b":1}});
    value["data"]["arguments_hash"] =
        json!(ai_platform_capability_contract::canonical_json_hash(&arguments).unwrap());
    assert!(validate(&value, "capability-id", json!({"a":{"b":1},"z":2})).is_ok());
    assert!(validate(&value, "capability-id", json!({"a":{"b":2},"z":2})).is_err());
    for invalid in [Value::Null, json!([]), json!("arguments")] {
        assert!(validate(&value, "capability-id", invalid).is_err());
    }
}

#[test]
fn identical_decision_retry_ignores_only_timestamp() {
    for approved in [true, false] {
        for reason in [None, Some(""), Some("original reason")] {
            let original = decision(approved, reason);
            let mut retry = original.clone();
            retry.timestamp += 300.0;
            assert!(same_decision(&original, &retry));
        }
    }
}

#[test]
fn decision_retry_rejects_opposite_or_changed_reason() {
    assert!(!same_decision(
        &decision(true, None),
        &decision(false, None)
    ));
    assert!(!same_decision(
        &decision(true, None),
        &decision(true, Some(""))
    ));
    assert!(!same_decision(
        &decision(false, Some("original")),
        &decision(false, Some("changed"))
    ));
}

#[test]
fn decision_retry_rejects_changed_scope_call_or_contract() {
    let original = decision(true, Some("original"));
    for field in [
        "approval_id",
        "run_id",
        "thread_id",
        "session_id",
        "tool_call_id",
        "status",
        "approved",
    ] {
        let mut changed = original.clone();
        changed.data[field] = json!("different");
        assert!(!same_decision(&original, &changed), "{field}");
    }
    let mut changed = original.clone();
    changed.schema_version = "different".to_string();
    assert!(!same_decision(&original, &changed));
    changed = original.clone();
    changed.event_type = "different".to_string();
    assert!(!same_decision(&original, &changed));
}

#[test]
fn decision_envelope_preserves_public_result_shape() {
    for approved in [true, false] {
        let event = decision(approved, None);
        assert_eq!(event.event_type, "approval_result");
        assert_eq!(event.data["approved"], approved);
        assert_eq!(
            event.data["status"],
            if approved { "approved" } else { "rejected" }
        );
        assert!(event.data["reason"].is_null());
        assert_eq!(event.data.as_object().unwrap().len(), 8);
    }
}

#[test]
fn result_identity_is_stable_and_separate_from_required_receipt() {
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    assert_ne!(decision_event_id(first), first);
    assert_ne!(decision_event_id(second), second);
    assert_ne!(decision_event_id(first), decision_event_id(second));
    assert_eq!(decision_event_id(first), decision_event_id(first));
    let digest = Sha256::digest(format!("compat/approval/{first}/result").as_bytes());
    assert_eq!(decision_event_id(first).as_bytes(), &digest[..16]);
}

#[test]
fn receipt_only_missing_never_admits_a_new_decision() {
    let proposed = decision(true, None);
    let id = decision_event_id(Uuid::from_u128(1));
    assert!(resolve_decision_receipt(true, None, id, &proposed).is_err());
    assert!(
        resolve_decision_receipt(false, None, id, &proposed)
            .unwrap()
            .is_none()
    );
}

#[test]
fn receipt_only_returns_the_original_envelope_and_cursor() {
    let original = decision(true, Some("original"));
    let timestamp = original.timestamp;
    let mut proposed = original.clone();
    proposed.timestamp += 100.0;
    let id = decision_event_id(Uuid::from_u128(1));
    let result = resolve_decision_receipt(
        true,
        Some((
            id,
            SequencedAssistantTurnEventV1 {
                sequence: 42,
                event: original,
            },
        )),
        id,
        &proposed,
    )
    .unwrap()
    .unwrap();
    assert_eq!(result.sequence, 42);
    assert_eq!(result.event.timestamp, timestamp);
    assert!(same_decision(&result.event, &proposed));
}

#[test]
fn receipt_only_conflicts_never_admit_a_new_decision() {
    let original = decision(true, None);
    let id = decision_event_id(Uuid::from_u128(1));
    for proposed in [
        decision(false, None),
        decision(true, Some("")),
        decision(true, Some("changed")),
    ] {
        assert!(
            resolve_decision_receipt(
                true,
                Some((
                    id,
                    SequencedAssistantTurnEventV1 {
                        sequence: 42,
                        event: original.clone()
                    }
                )),
                id,
                &proposed
            )
            .is_err()
        );
    }
    assert!(
        resolve_decision_receipt(
            true,
            Some((
                Uuid::from_u128(99),
                SequencedAssistantTurnEventV1 {
                    sequence: 42,
                    event: original.clone()
                }
            )),
            id,
            &original
        )
        .is_err()
    );
    let mut changed = original.clone();
    changed.data["run_id"] = json!(Uuid::from_u128(99));
    assert!(
        resolve_decision_receipt(
            true,
            Some((
                id,
                SequencedAssistantTurnEventV1 {
                    sequence: 42,
                    event: changed
                }
            )),
            id,
            &original
        )
        .is_err()
    );
}

#[test]
fn receipt_only_binds_the_public_route_thread() {
    let root = Uuid::from_u128(3);
    assert!(validate_receipt_thread(Some(root), root).is_ok());
    assert!(validate_receipt_thread(Some(Uuid::from_u128(4)), root).is_err());
    assert!(validate_receipt_thread(None, root).is_ok());
}
