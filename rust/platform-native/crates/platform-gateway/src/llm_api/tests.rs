use super::*;
use std::collections::BTreeMap;
pub(super) fn actor() -> Actor {
    Actor {
        tenant: "t".into(),
        user: "u".into(),
        roles: vec!["user".into()],
        tier: "normal".into(),
        allowed_models: vec![],
        constraints: Default::default(),
        routing: None,
    }
}
pub(super) fn model(surface: Surface) -> Value {
    let mut profile = crate::capabilities::safe(true, false);
    profile["wire_protocols"] = json!({"preferred":surface.wire(),"supported":[surface.wire()]});
    json!({"tenant_id":"t","provider_id":"p","model_id":"m","is_enabled":true,"access_level":"public","model_type":if surface==Surface::Embeddings{"embedding"}else{"chat"},"max_output_tokens":100,"catalog_capabilities":profile,"supports_tools":true,"supports_vision":false})
}
fn provider(surface: Surface) -> Value {
    json!({"api_type":if surface==Surface::Messages{"anthropic"}else{"openai"}})
}
#[test]
fn native_protocol_profiles_extend_existing_wire_values_and_fail_closed() {
    for surface in [Surface::Chat, Surface::Embeddings, Surface::Messages] {
        assert!(
            crate::capabilities::validate(model(surface)["catalog_capabilities"].clone()).is_ok()
        );
    }
    let mut old = crate::capabilities::safe(false, false);
    old["wire_protocols"] =
        json!({"preferred":"responses_v1","supported":["chat_completions","responses_v1"]});
    assert!(crate::capabilities::validate(old.clone()).is_ok());
    old["wire_protocols"]["supported"] = json!(["guess_protocol"]);
    assert!(crate::capabilities::validate(old).is_err());
    let call = codec::parse(
        Surface::Messages,
        Bytes::from_static(br#"{"model":"m","messages":[],"max_tokens":10}"#),
    )
    .unwrap();
    assert!(codec::bind(&call, &model(Surface::Messages), &provider(Surface::Chat)).is_err());
    assert!(codec::bind(&call, &model(Surface::Chat), &provider(Surface::Messages)).is_err());
}
#[test]
fn exact_same_protocol_bytes_and_unknown_cache_reasoning_fields_survive() {
    for (surface, body) in [
        (
            Surface::Chat,
            "{ \"model\":\"m\",\"max_tokens\":10,\"messages\":[], \"vendor_future\":{\"z\":1e-2,\"cache_control\":{\"type\":\"ephemeral\"}},\"reasoning_effort\":\"high\" }",
        ),
        (
            Surface::Messages,
            "{\"model\":\"m\",\"messages\":[], \"max_tokens\":10,\"system\":[{\"type\":\"text\",\"text\":\"x\",\"cache_control\":{\"type\":\"ephemeral\"}}],\"future\":true}",
        ),
        (
            Surface::Embeddings,
            "{\"model\":\"m\", \"input\":[1,2,3],\"encoding_format\":\"base64\",\"vendor_future\":1e4}",
        ),
    ] {
        let call = codec::parse(surface, Bytes::from(body)).unwrap();
        let (out, _, _) = codec::bind(&call, &model(surface), &provider(surface)).unwrap();
        assert_eq!(out.as_ref(), body.as_bytes());
    }
}
#[test]
fn authorized_model_and_stream_usage_mutations_patch_only_top_level_values() {
    let body = Bytes::from_static(
        b"{\"future\":1e-3, \"model\" : \"p/m\",\"max_tokens\":10,\"messages\":[]}",
    );
    let call = codec::parse(Surface::Chat, body).unwrap();
    let (out, _, _) = codec::bind(&call, &model(Surface::Chat), &provider(Surface::Chat)).unwrap();
    assert_eq!(
        out.as_ref(),
        b"{\"future\":1e-3, \"model\" : \"m\",\"max_tokens\":10,\"messages\":[]}"
    );
    let raw =
        Bytes::from_static(br#"{"x":"escaped \" , }","nested":{"model":"leave me"},"model":"m"}"#);
    let parsed = raw_json::RawObject::parse(&raw).unwrap();
    let out = parsed
        .patch(&raw, &BTreeMap::from([("model".into(), json!("n"))]))
        .unwrap();
    assert_eq!(
        out.as_ref(),
        br#"{"x":"escaped \" , }","nested":{"model":"leave me"},"model":"n"}"#
    );
    for body in [
        br#"{"model":"a","model":"b"}"#.as_slice(),
        br#"{"mo\u0064el":"a","model":"b"}"#,
    ] {
        assert!(raw_json::RawObject::parse(body).is_err());
    }
}
#[test]
fn cross_tenant_ambiguity_access_and_key_restrictions_never_select_a_route() {
    let mut foreign = model(Surface::Chat);
    foreign["tenant_id"] = json!("foreign");
    assert!(codec::select_model(&actor(), "m", vec![foreign.clone()]).is_err());
    assert_eq!(
        codec::select_model(&actor(), "m", vec![foreign, model(Surface::Chat)]).unwrap()["tenant_id"],
        "t"
    );
    let mut other = model(Surface::Chat);
    other["provider_id"] = json!("other");
    assert!(codec::select_model(&actor(), "m", vec![other, model(Surface::Chat)]).is_err());
    let mut restricted = actor();
    restricted.allowed_models = vec!["other/*".into()];
    assert!(codec::select_model(&restricted, "p/m", vec![model(Surface::Chat)]).is_err());
    let mut admin = model(Surface::Chat);
    admin["access_level"] = json!("admin");
    assert!(codec::select_model(&actor(), "m", vec![admin]).is_err());
}
#[test]
fn unsupported_tools_modalities_and_ambiguous_output_caps_fail_before_dispatch() {
    for payload in [
        json!({"model":"m","messages":[],"max_tokens":10,"max_completion_tokens":20}),
        json!({"model":"m","messages":[],"max_tokens":101}),
        json!({"model":"m","messages":[{"content":[{"type":"image_url","image_url":{"url":"https://example.invalid/a"}}]}],"max_tokens":10}),
    ] {
        let call = codec::parse(
            Surface::Chat,
            Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .unwrap();
        assert!(codec::bind(&call, &model(Surface::Chat), &provider(Surface::Chat)).is_err());
    }
    let mut m = model(Surface::Chat);
    m["catalog_capabilities"]["tools"]["function_calling"] = json!(false);
    let call = codec::parse(
        Surface::Chat,
        Bytes::from_static(
            br#"{"model":"m","messages":[],"max_tokens":10,"tools":[{"type":"future_tool"}]}"#,
        ),
    )
    .unwrap();
    assert!(codec::bind(&call, &m, &provider(Surface::Chat)).is_err());
}
#[test]
fn official_sdk_bearer_keys_and_anthropic_keys_use_the_same_canonical_authority() {
    let name = HeaderName::from_static("x-api-key");
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer platform-key".parse().unwrap());
    let selected = codec::credentials(&headers, &name).unwrap();
    assert!(!selected.contains_key("authorization"));
    assert_eq!(selected["x-api-key"], "platform-key");
    headers.insert(
        "authorization",
        "Bearer header.payload.bad-signature".parse().unwrap(),
    );
    let selected = codec::credentials(&headers, &name).unwrap();
    assert!(selected.contains_key("authorization"));
    assert!(!selected.contains_key("x-api-key"));
    headers.insert("x-api-key", "other-key".parse().unwrap());
    assert!(codec::credentials(&headers, &name).is_err());
    headers.remove("authorization");
    headers.append("x-api-key", "duplicate".parse().unwrap());
    assert!(codec::credentials(&headers, &name).is_err());
    let mut standard = HeaderMap::new();
    standard.insert("x-api-key", "anthropic-sdk-key".parse().unwrap());
    let selected =
        codec::credentials(&standard, &HeaderName::from_static("x-platform-key")).unwrap();
    assert_eq!(selected["x-platform-key"], "anthropic-sdk-key");
    assert!(!selected.contains_key("x-api-key"));
}
#[test]
fn openai_stream_requires_all_choices_done_and_real_terminal_usage() {
    let mut meter = meter::StreamMeter::new(Surface::Chat, "p".into(), 1);
    assert!(
        meter
            .push(b"data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\r\n\r\n")
            .unwrap()
            .is_none()
    );
    assert!(meter.push(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4}}\n\n").unwrap().is_none());
    assert!(meter.push(b"data: [DO").unwrap().is_none());
    let usage = meter.push(b"NE]\n\n").unwrap().unwrap();
    assert_eq!((usage.input, usage.output), (3, 4));
    for body in [
        b"data: [DONE]\n\n".as_slice(),
        b"data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    ] {
        let mut meter = meter::StreamMeter::new(Surface::Chat, "p".into(), 1);
        assert!(meter.push(body).is_err());
    }
}
#[test]
fn anthropic_cumulative_usage_replaces_counts_and_partitions_cache_once() {
    let mut meter = meter::StreamMeter::new(Surface::Messages, "p".into(), 1);
    let start=b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"output_tokens\":0,\"cache_read_input_tokens\":80,\"cache_creation_input_tokens\":10}}}\n\n";
    meter.push(start).unwrap();
    meter.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":null},\"usage\":{\"output_tokens\":3}}\n\n").unwrap();
    meter.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n").unwrap();
    let usage = meter
        .push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")
        .unwrap()
        .unwrap();
    assert_eq!((usage.input, usage.output), (110, 5));
    assert_eq!(
        usage.cache.partition("p", usage.input).unwrap(),
        (20, 80, 10)
    );
    let mut missing = meter::StreamMeter::new(Surface::Messages, "p".into(), 1);
    missing.push(start).unwrap();
    assert!(
        missing
            .push(
                b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n"
            )
            .is_err()
    );
}
#[test]
fn missing_negative_and_interrupted_usage_never_becomes_zero_or_success() {
    for body in [br#"{"choices":[{"finish_reason":"stop"}]}"#.as_slice(),br#"{"choices":[{"finish_reason":"stop"}],"usage":{"prompt_tokens":-1,"completion_tokens":0}}"#]{assert!(meter::non_stream(Surface::Chat,"p",body).is_none());}
    assert_eq!(
        meter::non_stream(
            Surface::Embeddings,
            "p",
            br#"{"data":[],"usage":{"prompt_tokens":7,"total_tokens":7}}"#
        )
        .unwrap()
        .output,
        0
    );
    assert!(
        meter::non_stream(
            Surface::Embeddings,
            "p",
            br#"{"data":[],"usage":{"total_tokens":7}}"#
        )
        .is_none()
    );
    let mut meter = meter::StreamMeter::new(Surface::Chat, "p".into(), 1);
    assert!(
        meter
            .push(b"data: {\"choices\":[{\"index\":0,\"finish_reason\":null}]}\n\n")
            .unwrap()
            .is_none()
    );
}

#[test]
fn server_alias_bindings_are_tenant_bound_and_require_verified_authority() {
    fn binding(tenant: &str, alias: &str) -> LlmRouteBinding {
        LlmRouteBinding {
            tenant_id: tenant.into(),
            alias: alias.into(),
            policy_id: "chat-policy".into(),
            policy_pin: None,
            seed: "eval-v1".into(),
        }
    }
    assert!(LlmRoutingBindings::new(vec![binding("t", "p/model")]).is_err());
    assert!(LlmRoutingBindings::new(vec![binding("t", "alias"), binding("t", "alias")]).is_err());
    let bindings =
        LlmRoutingBindings::new(vec![binding("t", "alias"), binding("other", "alias")]).unwrap();
    let a = actor();
    assert!(matches!(
        bindings.for_actor(&a, "alias"),
        Err(LlmError { status: 503, .. })
    ));
    assert!(bindings.for_actor(&a, "p/model").unwrap().is_none());
    let foreign = LlmRoutingBindings::new(vec![binding("other", "alias")]).unwrap();
    assert!(foreign.for_actor(&a, "alias").unwrap().is_none());
    assert!(
        LlmRoutingBindings::default()
            .for_actor(&a, "model")
            .unwrap()
            .is_none()
    );
}
