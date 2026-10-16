//! Source signed-envelope integrity/replay matrix. All secrets are synthetic test constants.
use ai_platform_gateway::migration_agent::runtime_envelope::*;
use serde_json::{Value, json};
use std::sync::Arc;
fn snapshot() -> Value {
    json!({"schema_version":"agent-runtime/v1","tenant_id":"tenant-a","agent_id":"agent-a","agent_version_id":"version-a","publication":{"id":"publication-a","channel":"api","auth_mode":"tenant"},"model":{"id":"qwen3.7-plus","provider":"dashscope","parameters":{"temperature":0.3,"max_tokens":2048}},"instructions":{"agent":"Approved instructions.","prompt_hash":"sha256:prompt"},"capabilities":[{"type":"platform","id":"search_knowledge_base","version":null,"schema_hash":"sha256:tool-schema","risk":"low","config":{}}],"knowledge":{"datasets":["dataset-a"],"retrieval":{"mode":"auto","top_k":5,"threshold":0.4}},"memory":{"mode":"session"},"channel_policy":{"attachments":true,"high_risk_tools":false,"allowed_origins":[]},"fingerprints":{"spec":"sha256:spec","tool_schema":"sha256:tools","skills":"sha256:skills","knowledge_revision":"sha256:knowledge"}})
}
fn identity() -> Value {
    json!({"tenant_id":"tenant-a","caller_principal":"user-a","agent_id":"agent-a","agent_version_id":"version-a","draft_revision":null,"publication_id":"publication-a","channel":"api","session_id":"session-a","spec_hash":"sha256:spec"})
}
fn signer() -> AgentRuntimeSigner {
    AgentRuntimeSigner::new(
        "synthetic-test-signing-secret".into(),
        "ai-gateway".into(),
        None,
        60000,
        5000,
    )
    .unwrap()
}
fn signed(signer: &AgentRuntimeSigner) -> Value {
    signer
        .sign(
            &identity(),
            &snapshot(),
            &json!({"message":"hello"}),
            Some(100000),
            Some(130000),
            Some("fresh-test-nonce"),
        )
        .unwrap()
}
#[test]
fn canonical_json_is_order_independent() {
    let a: Value = serde_json::from_str(r#"{"b":2,"a":{"z":1,"x":"中文"}}"#).unwrap();
    let b: Value = serde_json::from_str(r#"{"a":{"x":"中文","z":1},"b":2}"#).unwrap();
    assert_eq!(canonical_runtime_json(&a), canonical_runtime_json(&b));
    assert_eq!(runtime_sha256(&a), runtime_sha256(&b));
}
#[test]
fn verification_recalculates_hashes_and_consumes_nonce_once() {
    let s = signer();
    let e = signed(&s);
    let context = s
        .verify(
            &e,
            &json!({"message":"hello"}),
            "tenant-a",
            "user-a",
            "session-a",
            Some(100001),
        )
        .unwrap();
    assert_eq!(context.agent_id, "agent-a");
    assert!(context.capability_ids.contains("search_knowledge_base"));
    assert_eq!(context.dimensions()["agent_spec_hash"], "sha256:spec");
    assert_eq!(
        s.verify(
            &e,
            &json!({"message":"hello"}),
            "tenant-a",
            "user-a",
            "session-a",
            Some(100002)
        )
        .unwrap_err()
        .0,
        "AGENT_RUNTIME_REPLAYED"
    );
}
#[test]
fn snapshot_and_signature_mutation_rejected_before_nonce_consumption() {
    let s = signer();
    let e = signed(&s);
    let mut changed = e.clone();
    changed["resolved_snapshot"]["model"]["id"] = json!("other");
    assert_eq!(
        s.verify(
            &changed,
            &json!({"message":"hello"}),
            "tenant-a",
            "user-a",
            "session-a",
            Some(100001)
        )
        .unwrap_err()
        .0,
        "AGENT_RUNTIME_SNAPSHOT_HASH_MISMATCH"
    );
    let mut changed = e.clone();
    changed["signature"] = json!("sha256:forged");
    assert_eq!(
        s.verify(
            &changed,
            &json!({"message":"hello"}),
            "tenant-a",
            "user-a",
            "session-a",
            Some(100001)
        )
        .unwrap_err()
        .0,
        "AGENT_RUNTIME_SIGNATURE_INVALID"
    );
    assert!(
        s.verify(
            &e,
            &json!({"message":"hello"}),
            "tenant-a",
            "user-a",
            "session-a",
            Some(100001)
        )
        .is_ok()
    );
}
#[test]
fn body_identity_and_expiry_substitution_fail_closed() {
    for (body, tenant, caller, session, time, code) in [
        (
            json!({"message":"substituted"}),
            "tenant-a",
            "user-a",
            "session-a",
            100001,
            "AGENT_RUNTIME_BODY_HASH_MISMATCH",
        ),
        (
            json!({"message":"hello"}),
            "tenant-b",
            "user-a",
            "session-a",
            100001,
            "AGENT_RUNTIME_TENANT_MISMATCH",
        ),
        (
            json!({"message":"hello"}),
            "tenant-a",
            "user-b",
            "session-a",
            100001,
            "AGENT_RUNTIME_CALLER_MISMATCH",
        ),
        (
            json!({"message":"hello"}),
            "tenant-a",
            "user-a",
            "session-b",
            100001,
            "AGENT_RUNTIME_SESSION_MISMATCH",
        ),
        (
            json!({"message":"hello"}),
            "tenant-a",
            "user-a",
            "session-a",
            130001,
            "AGENT_RUNTIME_EXPIRED",
        ),
        (
            json!({"message":"hello"}),
            "tenant-a",
            "user-a",
            "session-a",
            94999,
            "AGENT_RUNTIME_NOT_YET_VALID",
        ),
    ] {
        let s = signer();
        assert_eq!(
            s.verify(&signed(&s), &body, tenant, caller, session, Some(time))
                .unwrap_err()
                .0,
            code
        );
    }
}
struct Unavailable;
impl ReplayStore for Unavailable {
    fn seen_or_record(&self, _: &str, _: i64) -> Result<bool, String> {
        Err("unavailable".into())
    }
}
#[test]
fn unavailable_atomic_nonce_store_is_failure() {
    let s = AgentRuntimeSigner::new(
        "synthetic-test-signing-secret".into(),
        "ai-gateway".into(),
        Some(Arc::new(Unavailable)),
        60000,
        5000,
    )
    .unwrap();
    assert_eq!(
        s.verify(
            &signed(&s),
            &json!({"message":"hello"}),
            "tenant-a",
            "user-a",
            "session-a",
            Some(100001)
        )
        .unwrap_err()
        .0,
        "AGENT_RUNTIME_REPLAY_STORE_UNAVAILABLE"
    );
}
#[test]
fn preview_requires_exactly_one_draft_or_version_pin() {
    let s = signer();
    for (draft, version, publication, valid) in [
        (json!(1), Value::Null, Value::Null, true),
        (Value::Null, json!("version-a"), Value::Null, true),
        (Value::Null, Value::Null, Value::Null, false),
        (json!(1), json!("version-a"), Value::Null, false),
        (json!(1), Value::Null, json!("publication-a"), false),
    ] {
        let mut i = identity();
        i["channel"] = json!("preview");
        i["draft_revision"] = draft;
        i["agent_version_id"] = version.clone();
        i["publication_id"] = publication.clone();
        let mut snapshot = snapshot();
        snapshot["agent_version_id"] = version;
        snapshot["publication"]["id"] = publication;
        snapshot["publication"]["channel"] = json!("preview");
        assert_eq!(
            s.sign(&i, &snapshot, &json!({}), Some(1000), None, Some("nonce"))
                .is_ok(),
            valid
        );
    }
}
#[test]
fn snapshot_rejects_credential_shaped_configuration() {
    for name in [
        "api_key",
        "api-token",
        "Authorization",
        "credential_ref",
        "privateKey",
        "refresh_token",
        "secret_ref",
    ] {
        let mut value = snapshot();
        value["capabilities"][0]["config"][name] = json!("must-not-cross");
        assert_eq!(
            validate_snapshot(&value).unwrap_err().0,
            "AGENT_RUNTIME_SECRET_FORBIDDEN"
        );
    }
}
#[test]
fn signed_identity_must_equal_snapshot_identity() {
    let s = signer();
    let mut i = identity();
    i["agent_id"] = json!("agent-other");
    let e = s
        .sign(&i, &snapshot(), &json!({}), Some(1000), None, Some("nonce"))
        .unwrap();
    assert_eq!(
        s.verify(
            &e,
            &json!({}),
            "tenant-a",
            "user-a",
            "session-a",
            Some(1001)
        )
        .unwrap_err()
        .0,
        "AGENT_RUNTIME_IDENTITY_MISMATCH"
    );
}
