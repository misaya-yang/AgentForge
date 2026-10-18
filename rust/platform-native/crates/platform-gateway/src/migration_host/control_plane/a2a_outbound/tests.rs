use super::*;
fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../../tests/fixtures/protocols/a2a-task-authority-v1.json"
    ))
    .unwrap()
}
fn config() -> OutboundAgent {
    let f = fixture();
    OutboundAgent {
        name: "fixture-peer".into(),
        endpoint: "https://peer.example/a2a/message:send".into(),
        address: "8.8.8.8".parse().unwrap(),
        bearer_token: "fixture-peer-bearer-never-user-token".into(),
        agent_card_key_id: "wire-test-k1".into(),
        agent_card_public_key: f["public_key_base64url"].as_str().unwrap().into(),
        max_calls_per_day: 1,
        max_request_bytes: 1024 * 1024,
        max_response_bytes: 1024 * 1024,
        timeout_seconds: 120,
    }
}
fn actor() -> Actor {
    Actor {
        tenant_id: "tenant-fixture".into(),
        user_id: "user-fixture".into(),
        roles: vec![],
        tier: "normal".into(),
    }
}
#[test]
fn actual_egress_builder_matches_the_independently_signed_node_fixture_byte_for_byte() {
    let f = fixture();
    let config = config();
    let actor = actor();
    let remote = external_message_id(
        &actor,
        &config.name,
        f["original_message_id"].as_str().unwrap(),
    );
    assert_eq!(remote, f["claims"]["message_id"]);
    let body: Value = serde_json::from_str(f["raw_body"].as_str().unwrap()).unwrap();
    let request = prepare(
        &config,
        &client(&config).unwrap(),
        &SigningKey::from_bytes(&[19; 32]),
        "wire-test-k1",
        "https://gateway.example/",
        &actor,
        &remote,
        reqwest::Method::POST,
        config.endpoint.parse().unwrap(),
        Some(body),
        1800000000,
        "00000000-0000-4000-8000-000000000002",
    )
    .unwrap()
    .build()
    .unwrap();
    assert_eq!(
        request.headers()["x-agentforge-a2a-task"].to_str().unwrap(),
        f["proof"].as_str().unwrap()
    );
    assert_eq!(
        request.body().unwrap().as_bytes().unwrap(),
        f["raw_body"].as_str().unwrap().as_bytes()
    );
    assert_eq!(
        request.headers()["authorization"],
        "Bearer fixture-peer-bearer-never-user-token"
    );
    let proof = request.headers()["x-agentforge-a2a-task"].to_str().unwrap();
    let parts = proof.split('.').collect::<Vec<_>>();
    let signature = Signature::from_slice(&B64.decode(parts[2]).unwrap()).unwrap();
    SigningKey::from_bytes(&[19; 32])
        .verifying_key()
        .verify_strict(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .unwrap();
}
#[test]
fn get_cancel_and_reconcile_have_distinct_signed_paths_empty_get_hash_and_no_query_escape() {
    let config = config();
    let key = SigningKey::from_bytes(&[19; 32]);
    let actor = actor();
    let message = "00000000-0000-4000-8000-000000000001";
    let get = operation_url(&config, message, false).unwrap();
    let cancel = operation_url(&config, message, true).unwrap();
    let reconciliation = reconcile_url(&config, message).unwrap();
    assert_eq!(
        reconciliation.path(),
        format!("/a2a/tasks:byMessage/{message}")
    );
    assert_eq!(reconciliation.origin(), get.origin());
    assert_eq!(get.path(), format!("/a2a/tasks/{message}"));
    assert_eq!(cancel.path(), format!("/a2a/tasks/{message}:cancel"));
    let p = proof(
        &key,
        "wire-test-k1",
        "https://gateway.example",
        &config,
        &actor,
        message,
        "GET",
        &get,
        &[],
        1,
        "00000000-0000-4000-8000-000000000002",
    )
    .unwrap();
    let c: Value =
        serde_json::from_slice(&B64.decode(p.split('.').nth(1).unwrap()).unwrap()).unwrap();
    assert_eq!(c["body_sha256"], format!("{:x}", Sha256::digest(b"")));
    assert_eq!(c["path"], get.path());
    let post = proof(
        &key,
        "wire-test-k1",
        "https://gateway.example",
        &config,
        &actor,
        message,
        "POST",
        &cancel,
        b"{}",
        1,
        "00000000-0000-4000-8000-000000000002",
    )
    .unwrap();
    assert_ne!(p, post);
    let mut query = get.clone();
    query.set_query(Some("owner=other"));
    assert!(
        proof(
            &key,
            "wire-test-k1",
            "https://gateway.example",
            &config,
            &actor,
            message,
            "GET",
            &query,
            &[],
            1,
            "00000000-0000-4000-8000-000000000002"
        )
        .is_err()
    );
    assert!(operation_url(&config, "../another-task", false).is_err());
    let foreign = "https://other.example/a2a/tasks/x".parse().unwrap();
    assert!(
        proof(
            &key,
            "wire-test-k1",
            "https://gateway.example",
            &config,
            &actor,
            message,
            "GET",
            &foreign,
            &[],
            1,
            "00000000-0000-4000-8000-000000000002"
        )
        .is_err()
    );
}
fn card_value() -> Value {
    json!({"name":"fixture-peer","description":"fixture peer","version":"1","supportedInterfaces":[{"url":"https://peer.example/a2a","protocolBinding":"HTTP+JSON","protocolVersion":"1.0"}],"defaultInputModes":["text/plain"],"defaultOutputModes":["text/plain"],"skills":[{"id":"text","name":"text","description":"fixture","tags":["test"]}],"capabilities":{"extensions":[{"uri":RECONCILE,"required":false},{"uri":AUTHORITY,"required":true}]}})
}
#[test]
fn agent_card_uses_explicit_key_pin_and_signature_covers_interface_and_extensions() {
    let config = config();
    let key = SigningKey::from_bytes(&[19; 32]);
    let valid = sign_card(card_value(), "wire-test-k1", &key).unwrap();
    assert!(verify_card(&config, &valid).unwrap().reconcile);
    let mut tampered = valid.clone();
    tampered["supportedInterfaces"][0]["url"] = json!("https://evil.example/a2a");
    assert!(verify_card(&config, &tampered).is_err());
    let attacker = sign_card(
        card_value(),
        "wire-test-k1",
        &SigningKey::from_bytes(&[20; 32]),
    )
    .unwrap();
    assert!(verify_card(&config, &attacker).is_err());
    let mut wrong_interface = card_value();
    wrong_interface["supportedInterfaces"][0]["url"] = json!("https://peer.example/another");
    assert!(
        verify_card(
            &config,
            &sign_card(wrong_interface, "wire-test-k1", &key).unwrap()
        )
        .is_err()
    );
    let mut required = card_value();
    required["capabilities"]["extensions"] = json!([{"uri":"unknown","required":true}]);
    assert!(verify_card(&config, &sign_card(required, "wire-test-k1", &key).unwrap()).is_err());
    assert!(
        verify_card(
            &config,
            &sign_card(card_value(), "other-key-id", &key).unwrap()
        )
        .is_err()
    );
}
#[test]
fn terminal_receipts_are_monotonic_and_ambiguous_cancellation_cannot_be_transmitted_again() {
    let task = |state: &str| json!({"task":{"id":"00000000-0000-4000-8000-000000000001","status":{"state":state}}});
    let completed = task("TASK_STATE_COMPLETED");
    assert_eq!(
        monotonic_response(&completed, &task("TASK_STATE_WORKING")),
        completed
    );
    let canceled = task("TASK_STATE_CANCELED");
    assert_eq!(
        monotonic_response(&canceled, &task("TASK_STATE_COMPLETED")),
        canceled
    );
    let pending = task("TASK_STATE_WORKING");
    assert_eq!(monotonic_response(&pending, &canceled), canceled);
    for state in ["unknown", "rejected"] {
        assert!(cancel_admission(&json!({"cancel_state":state,"response":pending})).is_err());
    }
    assert_eq!(
        cancel_admission(&json!({"cancel_state":"none","response":pending})).unwrap(),
        CancelAdmission::Transmit
    );
    assert_eq!(
        cancel_admission(&json!({"cancel_state":"confirmed","response":canceled})).unwrap(),
        CancelAdmission::Confirmed
    );
    assert!(cancel_admission(&json!({"cancel_state":"none","response":completed})).is_err());
}
#[test]
fn remote_task_ids_and_state_must_be_verified_before_a_durable_confirmation() {
    assert!(
        task_id(&json!({"task":{"id":"../other","status":{"state":"TASK_STATE_COMPLETED"}}}))
            .is_err()
    );
    assert!(task_id(&json!({"task":{"id":"00000000-0000-4000-8000-000000000001","status":{"state":"completed"}}})).is_err());
    assert!(task_id(&json!({"ok":true})).is_err());
    assert!(task_id(&json!({"message":{"role":"ROLE_AGENT","messageId":"00000000-0000-4000-8000-000000000001","parts":[{"text":"x"}]}})).unwrap().is_none());
}
