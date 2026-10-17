use super::*;
fn call(wire: ModelWire, payload: Value) -> ModelCall {
    let mut call = crate::knowledge_model_plane::tests::call();
    call.wire = wire;
    call.payload = payload;
    call
}
fn checked(call: &ModelCall) -> PlaneResult<()> {
    // Exercise the production codec gate used by prepare(), not only the cap helper.
    super::super::validate_codec(call, &json!({"model":{"model_type":"chat"}}))
}
#[test]
fn each_generation_wire_requires_its_native_output_cap() {
    for (wire, payload) in [
        (ModelWire::ChatCompletions, json!({"max_tokens":10})),
        (
            ModelWire::ChatCompletions,
            json!({"max_completion_tokens":10}),
        ),
        (
            ModelWire::GeminiGenerateContent,
            json!({"generationConfig":{"maxOutputTokens":10}}),
        ),
        (
            ModelWire::DashscopeVision,
            json!({"parameters":{"max_tokens":10}}),
        ),
    ] {
        assert!(checked(&call(wire, payload)).is_ok());
    }
    for (wire, payload) in [
        (ModelWire::ChatCompletions, json!({"max_output_tokens":1})),
        (ModelWire::GeminiGenerateContent, json!({"max_tokens":1})),
        (ModelWire::DashscopeVision, json!({"max_tokens":1})),
    ] {
        assert_eq!(checked(&call(wire, payload)).unwrap_err().status, 422);
    }
}
#[test]
fn shadow_caps_and_conflicting_aliases_cannot_bypass_the_reservation() {
    for (wire, payload) in [
        (
            ModelWire::ChatCompletions,
            json!({"max_tokens":1,"max_completion_tokens":100}),
        ),
        (
            ModelWire::GeminiGenerateContent,
            json!({"max_tokens":1,"generationConfig":{"maxOutputTokens":100}}),
        ),
        (
            ModelWire::DashscopeVision,
            json!({"max_tokens":1,"parameters":{"max_tokens":100}}),
        ),
        (
            ModelWire::ChatCompletions,
            json!({"max_tokens":1,"max_completion_tokens":10}),
        ),
    ] {
        assert!(checked(&call(wire, payload)).is_err());
    }
}
#[test]
fn malformed_and_zero_caps_do_not_fall_through_to_another_alias() {
    for value in [Value::Null, json!("10"), json!(-1), json!(0), json!(1.5)] {
        assert!(
            checked(&call(
                ModelWire::ChatCompletions,
                json!({"max_tokens":value,"max_completion_tokens":10})
            ))
            .is_err()
        );
    }
}
#[test]
fn candidate_count_must_fit_the_total_output_reservation() {
    for (wire, payload) in [
        (ModelWire::ChatCompletions, json!({"max_tokens":10,"n":2})),
        (
            ModelWire::ChatCompletions,
            json!({"max_tokens":10,"best_of":2}),
        ),
        (
            ModelWire::GeminiGenerateContent,
            json!({"generationConfig":{"maxOutputTokens":10,"candidateCount":2}}),
        ),
        (
            ModelWire::DashscopeVision,
            json!({"parameters":{"max_tokens":10,"n":2}}),
        ),
    ] {
        let mut c = call(wire, payload);
        assert!(checked(&c).is_err());
        c.budget.max_output_tokens = 20;
        assert!(checked(&c).is_ok());
    }
    for count in [
        Value::Null,
        json!(0),
        json!(-1),
        json!("2"),
        json!(u64::MAX),
    ] {
        assert!(
            checked(&call(
                ModelWire::ChatCompletions,
                json!({"max_tokens":10,"n":count})
            ))
            .is_err()
        );
    }
}
