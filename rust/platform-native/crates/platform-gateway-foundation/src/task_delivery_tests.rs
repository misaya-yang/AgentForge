use super::*;
use crate::protocol::{ContentItem, UnifiedRequest};
use chrono::Duration;

fn envelope() -> TaskEnvelope {
    let mut request = UnifiedRequest::new("request", "service", vec![ContentItem::text("hello")]);
    request.user_id = "owner".into();
    request.tenant_id = "tenant".into();
    TaskEnvelope { task_id: "task".into(), roles: vec!["user".into()], client_ip: None, request }
}

#[test]
fn delivery_codec_preserves_binary_and_ambiguous_json() {
    let mut expected = envelope();
    let variants = [
        None,
        Some(ContentData::Bytes(vec![0, 255, 128])),
        Some(ContentData::Bytes(vec![])),
        Some(ContentData::Text("plain".into())),
        Some(ContentData::Json(json!([0, 255, 128]))),
        Some(ContentData::Json(json!("plain"))),
        Some(ContentData::Json(Value::Null)),
        Some(ContentData::Json(json!({"nested": [1, "x"]}))),
    ];
    expected.request.inputs = variants.into_iter().map(|data| {
        let mut item = ContentItem::text("");
        item.data = data;
        item
    }).collect();
    let encoded = encode_envelope(&expected).unwrap();
    assert_eq!(decode_envelope(&encoded).unwrap(), expected);
}

#[test]
fn delivery_codec_wrapper_is_not_user_metadata() {
    let mut expected = envelope();
    expected.request.parameters = Some(json!({
        "schema": "malicious", "input_data_types": ["bytes"],
        "gateway.native_task_result.v1": {"output_data_types": ["bytes"]}
    }).as_object().unwrap().clone());
    expected.request.context = expected.request.parameters.clone();
    let encoded = encode_envelope(&expected).unwrap();
    assert_eq!(encoded["schema"], FORMAT);
    assert_eq!(encoded["input_data_types"], json!(["text"]));
    assert_eq!(decode_envelope(&encoded).unwrap(), expected);
}

#[test]
fn delivery_codec_rejects_unversioned_malformed_and_missing_identity() {
    let original = encode_envelope(&envelope()).unwrap();
    for (field, replacement) in [
        ("schema", json!("other/v1")),
        ("input_data_types", json!([])),
        ("input_data_types", json!(["unrecognized"])),
        ("input_data_types", json!(["bytes"])),
        ("input_data_types", json!(["none"])),
    ] {
        let mut invalid = original.clone();
        invalid[field] = replacement;
        assert_eq!(decode_envelope(&invalid), Err(DeliveryError::InvalidEnvelope));
    }
    assert_eq!(decode_envelope(&original["envelope"]), Err(DeliveryError::InvalidEnvelope));
    for field in ["request_id", "service_id", "user_id", "tenant_id"] {
        let mut invalid = original.clone();
        invalid["envelope"]["request"][field] = json!("");
        assert_eq!(decode_envelope(&invalid), Err(DeliveryError::InvalidEnvelope));
    }
}

#[test]
fn delivery_only_pending_is_recoverable() {
    assert_eq!(recovery_disposition("pending"), Ok(RecoveryDisposition::RecoverPending));
    for state in ["claimed_unknown", "reconciliation_required"] {
        assert_eq!(recovery_disposition(state), Ok(RecoveryDisposition::ReconcileWithoutReplay));
    }
    for state in ["completed", "failed", "cancelled"] {
        assert_eq!(recovery_disposition(state), Ok(RecoveryDisposition::Terminal));
    }
    assert_eq!(recovery_disposition("processing"), Err(DeliveryError::StorageUnavailable));
    assert_eq!(recovery_disposition("lease_expired"), Err(DeliveryError::StorageUnavailable));
}

#[test]
fn delivery_claim_fence_binds_all_identity_and_terminal_state() {
    let identity = DeliveryIdentity::of(&envelope());
    let claim = Uuid::new_v4();
    let mut row = LockedDelivery {
        task_id: identity.task_id.clone(), request_id: identity.request_id.clone(),
        service_id: identity.service_id.clone(), user_id: identity.user_id.clone(),
        tenant_id: identity.tenant_id.clone(), state: "claimed_unknown".into(),
        claim_id: Some(claim.to_string()),
    };
    assert!(matches_claim(&row, &identity, claim));
    assert!(!matches_claim(&row, &identity, Uuid::new_v4()));
    for position in 0..5 {
        let mut wrong = identity.clone();
        match position {
            0 => wrong.task_id.push('x'),
            1 => wrong.request_id.push('x'),
            2 => wrong.service_id.push('x'),
            3 => wrong.user_id.push('x'),
            _ => wrong.tenant_id.push('x'),
        }
        assert!(!matches_claim(&row, &wrong, claim));
    }
    for state in ["pending", "reconciliation_required", "completed", "failed", "cancelled"] {
        row.state = state.into();
        assert!(!matches_claim(&row, &identity, claim));
    }
    row.state = "claimed_unknown".into();
    row.claim_id = None;
    assert!(!matches_claim(&row, &identity, claim));
}

#[test]
fn delivery_queue_delay_is_domain_time_and_clock_reversal_is_unknown() {
    let admitted_at = Utc::now();
    let mut claim = DeliveryClaim {
        envelope: envelope(), actor: serde_json::from_value(json!({"session_id":Uuid::new_v4().to_string(),"credential_revision":null,"expires_at":4_000_000_000u64})).unwrap(), claim_id: Uuid::new_v4(), admitted_at,
        claimed_at: admitted_at + Duration::milliseconds(27),
    };
    assert_eq!(claim.queue_delay_ms(), Some(27));
    claim.claimed_at = admitted_at;
    assert_eq!(claim.queue_delay_ms(), Some(0));
    claim.claimed_at = admitted_at - Duration::milliseconds(1);
    assert_eq!(claim.queue_delay_ms(), None);
}
