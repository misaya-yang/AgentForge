use super::*;
use codex_protocol::ThreadId;
use serde_json::json;

fn fixture(status: &str) -> (ExecutionClaim, AssistantTurnEventV1) {
    let claim = ExecutionClaim {
        run_id: Uuid::from_u128(2), thread_id: ThreadId::from_u128(1),
        owner_id: Uuid::from_u128(3), fence: 1, context: json!({}),
    };
    let event = AssistantTurnEventV1::new(
        if status == "succeeded" { "run_finished" } else { "run_error" },
        json!({"run_id":claim.run_id,"thread_id":claim.thread_id,
               "session_id":"fixture","status":status,"error_code":null}),
    );
    (claim, event)
}

#[test]
fn recovery_terminal_accepts_only_canonical_matching_envelopes() {
    for status in ["succeeded", "failed", "cancelled"] {
        let (claim, event) = fixture(status);
        assert_eq!(validate_terminal(&claim, &event, RecoveryTerminalMode::Owned).unwrap(), (status, None));
    }
    for status in ["completed", "running", "side_effect_unknown"] {
        let (claim, event) = fixture(status);
        assert!(validate_terminal(&claim, &event, RecoveryTerminalMode::Owned).is_err());
    }
}

#[test]
fn recovery_terminal_rejects_mismatched_original_identity_and_event_kind() {
    let (claim, original) = fixture("succeeded");
    for field in ["run_id", "thread_id"] {
        let mut event = original.clone();
        event.data[field] = json!(Uuid::from_u128(4));
        assert!(validate_terminal(&claim, &event, RecoveryTerminalMode::Owned).is_err());
    }
    let mut event = original.clone();
    event.event_type = "run_error".to_string();
    assert!(validate_terminal(&claim, &event, RecoveryTerminalMode::Owned).is_err());
    event = original;
    event.data["error_code"] = json!({"invalid":"error"});
    assert!(validate_terminal(&claim, &event, RecoveryTerminalMode::Owned).is_err());
}

#[test]
fn expired_authority_cannot_create_success_or_unrelated_failure() {
    for status in ["succeeded", "failed", "cancelled"] {
        let (claim, mut event) = fixture(status);
        assert!(validate_terminal(&claim, &event, RecoveryTerminalMode::ExpiredAuthority).is_err());
        event.data["error_code"] = json!("RUNTIME_RECOVERY_AUTHORITY_EXPIRED");
        assert_eq!(validate_terminal(&claim, &event, RecoveryTerminalMode::ExpiredAuthority).is_ok(), status == "failed");
    }
}
