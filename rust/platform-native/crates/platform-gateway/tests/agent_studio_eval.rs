//! Source matrices: test_agent_publish_gate.py and test_agent_version_candidate.py.
use ai_platform_gateway::migration_agent::{release_contract as r, spec};
use serde_json::{Value, json};
fn policy() -> Value {
    json!({"attachments":false,"high_risk_tools":false,"allowed_origins":[]})
}
fn resolution() -> Value {
    json!({"agent":{"tenant_id":"tenant-a","agent_id":"11111111-1111-4111-8111-111111111111"},"draft":{"draft_id":"22222222-2222-4222-8222-222222222222","revision":7,"spec_hash":"a".repeat(64)}})
}
fn hash(v: Value) -> String {
    format!("sha256:{}", spec::hash(&v))
}
fn snapshot() -> Value {
    json!({"schema_version":"agent-runtime/v1","tenant_id":"tenant-a","agent_id":"11111111-1111-4111-8111-111111111111","agent_version_id":null,"publication":{"id":null,"channel":"hosted","auth_mode":"private"},"model":{"id":"qwen3.7-plus","provider":"dashscope","parameters":{"temperature":0.2}},"instructions":{"agent":"Never copy this prompt into release evidence.","prompt_hash":hash(json!("Never copy this prompt into release evidence."))},"capabilities":[],"knowledge":{"datasets":[],"retrieval":{}},"memory":{"mode":"session"},"channel_policy":policy(),"fingerprints":{"spec":format!("sha256:{}","a".repeat(64)),"tool_schema":hash(json!([])),"skills":hash(json!([])),"knowledge_revision":hash(json!({}))}})
}
fn authorization(access: &str, updated: &str) -> Value {
    r::model_evidence(
        "database",
        &json!({"model_id":"qwen3.7-plus","provider_id":"dashscope","access_level":access,"is_enabled":true,"updated_at":updated}),
        &json!({"is_enabled":true,"updated_at":"2026-07-19T08:00:00+00:00"}),
        true,
    )
}
fn candidate() -> Value {
    r::candidate(
        &resolution(),
        &snapshot(),
        "hosted",
        "private",
        &policy(),
        None,
        &authorization("public", "2026-07-19T08:00:00+00:00"),
    )
    .unwrap()
}
#[test]
fn candidate_is_deterministic_and_hash_only() {
    let c = candidate();
    assert_eq!(c, candidate());
    assert_eq!(c["draft_revision"], 7);
    assert_eq!(c["spec_hash"], "a".repeat(64));
    for k in ["runtime_fingerprint_hash", "release_identity_hash"] {
        assert_eq!(c[k].as_str().unwrap().len(), 64);
    }
    assert!(!c.to_string().contains("Never copy"));
    assert!(
        c["runtime_fingerprint"]["prompt_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
}
#[test]
fn channel_policy_changes_gate_not_version_identity() {
    let a = candidate();
    let mut s = snapshot();
    s["channel_policy"]["attachments"] = json!(true);
    let b = r::candidate(
        &resolution(),
        &s,
        "hosted",
        "private",
        &s["channel_policy"],
        None,
        &authorization("public", "2026-07-19T08:00:00+00:00"),
    )
    .unwrap();
    assert_ne!(a["channel_policy_hash"], b["channel_policy_hash"]);
    assert_ne!(a["runtime_fingerprint_hash"], b["runtime_fingerprint_hash"]);
    assert_eq!(a["release_identity_hash"], b["release_identity_hash"]);
}
#[test]
fn public_expiry_is_normalized_and_bound() {
    let mut s = snapshot();
    s["publication"]["auth_mode"] = json!("public");
    let mut results = vec![];
    for expiry in [
        "2027-01-01T05:00:00+05:00",
        "2027-01-01T00:00:00Z",
        "2027-01-02T00:00:00Z",
    ] {
        s["channel_policy"]["expires_at"] = json!(expiry);
        results.push(
            r::candidate(
                &resolution(),
                &s,
                "hosted",
                "public",
                &s["channel_policy"],
                None,
                &authorization("public", "2026-07-19T08:00:00+00:00"),
            )
            .unwrap(),
        );
    }
    assert_eq!(
        results[0]["channel_policy_hash"],
        results[1]["channel_policy_hash"]
    );
    assert_eq!(
        results[0]["channel_policy"]["expires_at"],
        "2027-01-01T00:00:00+00:00"
    );
    assert_ne!(
        results[0]["runtime_fingerprint_hash"],
        results[2]["runtime_fingerprint_hash"]
    );
}
#[test]
fn dataset_and_model_authorization_bind_release_identity() {
    let dataset = json!({"dataset_id":"33333333-3333-4333-8333-333333333333","tenant_id":"tenant-a","version":"release-v3","manifest_hash":"7".repeat(64)});
    let build = |d: &Value, a: &Value| {
        r::candidate(
            &resolution(),
            &snapshot(),
            "hosted",
            "private",
            &policy(),
            Some(d),
            a,
        )
        .unwrap()
    };
    let auth = authorization("public", "2026-07-19T08:00:00+00:00");
    let first = build(&dataset, &auth);
    let mut changed = dataset.clone();
    changed["manifest_hash"] = json!("8".repeat(64));
    let second = build(&changed, &auth);
    let third = build(
        &dataset,
        &authorization("premium", "2026-07-19T09:00:00+00:00"),
    );
    assert_eq!(first["dataset_version"], "release-v3");
    assert_eq!(first["dataset_manifest_hash"], "7".repeat(64));
    for key in [
        "runtime_fingerprint_hash",
        "release_identity_hash",
        "evaluation_identity_hash",
    ] {
        assert_ne!(first[key], second[key]);
    }
    for key in ["runtime_fingerprint_hash", "release_identity_hash"] {
        assert_ne!(first[key], third[key]);
    }
}
#[test]
fn selected_dataset_requires_matching_tenant() {
    let d = json!({"dataset_id":"33333333-3333-4333-8333-333333333333","tenant_id":"tenant-b","version":"v1","manifest_hash":"7".repeat(64)});
    assert!(
        r::candidate(
            &resolution(),
            &snapshot(),
            "hosted",
            "private",
            &policy(),
            Some(&d),
            &authorization("public", "")
        )
        .unwrap_err()
        .to_string()
        .contains("DATASET_TENANT_MISMATCH")
    );
}
#[test]
fn rejects_all_detached_runtime_dimensions() {
    for (path, value, code) in [
        (
            "/fingerprints/spec",
            json!(format!("sha256:{}", "b".repeat(64))),
            "SPEC_HASH",
        ),
        (
            "/agent_id",
            json!("33333333-3333-4333-8333-333333333333"),
            "AGENT_ID",
        ),
        ("/tenant_id", json!("tenant-b"), "TENANT_ID"),
        ("/publication/channel", json!("api"), "CHANNEL_MISMATCH"),
        ("/publication/auth_mode", json!("token"), "AUTH_MODE"),
        (
            "/schema_version",
            json!("agent-runtime/v0"),
            "RUNTIME_VERSION",
        ),
    ] {
        let mut s = snapshot();
        *s.pointer_mut(path).unwrap() = value;
        assert!(
            r::candidate(
                &resolution(),
                &s,
                "hosted",
                "private",
                &s["channel_policy"],
                None,
                &authorization("public", "")
            )
            .unwrap_err()
            .to_string()
            .contains(code),
            "{code}"
        );
    }
}
#[test]
fn capability_omission_and_high_risk_policy() {
    for (kind, id, risk, allowed) in [
        ("native", "web_fetch", "low", false),
        ("mcp", "write_ticket", "high", true),
    ] {
        let mut resolution = resolution();
        resolution["capabilities"] = json!([{"capability_type":kind,"resource_id":id,"resource_version":"1","schema_hash":"b".repeat(64),"config":{"risk":risk}}]);
        let result = r::candidate(
            &resolution,
            &snapshot(),
            "hosted",
            "private",
            &policy(),
            None,
            &authorization("public", ""),
        );
        if allowed {
            assert_eq!(result.unwrap()["channel"], "hosted");
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("CAPABILITY_SET_MISMATCH")
            );
        }
    }
}
#[test]
fn structured_diff_never_returns_prompt_body() {
    let before = json!({"instructions":"old secret-shaped prompt body","model":{"model_id":"old","provider_id":"dashscope"},"capabilities":[],"knowledge":[]});
    let after = json!({"instructions":"new secret-shaped prompt body","model":{"model_id":"new","provider_id":"dashscope"},"capabilities":[{"type":"native","resource_id":"web_fetch","config":{"risk":"low"}}],"knowledge":[]});
    let diff = r::structured_diff(&before, &after);
    for section in ["prompt", "model", "capabilities"] {
        assert!(
            diff["changed_sections"]
                .as_array()
                .unwrap()
                .contains(&json!(section))
        );
    }
    assert_eq!(
        diff["sections"]["prompt"]["before_length"],
        before["instructions"].as_str().unwrap().len()
    );
    assert!(!diff.to_string().contains("secret-shaped"));
}
#[test]
fn offline_profile_truthfully_tests_integrity_only() {
    let g = r::evaluate(&candidate(), &r::profile(""), 0.).unwrap();
    assert_eq!(g["status"], "passed");
    assert_eq!(g["execution_scope"], "provider_free_release_integrity");
    assert_eq!(g["model_quality_evaluated"], false);
    assert_eq!(g["metrics"]["critical_pass_rate"], 1.0);
    assert_eq!(g["metrics"]["provider_cost_cents"], 0.0);
    assert!(
        g["non_blocking_findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["code"] == "AGENT_EVAL_DATASET_NOT_SELECTED")
    );
}
#[test]
fn fingerprint_tampering_and_missing_dimension_block() {
    let mut c = candidate();
    c["runtime_fingerprint"]["model_id"] = json!("forged");
    let g = r::evaluate(&c, &r::profile(""), 0.).unwrap();
    assert_eq!(g["status"], "failed");
    assert_eq!(g["metrics"]["critical_pass_rate"], 0.);
    assert_eq!(
        g["blocking_findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["AGENT_EVAL_FINGERPRINT_HASH_MISMATCH"]
    );
    let mut c = candidate();
    c["runtime_fingerprint"]["knowledge_revision"] = json!("");
    c["runtime_fingerprint_hash"] = json!(spec::hash(&c["runtime_fingerprint"]));
    let g = r::evaluate(&c, &r::profile(""), 0.).unwrap();
    assert_eq!(g["status"], "failed");
    assert_eq!(
        g["blocking_findings"][0]["field"],
        "runtime_fingerprint.knowledge_revision"
    );
}
#[test]
fn unavailable_production_profile_fails_closed() {
    let p = r::profile("production_v1");
    assert_eq!(p["available"], false);
    assert!(
        r::evaluate(&candidate(), &p, 0.)
            .unwrap_err()
            .to_string()
            .contains("AGENT_RELEASE_PROFILE_UNAVAILABLE")
    );
}
