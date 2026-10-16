//! Socket-free checks of the exact production request constructor and v2 actor verifier.
use super::*;
use crate::migration_knowledge_query::auth::ReplayStore;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SECRET: &str = "synthetic-hybrid-contract-only";
const TASK: &str = "4fd4c558-18f1-4b33-9314-675cf4907a01";

fn auth(reverse: bool) -> GatewayAuth {
    let mut auth = GatewayAuth::new(
        SECRET.into(),
        BTreeMap::new(),
        "v2".into(),
        ReplayStore::memory(),
    )
    .unwrap();
    if reverse {
        auth.caller_service = "knowledge-service".into();
        auth.audience = "gateway".into();
        auth.allowed_path_prefixes =
            vec![crate::migration_knowledge_query::actor::ACTOR_PATH.into()];
    }
    auth
}

#[tokio::test]
async fn native_manual_sync_requests_bind_exact_target_body_and_actor() {
    let client = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let origin = reqwest::Url::parse("https://knowledge.example.invalid/").unwrap();
    let user = ContentIdentity {
        tenant_id: "tenant".into(),
        user_id: "owner".into(),
        is_authenticated: true,
        roles: vec!["user".into()],
        permissions: vec![],
        tier: "normal".into(),
    };
    let mut incoming = HeaderMap::new();
    for (name, value) in [
        ("authorization", "Bearer synthetic-untrusted"),
        ("cookie", "synthetic-untrusted"),
        ("x-api-key", "synthetic-untrusted"),
        ("x-custom-key", "synthetic-untrusted"),
        ("x-user-id", "attacker"),
        ("x-tenant-id", "other"),
        ("x-user-roles", "admin"),
        ("x-app-user-id", "attacker"),
        ("x-user-email", "spoof@example.invalid"),
        ("x-gateway-secret", "synthetic-untrusted"),
        ("content-type", "application/json"),
    ] {
        incoming.insert(name, HeaderValue::from_static(value));
    }
    let cases = [
        (Method::GET, "dataset/sources".to_owned(), String::new()),
        (Method::POST, "dataset/sources/confluence/first/sync".to_owned(), format!(r#"{{"request_id":"{TASK}"}}"#)),
        (Method::GET, format!("dataset/sources/confluence/first/sync/{TASK}"), String::new()),
        (Method::PATCH, "dataset/sources/confluence/connections/connection/control".to_owned(),
            r#"{"expected_revision":"2026-10-12T00:00:00+00:00","expected_status":"active","status":"disabled"}"#.to_owned()),
        (Method::GET, "dataset/sources/confluence/connections/connection/control".to_owned(), String::new()),
        (Method::POST, "dataset/sources/confluence/connections/connection/credentials".to_owned(),
            r#"{"expected_revision":"2026-10-12T00:00:00+00:00","expected_status":"active","email":"replacement@example.invalid","api_token":"synthetic-replacement-only"}"#.to_owned()),
    ];
    let mut exported = vec![];
    for (method, suffix, body) in cases {
        assert_eq!(
            super::super::read_operation(&method, &suffix),
            method == Method::GET
        );
        let path = public_upstream_path(&suffix);
        let request = signed_request(
            &client,
            &origin,
            &auth(false),
            "x-custom-key",
            method.clone(),
            &path,
            "",
            trusted_headers(&user, &incoming).unwrap(),
            body.as_bytes().to_vec(),
        )
        .unwrap();
        assert_eq!(request.url().path(), path);
        assert_eq!(request.method(), method);
        assert_eq!(request.body().unwrap().as_bytes().unwrap(), body.as_bytes());
        for name in [
            "authorization",
            "cookie",
            "x-api-key",
            "x-custom-key",
            "x-app-user-id",
            "x-user-email",
        ] {
            assert!(!request.headers().contains_key(name), "{name}");
        }
        assert_eq!(request.headers()["x-user-id"], "owner");
        assert_eq!(request.headers()["x-tenant-id"], "tenant");
        assert_eq!(request.headers()["x-user-roles"], "user");
        let signature = request.headers()["x-gateway-secret"].to_str().unwrap();
        let verifier = auth(false);
        verifier
            .verify(
                signature,
                method.as_str(),
                &path,
                "",
                body.as_bytes(),
                request.headers(),
            )
            .await
            .unwrap();
        assert!(
            verifier
                .verify(
                    signature,
                    method.as_str(),
                    &path,
                    "",
                    body.as_bytes(),
                    request.headers()
                )
                .await
                .is_err()
        );
        let headers: BTreeMap<_, _> = request
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str(), v.to_str().unwrap()))
            .collect();
        exported.push(
            json!({"method":method.as_str(),"path":path,"query":"","body":body,"headers":headers}),
        );
    }
    if let Ok(path) = std::env::var("KNOWLEDGE_HYBRID_CONTRACT_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&exported).unwrap()).unwrap();
    }
}

#[tokio::test]
async fn python_current_actor_request_is_accepted_by_native_verifier() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../../../tests/fixtures/knowledge_hybrid_manual_sync.json"
    ))
    .unwrap();
    let request = &fixture["python_actor_request"];
    let mut headers = HeaderMap::new();
    for (k, v) in request["headers"].as_object().unwrap() {
        headers.insert(
            HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v.as_str().unwrap()).unwrap(),
        );
    }
    let signature = headers["x-gateway-secret"].to_str().unwrap();
    let mut verifier = auth(true);
    // Frozen synthetic interoperability fixture only; production keeps 60-second skew.
    verifier.max_skew_ms = u64::MAX;
    let path = request["path"].as_str().unwrap();
    assert_eq!(path, crate::migration_knowledge_query::actor::ACTOR_PATH);
    let body = request["body"].as_str().unwrap().as_bytes();
    assert_eq!(
        serde_json::from_slice::<Value>(body).unwrap(),
        json!({"tenant_id":"tenant","user_id":"owner"})
    );
    verifier
        .verify(signature, "POST", path, "", body, &headers)
        .await
        .unwrap();
    assert!(
        verifier
            .verify(signature, "POST", path, "", body, &headers)
            .await
            .is_err()
    );
    let mut changed = auth(true);
    changed.max_skew_ms = u64::MAX;
    assert!(
        changed
            .verify(
                signature,
                "POST",
                path,
                "",
                br#"{"tenant_id":"other","user_id":"owner"}"#,
                &headers
            )
            .await
            .is_err()
    );
    let mut wrong_direction = auth(false);
    wrong_direction.max_skew_ms = u64::MAX;
    assert!(
        wrong_direction
            .verify(signature, "POST", path, "", body, &headers)
            .await
            .is_err()
    );
}
