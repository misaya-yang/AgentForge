//! Synthetic DTO tests only. These do not claim real PG/source-role acceptance.
use super::*;
fn binding() -> ModelCallBinding {
    ModelCallBinding {
        tenant: "tenant".into(),
        user: "user".into(),
        reservation_id: "00000000-0000-4000-8000-000000000001".into(),
        provider: "provider".into(),
        model: "model".into(),
    }
}
fn runtime() -> Value {
    json!({"remaining_ms":10000,"quota":null,
    "call":{"tenant_id":"tenant","user_id":"user","call_id":binding().reservation_id,
        "estimated_input_tokens":30,"reserved_output_tokens":40,"reserved_cost_microusd":20,
        "lease_id":"lease","request_sha256":"hash","snapshot_id":"snapshot"},
    "lease":{"provider_id":"provider","model_id":"model","used_input_tokens":50,"used_output_tokens":50,
        "reserved_input_tokens":70,"reserved_output_tokens":80,"max_input_tokens":100,"max_output_tokens":100,
        "reserved_cost_microusd":80,"used_cost_microusd":70,"max_cost_microusd":100}})
}
fn knowledge() -> Value {
    json!({"remaining_ms":10000,
    "call":{"tenant_id":"tenant","user_id":"user","lease_id":binding().reservation_id,
        "call":{"model":{"provider":"provider","model":"model"}},"reserved_input":0,"reserved_output":0,
        "pricing":{"pricing_meter":"requests"},"reserved_cost_nanos":"100"},
    "quota":{"current_daily_tokens":50,"current_monthly_tokens":100,"daily_token_limit":100,"monthly_token_limit":200,
        "temporary_extra_tokens":0,"temporary_extra_cost_cents":0,"overage_strategy":"hard_block","is_blocked":false}})
}
#[test]
fn runtime_lifetime_reservation_is_not_added_to_used_or_reserved_twice() {
    let proof = proof_from_row(Owner::Assistant, binding(), runtime()).unwrap();
    assert_eq!(proof.envelope.estimate, 0);
    assert_eq!(proof.envelope.daily_used, 0);
    assert_eq!(proof.envelope.pending, 150);
    assert_eq!(proof.envelope.daily_limit, Some(200));
    assert_eq!(
        proof.units,
        ModelTokenReservation::Tokens {
            input: 30,
            output: 40
        }
    );
}
#[test]
fn stored_scope_budget_and_expiry_cannot_be_bypassed_by_claimed_binding() {
    for (pointer, value) in [
        ("/call/tenant_id", json!("foreign")),
        ("/lease/provider_id", json!("foreign")),
        ("/lease/reserved_input_tokens", json!(101)),
        ("/lease/reserved_cost_microusd", json!(101)),
        ("/remaining_ms", json!(0)),
    ] {
        let mut row = runtime();
        *row.pointer_mut(pointer).unwrap() = value;
        assert!(proof_from_row(Owner::Assistant, binding(), row).is_err());
    }
}
#[test]
fn knowledge_request_meter_remains_not_applicable_and_precharge_is_not_repeated() {
    let proof = proof_from_row(Owner::Knowledge, binding(), knowledge()).unwrap();
    assert_eq!(proof.units, ModelTokenReservation::NotApplicable);
    assert_eq!(proof.envelope.estimate, 0);
    assert_eq!(proof.envelope.pending, 0);
    assert_eq!(proof.envelope.daily_used, 50);
    assert_eq!(proof.envelope.monthly_used, 100);
    let mut invalid = knowledge();
    invalid["call"]["reserved_input"] = json!(1);
    assert!(proof_from_row(Owner::Knowledge, binding(), invalid).is_err());
}
